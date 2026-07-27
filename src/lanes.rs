//! Per-target drain lanes (U4): independent spool-to-target pipelines that
//! copy staged entries to their target, verify against the *original
//! source* signature (never a spool re-hash, per R6), and report terminal
//! outcomes back to the spool store for refcounted eviction.
//!
//! Each target gets its own lane: one copy worker and one verify worker,
//! fed by an unbounded queue of small metadata records
//! ([`LaneEntry`]). The queue is deliberately unbounded -- see
//! [`start_target_lanes`] -- so a stalled or slow target can never apply
//! backpressure to staging or to any other lane; the only real
//! backpressure in the staged pipeline is the spool store's `reserve` call
//! (U2), which this module does not touch.
//!
//! Wired into `run_copy`'s staged relay path via `run_copy_staged` in
//! `src/copy.rs` (U5), which feeds staged entries into
//! [`start_target_lanes`]'s lanes and joins them at run end. The
//! module-level `#![allow(dead_code)]` remains for the handful of items
//! still exercised only by this module's own tests (forward-looking
//! surface for later units) -- matching the convention already used in
//! `copy_fast_path.rs`/`stage.rs`.
#![allow(dead_code)]

use std::fs;
use std::io::{self, ErrorKind};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use filetime::{FileTime, set_file_mtime};

use crate::copy::{
    FileSignature, SizeBucket, WorkerEvent, classify_failure, hash_file_xxh3_128, panic_failure,
    temp_path_for,
};
use crate::copy_fast_path::{CopyTransferError, CopyTransferOperation, copy_file_data};
use crate::error::{CopyFailure, CopyOperation};
use crate::progress_model::TransferRowPhase;
use crate::spool::{EntryId, SpoolStore};

/// Global, deterministic worker-id layout for staged mode: a pure function
/// of `(staging_parallel, target_count)` (and, per-accessor, `lane_index`)
/// so U5/U6 can size UI state (e.g. `worker_states`) once for the whole
/// run before dispatching anything, instead of discovering ids ad hoc as
/// workers start.
///
/// Layout: the staging lane occupies worker ids `0..staging_parallel`;
/// each target lane `i` then gets a fixed two-slot block immediately after
/// that: `staging_parallel + i*2` for its copy worker and
/// `staging_parallel + i*2 + 1` for its verifier. Concurrent staging and
/// lane events can therefore never collide on the same row of the
/// existing `WorkerEvent` stream / render loop.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LaneWorkerLayout {
    staging_parallel: usize,
    target_count: usize,
}

impl LaneWorkerLayout {
    pub(crate) fn new(staging_parallel: usize, target_count: usize) -> Self {
        Self {
            staging_parallel,
            target_count,
        }
    }

    /// The worker-id block reserved for the staging lane.
    pub(crate) fn staging_worker_ids(&self) -> Range<usize> {
        0..self.staging_parallel
    }

    /// Worker id for target lane `lane_index`'s copy worker.
    pub(crate) fn copy_worker_id(&self, lane_index: usize) -> usize {
        self.staging_parallel + lane_index * 2
    }

    /// Worker id for target lane `lane_index`'s verify worker.
    pub(crate) fn verify_worker_id(&self, lane_index: usize) -> usize {
        self.staging_parallel + lane_index * 2 + 1
    }

    /// Total worker-id slots occupied across staging plus every target
    /// lane; callers size per-worker UI state to this length for the
    /// whole run.
    pub(crate) fn total_workers(&self) -> usize {
        self.staging_parallel + self.target_count * 2
    }
}

/// One staged file queued for a specific target lane: a small metadata
/// record (no file bytes), which is exactly why the per-target queue
/// holding these can safely be unbounded -- see [`start_target_lanes`].
///
/// `signature` must be the *original source* signature (the integrity
/// anchor carried from `stage::StagedFile`), never a hash re-derived from
/// the spool file, per R6.
#[derive(Debug, Clone)]
pub(crate) struct LaneEntry {
    pub(crate) entry_id: EntryId,
    /// Original source path, kept only for display/failure reporting
    /// (e.g. `relative_file_label`), never read from during this hop.
    pub(crate) source: PathBuf,
    /// Spool file to copy from.
    pub(crate) spool_path: PathBuf,
    /// Resolved final destination on this lane's target.
    pub(crate) dest: PathBuf,
    pub(crate) size: u64,
    pub(crate) signature: FileSignature,
    pub(crate) display_name: String,
}

/// A running per-target drain lane: a copy worker and a verify worker,
/// each pulling from their own unbounded queue, plus the entry point
/// ([`sender`](Self::sender)) callers use to feed staged files destined
/// for this lane's target.
pub(crate) struct TargetLane {
    target_root: PathBuf,
    copy_worker_id: usize,
    verify_worker_id: usize,
    entry_tx: Sender<LaneEntry>,
    copy_handle: thread::JoinHandle<()>,
    verify_handle: thread::JoinHandle<()>,
}

impl TargetLane {
    /// The entry point a caller (U5's staging-completion callback, or a
    /// test) uses to feed a staged file into this lane. The underlying
    /// channel is unbounded, so this never blocks the caller regardless of
    /// how far behind this lane's target is -- see [`start_target_lanes`].
    ///
    /// Returns `Err(entry)` (handing the entry back) if this lane's copy
    /// worker has already exited (e.g. after a panic): the caller is then
    /// responsible for treating `entry` as failed for this target (it was
    /// never handed off, so the spool store still thinks it's pending).
    pub(crate) fn sender(&self) -> &Sender<LaneEntry> {
        &self.entry_tx
    }

    /// Closes this lane's input (so its workers drain and stop once their
    /// queues empty) and joins both worker threads. Mirrors the existing
    /// `execute_phase`/`join_verifier_workers` panic-to-`WorkerEvent`
    /// conversion: a worker thread that panicked reports as one systemic
    /// `WorkerEvent::Error` for this lane's target, attributed via
    /// `failure.dest` so the render loop's existing
    /// `record_target_copy_failure`/`record_target_verify_failure`
    /// bookkeeping keeps working unmodified.
    ///
    /// Spool capacity for entries this lane was still holding at the time
    /// of a panic is *not* released here -- that already happened
    /// (potentially long before this call) via each worker's
    /// [`LaneReleaseGuard`], which fires on the panicking thread's own
    /// unwind.
    pub(crate) fn join(self, event_tx: &Sender<WorkerEvent>) {
        drop(self.entry_tx);
        if self.copy_handle.join().is_err() {
            let mut failure = panic_failure(self.copy_worker_id, CopyOperation::WorkerPanic);
            failure.dest = Some(self.target_root.clone());
            let _ = event_tx.send(WorkerEvent::Error {
                worker: self.copy_worker_id,
                failure,
            });
        }
        if self.verify_handle.join().is_err() {
            let mut failure = panic_failure(self.verify_worker_id, CopyOperation::WorkerPanic);
            failure.dest = Some(self.target_root.clone());
            let _ = event_tx.send(WorkerEvent::Error {
                worker: self.verify_worker_id,
                failure,
            });
        }
    }
}

/// Starts one independent drain lane per target. Each lane's queue is a
/// `crossbeam_channel::unbounded` channel, not a bounded one: staged
/// entries are small metadata records ([`LaneEntry`]), so feeding a
/// target's queue costs nothing, and the real backpressure point is
/// entirely `SpoolStore::reserve` (U2) -- if staging can't reserve
/// capacity for the next file, it naturally slows down. A bounded
/// per-lane channel would reintroduce exactly the slowest-target-blocks-
/// everything bug staged mode exists to fix (a slow lane's full queue
/// would stall whichever thread is trying to push the next entry into
/// it); an unbounded queue keeps a stalled lane structurally unable to
/// block staging or any other lane.
pub(crate) fn start_target_lanes(
    targets: &[PathBuf],
    spool: Arc<SpoolStore>,
    layout: LaneWorkerLayout,
    event_tx: Sender<WorkerEvent>,
) -> Vec<TargetLane> {
    targets
        .iter()
        .enumerate()
        .map(|(target_index, target_root)| {
            start_one_lane(
                target_index,
                target_root.clone(),
                Arc::clone(&spool),
                layout,
                event_tx.clone(),
            )
        })
        .collect()
}

fn start_one_lane(
    target_index: usize,
    target_root: PathBuf,
    spool: Arc<SpoolStore>,
    layout: LaneWorkerLayout,
    event_tx: Sender<WorkerEvent>,
) -> TargetLane {
    let copy_worker_id = layout.copy_worker_id(target_index);
    let verify_worker_id = layout.verify_worker_id(target_index);

    let (entry_tx, entry_rx) = unbounded::<LaneEntry>();
    let (verify_tx, verify_rx) = unbounded::<LaneEntry>();

    let copy_handle = {
        let spool = Arc::clone(&spool);
        let tx = event_tx.clone();
        thread::spawn(move || {
            run_copy_worker(target_index, copy_worker_id, entry_rx, verify_tx, tx, spool);
        })
    };

    let verify_handle = {
        let spool = Arc::clone(&spool);
        let tx = event_tx.clone();
        let target_root = target_root.clone();
        thread::spawn(move || {
            run_verify_worker(
                target_index,
                verify_worker_id,
                target_root,
                verify_rx,
                tx,
                spool,
            );
        })
    };

    TargetLane {
        target_root,
        copy_worker_id,
        verify_worker_id,
        entry_tx,
        copy_handle,
        verify_handle,
    }
}

/// Panic-safety mechanism required by R3/R7, scoped to a single worker's
/// own responsibility -- mirrors `stage.rs`'s `StageReleaseGuard` arm/disarm
/// pattern rather than firing unconditionally on every drop.
///
/// Armed only for the window a worker holds an entry it hasn't yet handed
/// off through the normal path (copy worker: handed to the verify queue;
/// verify worker: `mark_terminal` called after verification completes) and
/// disarmed immediately once that happens, so ordinary successful
/// completion of an entry -- and the eventual normal loop exit once the
/// queue drains/closes -- never triggers this guard's `Drop` logic.
///
/// On a genuine panic while armed, `Drop` releases exactly what this
/// worker itself still owned and nothing more: the one entry it was
/// actively handling (`current_entry`), plus any entries still sitting
/// unconsumed in *this worker's own* queue (via the cloned `rx`) -- since
/// this worker's thread is unwinding, nothing else will ever drain them.
/// It deliberately does not use `mark_all_remaining_terminal_for_target`,
/// which sweeps every entry still pending on the whole target index
/// regardless of which worker (copy or verify) actually holds it. That
/// broader scope was the bug: a copy worker's guard could fire on every
/// normal completion (not just panics) and race ahead of the verify
/// worker's own verification of the same entries, and a verify worker's
/// panic could evict spool files the lane's still-healthy copy worker
/// hasn't read yet. Scoping release to only this worker's own queue avoids
/// both.
struct LaneReleaseGuard {
    spool: Arc<SpoolStore>,
    target_index: usize,
    /// A second handle onto this worker's own inbound queue, used only to
    /// drain and release any entries still sitting there on a panic -- see
    /// the struct docs. Never raced against the worker's own `recv()` loop:
    /// `Drop` only runs after that loop has already stopped consuming
    /// (either normal exit with `armed == false`, a no-op, or mid-panic
    /// unwind on the same thread).
    rx: Receiver<LaneEntry>,
    armed: bool,
    current_entry: Option<EntryId>,
}

impl LaneReleaseGuard {
    fn new(spool: Arc<SpoolStore>, target_index: usize, rx: Receiver<LaneEntry>) -> Self {
        Self {
            spool,
            target_index,
            rx,
            armed: false,
            current_entry: None,
        }
    }

    /// Arms the guard for the window during which `entry_id` is owned by
    /// this worker and hasn't yet been handed off (copy) or driven to a
    /// terminal outcome through the normal path (verify).
    fn arm(&mut self, entry_id: EntryId) {
        self.current_entry = Some(entry_id);
        self.armed = true;
    }

    /// Disarms once the current entry's fate has been settled through the
    /// normal path, so a subsequent panic on a *later* entry -- or the
    /// eventual normal loop exit -- doesn't re-release this one.
    fn disarm(&mut self) {
        self.armed = false;
        self.current_entry = None;
    }
}

impl Drop for LaneReleaseGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(entry_id) = self.current_entry.take() {
            self.spool.mark_terminal(entry_id, self.target_index);
        }
        // This worker's thread is unwinding, so nothing will ever call
        // `recv()` on its queue again -- drain and release whatever is
        // still sitting there rather than leaking that capacity forever.
        while let Ok(entry) = self.rx.try_recv() {
            self.spool.mark_terminal(entry.entry_id, self.target_index);
        }
    }
}

fn run_copy_worker(
    target_index: usize,
    worker_id: usize,
    rx: Receiver<LaneEntry>,
    verify_tx: Sender<LaneEntry>,
    event_tx: Sender<WorkerEvent>,
    spool: Arc<SpoolStore>,
) {
    let mut release_guard = LaneReleaseGuard::new(Arc::clone(&spool), target_index, rx.clone());
    let mut next_transfer_id = 1_usize;

    while let Ok(entry) = rx.recv() {
        release_guard.arm(entry.entry_id);
        let transfer_id = next_transfer_id;
        next_transfer_id += 1;

        let _ = event_tx.send(WorkerEvent::Started {
            worker: worker_id,
            transfer_id,
            bucket: SizeBucket::Small,
            phase: TransferRowPhase::Copying,
            name: entry.display_name.clone(),
            source: entry.source.clone(),
            dest: entry.dest.clone(),
            total: entry.size,
        });

        match copy_lane_file(&entry, worker_id, &event_tx) {
            Ok(bytes) => {
                // Test-only hook, firing right after the target copy is in
                // its final place and before the entry is handed to the
                // verify worker. Used by tests to corrupt the just-copied
                // destination (verify-mismatch scenario), force a panic
                // mid-drain (panic-safety scenario), or stall a specific
                // entry (slow-target isolation scenario). A no-op outside
                // `cfg(test)` and when nothing is armed.
                run_lane_copy_test_hook(&entry);

                // The entry is about to be handed off to the verify queue,
                // so this worker no longer owns it -- disarm before the
                // handoff so a panic on a *later* entry can't re-release
                // this one, and so ordinary successful completion never
                // trips the guard's `Drop` logic.
                release_guard.disarm();

                let _ = event_tx.send(WorkerEvent::Finished {
                    worker: worker_id,
                    bucket: SizeBucket::Small,
                    name: entry.display_name.clone(),
                    source: entry.source.clone(),
                    dest: entry.dest.clone(),
                    bytes,
                });

                if let Err(send_err) = verify_tx.send(entry) {
                    // The verify worker already exited (e.g. it panicked).
                    // The entry never got a terminal outcome recorded by
                    // the verify side in that case, so release it directly
                    // rather than relying on this thread's own release
                    // guard, which was just disarmed above precisely
                    // because the entry was believed handed off.
                    spool.mark_terminal(send_err.0.entry_id, target_index);
                }
            }
            Err(failure) => {
                let _ = event_tx.send(WorkerEvent::Error {
                    worker: worker_id,
                    failure,
                });
                spool.mark_terminal(entry.entry_id, target_index);
                release_guard.disarm();
            }
        }
    }
}

fn run_verify_worker(
    target_index: usize,
    worker_id: usize,
    target_root: PathBuf,
    rx: Receiver<LaneEntry>,
    event_tx: Sender<WorkerEvent>,
    spool: Arc<SpoolStore>,
) {
    let mut release_guard = LaneReleaseGuard::new(Arc::clone(&spool), target_index, rx.clone());
    let mut next_transfer_id = 1_usize;

    while let Ok(entry) = rx.recv() {
        release_guard.arm(entry.entry_id);
        let transfer_id = next_transfer_id;
        next_transfer_id += 1;

        let _ = event_tx.send(WorkerEvent::Started {
            worker: worker_id,
            transfer_id,
            bucket: SizeBucket::Small,
            phase: TransferRowPhase::Verifying,
            name: entry.display_name.clone(),
            source: entry.source.clone(),
            dest: entry.dest.clone(),
            total: entry.size,
        });

        // Test-only hook, firing right after this entry is dequeued for
        // verification and before verification itself runs. A no-op
        // outside `cfg(test)` and when nothing is armed; see
        // `run_lane_copy_test_hook` above for why this is `Fn`/keyed on the
        // entry rather than single-shot.
        run_lane_verify_test_hook(&entry);

        let result = verify_lane_transfer(&entry, |done| {
            let _ = event_tx.send(WorkerEvent::Progress {
                worker: worker_id,
                copied: done,
            });
        });

        match result {
            Ok(()) => {
                let _ = event_tx.send(WorkerEvent::Verified {
                    worker: worker_id,
                    target: target_root.clone(),
                    size: entry.size,
                });
            }
            Err(failure) => {
                let _ = event_tx.send(WorkerEvent::VerificationFailed {
                    worker: worker_id,
                    failure,
                });
            }
        }

        // Terminal either way (verified or failed): decrement this
        // target's refcount on the entry so the spool can evict once every
        // needing target has reported. This is the normal path settling
        // the entry's fate, so disarm the guard right after -- a panic
        // from here on (e.g. on the *next* entry) must not re-release this
        // one.
        spool.mark_terminal(entry.entry_id, target_index);
        release_guard.disarm();
    }
}

/// Copies `entry.spool_path` -> `entry.dest`, reusing the exact
/// temp-file-then-rename-then-mtime discipline `copy_file` uses for the
/// legacy source->target hop, but via the native-preferred fast path
/// (`copy_file_data`) rather than the hashing buffered loop staging uses --
/// this hop doesn't need to compute a hash in-flight, since verification
/// compares against the signature already carried on `entry`.
fn copy_lane_file(
    entry: &LaneEntry,
    worker: usize,
    tx: &Sender<WorkerEvent>,
) -> Result<u64, CopyFailure> {
    if let Some(parent) = entry.dest.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            lane_copy_failure(
                entry,
                CopyOperation::CreateDir,
                err,
                format!("failed to create parent directory: {}", parent.display()),
            )
        })?;
    }

    let temp_dest = temp_path_for(&entry.dest);
    if temp_dest.exists() {
        fs::remove_file(&temp_dest).map_err(|err| {
            lane_copy_failure(
                entry,
                CopyOperation::CleanupTemp,
                err,
                format!("failed to remove stale temp file: {}", temp_dest.display()),
            )
        })?;
    }

    let copy_result = (|| -> Result<u64, CopyFailure> {
        let transfer = copy_file_data(&entry.spool_path, &temp_dest, |copied| {
            let _ = tx.send(WorkerEvent::Progress { worker, copied });
        })
        .map_err(|err| lane_transfer_failure(entry, err, &temp_dest))?;
        let metadata = transfer.metadata;

        fs::set_permissions(&temp_dest, metadata.permissions()).map_err(|err| {
            lane_copy_failure(
                entry,
                CopyOperation::SetPermissions,
                err,
                format!("failed setting permissions on {}", temp_dest.display()),
            )
        })?;

        if let Ok(modified) = metadata.modified() {
            set_file_mtime(&temp_dest, FileTime::from_system_time(modified)).map_err(|err| {
                lane_copy_failure(
                    entry,
                    CopyOperation::SetMtime,
                    err,
                    format!("failed setting mtime on {}", temp_dest.display()),
                )
            })?;
        }

        fs::rename(&temp_dest, &entry.dest).map_err(|err| {
            lane_copy_failure(
                entry,
                CopyOperation::Rename,
                err,
                format!(
                    "failed to move temp file into place: {} -> {}",
                    temp_dest.display(),
                    entry.dest.display()
                ),
            )
        })?;

        Ok(transfer.bytes)
    })();

    if copy_result.is_err() && temp_dest.exists() {
        let _ = fs::remove_file(&temp_dest);
    }

    copy_result
}

/// Verifies `entry.dest` against `entry.signature` -- the *original
/// source* signature, per R6 -- using the same read-back-and-compare
/// approach `verify_completed_transfer` uses for the legacy path (stat,
/// then `hash_file_xxh3_128`, then compare).
fn verify_lane_transfer(entry: &LaneEntry, progress: impl FnMut(u64)) -> Result<(), CopyFailure> {
    let metadata = fs::metadata(&entry.dest).map_err(|err| {
        lane_verify_failure(
            entry,
            err,
            format!(
                "verification failed: could not stat destination {}",
                entry.dest.display()
            ),
        )
    })?;
    let actual = hash_file_xxh3_128(&entry.dest, metadata.len(), progress).map_err(|err| {
        lane_verify_failure(
            entry,
            err,
            format!(
                "verification failed: could not read destination {}",
                entry.dest.display()
            ),
        )
    })?;

    if actual == entry.signature {
        Ok(())
    } else {
        Err(lane_verify_failure(
            entry,
            io::Error::new(ErrorKind::InvalidData, "destination signature mismatch"),
            format!(
                "verification failed: signature mismatch for {}",
                entry.dest.display()
            ),
        ))
    }
}

fn lane_copy_failure(
    entry: &LaneEntry,
    operation: CopyOperation,
    err: io::Error,
    message: String,
) -> CopyFailure {
    let kind = err.kind();
    let raw_os_error = err.raw_os_error();
    CopyFailure {
        source: entry.source.clone(),
        dest: Some(entry.dest.clone()),
        operation,
        kind,
        raw_os_error,
        classification: classify_failure(kind, raw_os_error, operation),
        message,
    }
}

fn lane_verify_failure(entry: &LaneEntry, err: io::Error, message: String) -> CopyFailure {
    lane_copy_failure(entry, CopyOperation::Verify, err, message)
}

fn lane_transfer_failure(
    entry: &LaneEntry,
    error: CopyTransferError,
    temp_dest: &Path,
) -> CopyFailure {
    match error {
        CopyTransferError::UnsupportedNative { reason } => lane_copy_failure(
            entry,
            CopyOperation::Read,
            io::Error::new(
                ErrorKind::Unsupported,
                format!("native copy unsupported: {reason:?}"),
            ),
            format!(
                "failed copying {} -> {}",
                entry.spool_path.display(),
                temp_dest.display()
            ),
        ),
        CopyTransferError::Io { operation, source } => {
            let op = match operation {
                CopyTransferOperation::StatSource | CopyTransferOperation::OpenSource => {
                    CopyOperation::OpenSource
                }
                CopyTransferOperation::CreateDestination => CopyOperation::CreateTemp,
                CopyTransferOperation::ReadSource => CopyOperation::Read,
                CopyTransferOperation::WriteDestination => CopyOperation::Write,
                CopyTransferOperation::FlushDestination => CopyOperation::Flush,
                CopyTransferOperation::NativeCopy => CopyOperation::Write,
            };

            let message = match op {
                CopyOperation::OpenSource => {
                    format!("failed to open spool file: {}", entry.spool_path.display())
                }
                CopyOperation::CreateTemp => {
                    format!("failed to create temp file: {}", temp_dest.display())
                }
                CopyOperation::Read => format!("failed reading {}", entry.spool_path.display()),
                CopyOperation::Write => format!(
                    "failed copying {} -> {}",
                    entry.spool_path.display(),
                    temp_dest.display()
                ),
                CopyOperation::Flush => format!("failed flushing {}", temp_dest.display()),
                _ => format!(
                    "failed copying {} -> {}",
                    entry.spool_path.display(),
                    temp_dest.display()
                ),
            };

            lane_copy_failure(entry, op, source, message)
        }
    }
}

/// Test-only per-entry hook, invoked after a lane's copy step lands the
/// file at its final destination and before that entry is handed to the
/// verify worker. Unlike `copy.rs`/`stage.rs`'s single-shot
/// (`FnOnce`) after-copy hooks, this one is `Fn` and re-armed per test, and
/// receives the triggering `LaneEntry` -- lanes run genuinely concurrently
/// (unlike the single-file staging tests those hooks serve), so a
/// single-shot, argument-less hook would race between lanes over which
/// entry actually consumes it. Keying on the entry lets a test act only on
/// the specific file it cares about regardless of thread interleaving.
#[cfg(test)]
type LaneCopyHook = Arc<dyn Fn(&LaneEntry) + Send + Sync>;

#[cfg(test)]
static LANE_COPY_TEST_HOOK: std::sync::Mutex<Option<LaneCopyHook>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn set_lane_copy_test_hook(hook: LaneCopyHook) {
    *LANE_COPY_TEST_HOOK.lock().unwrap() = Some(hook);
}

#[cfg(test)]
fn clear_lane_copy_test_hook() {
    *LANE_COPY_TEST_HOOK.lock().unwrap() = None;
}

#[cfg(test)]
fn run_lane_copy_test_hook(entry: &LaneEntry) {
    let hook = LANE_COPY_TEST_HOOK.lock().unwrap().clone();
    if let Some(hook) = hook {
        hook(entry);
    }
}

#[cfg(not(test))]
fn run_lane_copy_test_hook(_entry: &LaneEntry) {}

/// Verify-side counterpart to [`run_lane_copy_test_hook`], invoked right
/// after an entry is dequeued for verification and before verification
/// itself runs. Lets a test hold the verify worker on a specific entry to
/// observe state strictly between "the copy worker handed this entry off
/// and moved on" and "the verify worker actually verified it" -- e.g.
/// proving the copy worker's normal completion never releases an entry
/// still awaiting verification.
#[cfg(test)]
static LANE_VERIFY_TEST_HOOK: std::sync::Mutex<Option<LaneCopyHook>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn set_lane_verify_test_hook(hook: LaneCopyHook) {
    *LANE_VERIFY_TEST_HOOK.lock().unwrap() = Some(hook);
}

#[cfg(test)]
fn clear_lane_verify_test_hook() {
    *LANE_VERIFY_TEST_HOOK.lock().unwrap() = None;
}

#[cfg(test)]
fn run_lane_verify_test_hook(entry: &LaneEntry) {
    let hook = LANE_VERIFY_TEST_HOOK.lock().unwrap().clone();
    if let Some(hook) = hook {
        hook(entry);
    }
}

#[cfg(not(test))]
fn run_lane_verify_test_hook(_entry: &LaneEntry) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copy::after_copy_test_hook_guard;
    use crate::error::CopyFailureClassification;
    use crossbeam_channel::RecvTimeoutError;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "pathsync-lanes-{prefix}-{nanos}-{}-{counter}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("failed to create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn signature_of(payload: &[u8]) -> FileSignature {
        FileSignature {
            size: payload.len() as u64,
            xxh3_128: xxhash_rust::xxh3::xxh3_128(payload),
        }
    }

    /// Writes `payload` into the spool store's run dir and registers it,
    /// returning a [`LaneEntry`] destined for `dest` (which is not
    /// necessarily under any real target root in these unit tests -- lanes
    /// don't inspect `dest`'s ancestry, only copy to it).
    fn stage_entry(
        store: &SpoolStore,
        name: &str,
        payload: &[u8],
        dest: PathBuf,
        pending_targets: impl IntoIterator<Item = usize>,
    ) -> LaneEntry {
        let spool_path = store.run_dir().join(name);
        fs::write(&spool_path, payload).expect("failed to write spool file");
        let entry_id = store.register(spool_path.clone(), payload.len() as u64, pending_targets);
        LaneEntry {
            entry_id,
            source: PathBuf::from("/source").join(name),
            spool_path,
            dest,
            size: payload.len() as u64,
            signature: signature_of(payload),
            display_name: name.to_string(),
        }
    }

    /// Drains `rx` until either `n` `Verified` events have been seen or the
    /// deadline elapses, returning however many were actually observed.
    fn count_verified_within(rx: &Receiver<WorkerEvent>, n: usize, timeout: Duration) -> usize {
        let deadline = std::time::Instant::now() + timeout;
        let mut seen = 0;
        while seen < n {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(WorkerEvent::Verified { .. }) => seen += 1,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        seen
    }

    #[test]
    fn worker_layout_is_pure_and_non_overlapping() {
        let layout = LaneWorkerLayout::new(3, 2);
        assert_eq!(layout.staging_worker_ids(), 0..3);
        assert_eq!(layout.copy_worker_id(0), 3);
        assert_eq!(layout.verify_worker_id(0), 4);
        assert_eq!(layout.copy_worker_id(1), 5);
        assert_eq!(layout.verify_worker_id(1), 6);
        assert_eq!(layout.total_workers(), 7);

        // Pure function: identical inputs always produce identical layout,
        // computable up front without starting anything.
        let same = LaneWorkerLayout::new(3, 2);
        assert_eq!(same.copy_worker_id(1), layout.copy_worker_id(1));
    }

    #[test]
    fn happy_path_two_targets_all_verified_and_spool_empty() {
        let temp = TempDir::new("happy");
        let store = Arc::new(
            SpoolStore::open(temp.path().join("spool").as_path(), "job", None, 0).unwrap(),
        );
        let target_a = temp.path().join("target-a");
        let target_b = temp.path().join("target-b");
        let (event_tx, event_rx) = unbounded::<WorkerEvent>();
        let layout = LaneWorkerLayout::new(1, 2);

        let lanes = start_target_lanes(
            &[target_a.clone(), target_b.clone()],
            Arc::clone(&store),
            layout,
            event_tx.clone(),
        );

        let mut spool_paths = Vec::new();
        for i in 0..3 {
            let name = format!("file-{i}.bin");
            let payload = format!("payload-{i}").into_bytes();
            let entry = stage_entry(
                &store,
                &name,
                &payload,
                target_a.join(&name),
                [0usize, 1usize],
            );
            spool_paths.push(entry.spool_path.clone());
            let entry_b = LaneEntry {
                dest: target_b.join(&name),
                ..entry.clone()
            };
            lanes[0].sender().send(entry).unwrap();
            lanes[1].sender().send(entry_b).unwrap();
        }

        drop(event_tx);
        for lane in lanes {
            lane.join(&unbounded().0);
        }

        let mut verified = 0;
        let mut copy_events = 0;
        while let Ok(event) = event_rx.recv() {
            match event {
                WorkerEvent::Verified { .. } => verified += 1,
                WorkerEvent::Finished { .. } => copy_events += 1,
                WorkerEvent::Error { .. } | WorkerEvent::VerificationFailed { .. } => {
                    panic!("unexpected failure event in happy path")
                }
                _ => {}
            }
        }

        assert_eq!(verified, 6, "3 files x 2 targets verified");
        assert_eq!(copy_events, 6, "3 files x 2 targets copied");

        for path in &spool_paths {
            assert!(
                !path.exists(),
                "spool file must evict once both targets terminal: {path:?}"
            );
        }
        assert_eq!(fs::read(target_a.join("file-0.bin")).unwrap(), b"payload-0");
        assert_eq!(fs::read(target_b.join("file-2.bin")).unwrap(), b"payload-2");

        // Capacity fully released.
        store.reserve(1024).unwrap();
    }

    #[test]
    fn slow_target_does_not_block_fast_target() {
        let _hook_guard = after_copy_test_hook_guard();
        let temp = TempDir::new("slow-isolation");
        let store = Arc::new(
            SpoolStore::open(temp.path().join("spool").as_path(), "job", None, 0).unwrap(),
        );
        let fast_target = temp.path().join("fast");
        let slow_target = temp.path().join("slow");
        let (event_tx, event_rx) = unbounded::<WorkerEvent>();
        let layout = LaneWorkerLayout::new(1, 2);

        let lanes = start_target_lanes(
            &[fast_target.clone(), slow_target.clone()],
            Arc::clone(&store),
            layout,
            event_tx.clone(),
        );

        // Gate: block "slow.bin" (fed only to lane 1) until the test
        // explicitly releases it, well after observing the fast lane
        // finish. Real blocking on a channel, not a sleep-based guess, so
        // this is deterministic rather than timing-flaky.
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = StdMutex::new(release_rx);
        clear_lane_copy_test_hook();
        set_lane_copy_test_hook(Arc::new(move |entry: &LaneEntry| {
            if entry.display_name == "slow.bin" {
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .expect("slow lane must be released by the test, not time out");
            }
        }));

        // Feed the slow lane first so, absent isolation, it could plausibly
        // block a shared resource before the fast lane even starts.
        let slow_entry = stage_entry(
            &store,
            "slow.bin",
            b"slow-payload",
            slow_target.join("slow.bin"),
            [1usize],
        );
        lanes[1].sender().send(slow_entry).unwrap();

        for i in 0..5 {
            let name = format!("fast-{i}.bin");
            let payload = format!("fast-payload-{i}").into_bytes();
            let entry = stage_entry(&store, &name, &payload, fast_target.join(&name), [0usize]);
            lanes[0].sender().send(entry).unwrap();
        }

        // The fast lane must fully verify all 5 files while the slow lane
        // is still blocked in the hook.
        let fast_verified = count_verified_within(&event_rx, 5, Duration::from_secs(10));
        assert_eq!(
            fast_verified, 5,
            "fast lane must complete independently of the stalled slow lane"
        );
        // At this point the fast lane could only have reached 5 `Verified`
        // events because the slow lane's copy worker is parked inside the
        // hook (it blocks on `release_rx` before `release_tx.send` below is
        // ever called) -- proving completion did not wait on the slow lane.

        // Now release the slow lane and let it finish.
        release_tx.send(()).unwrap();
        let slow_verified = count_verified_within(&event_rx, 1, Duration::from_secs(10));
        assert_eq!(slow_verified, 1, "slow lane must eventually complete too");

        clear_lane_copy_test_hook();
        drop(event_tx);
        for lane in lanes {
            lane.join(&unbounded().0);
        }

        store.reserve(1024).unwrap();
    }

    #[test]
    fn verify_mismatch_on_one_target_does_not_affect_the_other() {
        let _hook_guard = after_copy_test_hook_guard();
        let temp = TempDir::new("verify-mismatch");
        let store = Arc::new(
            SpoolStore::open(temp.path().join("spool").as_path(), "job", None, 0).unwrap(),
        );
        let target_a = temp.path().join("target-a");
        let target_b = temp.path().join("target-b");
        let (event_tx, event_rx) = unbounded::<WorkerEvent>();
        let layout = LaneWorkerLayout::new(1, 2);

        let lanes = start_target_lanes(
            &[target_a.clone(), target_b.clone()],
            Arc::clone(&store),
            layout,
            event_tx.clone(),
        );

        let corrupt_dest = target_a.join("corrupt.bin");
        clear_lane_copy_test_hook();
        set_lane_copy_test_hook(Arc::new({
            let corrupt_dest = corrupt_dest.clone();
            move |entry: &LaneEntry| {
                // Match on the specific (target-resolved) destination, not
                // just the display name: both lanes copy a file sharing the
                // same source basename here, so matching on name alone
                // would corrupt both targets' copies instead of just
                // target A's.
                if entry.dest == corrupt_dest {
                    fs::write(&entry.dest, b"corrupted-and-different").unwrap();
                }
            }
        }));

        let payload = b"original-payload";
        let entry = stage_entry(
            &store,
            "corrupt.bin",
            payload,
            corrupt_dest.clone(),
            [0usize, 1usize],
        );
        let entry_b = LaneEntry {
            dest: target_b.join("corrupt.bin"),
            ..entry.clone()
        };
        lanes[0].sender().send(entry).unwrap();
        lanes[1].sender().send(entry_b).unwrap();

        let mut verify_failed_targets = Vec::new();
        let mut verified_targets = Vec::new();
        for _ in 0..2 {
            match recv_next_terminal(&event_rx) {
                WorkerEvent::Verified { target, .. } => verified_targets.push(target),
                WorkerEvent::VerificationFailed { failure, .. } => {
                    assert_eq!(failure.classification, CopyFailureClassification::Local);
                    verify_failed_targets.push(failure.dest.unwrap());
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }

        assert_eq!(verify_failed_targets, vec![target_a.join("corrupt.bin")]);
        assert_eq!(verified_targets, vec![target_b]);

        clear_lane_copy_test_hook();
        drop(event_tx);
        for lane in lanes {
            lane.join(&unbounded().0);
        }

        // Both targets reached a terminal outcome (one failed, one
        // verified), so the shared spool entry must still evict.
        let residue: Vec<_> = fs::read_dir(store.run_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name() != ".pathsync-lock")
            .collect();
        assert!(residue.is_empty(), "spool entry must evict: {residue:?}");
        store.reserve(1024).unwrap();
    }

    fn recv_next_terminal(rx: &Receiver<WorkerEvent>) -> WorkerEvent {
        loop {
            match rx
                .recv_timeout(Duration::from_secs(10))
                .expect("timed out waiting for event")
            {
                event @ (WorkerEvent::Verified { .. } | WorkerEvent::VerificationFailed { .. }) => {
                    return event;
                }
                _ => continue,
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn lane_local_copy_failure_only_affects_its_own_target() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new("copy-failure");
        let store = Arc::new(
            SpoolStore::open(temp.path().join("spool").as_path(), "job", None, 0).unwrap(),
        );
        let target_ok = temp.path().join("target-ok");
        let target_denied = temp.path().join("target-denied");
        fs::create_dir_all(&target_denied).unwrap();
        // Remove write permission on the target directory itself so
        // creating a new file inside it fails with PermissionDenied.
        fs::set_permissions(&target_denied, fs::Permissions::from_mode(0o555)).unwrap();

        let (event_tx, event_rx) = unbounded::<WorkerEvent>();
        let layout = LaneWorkerLayout::new(1, 2);
        let lanes = start_target_lanes(
            &[target_ok.clone(), target_denied.clone()],
            Arc::clone(&store),
            layout,
            event_tx.clone(),
        );

        let payload = b"payload";
        let entry = stage_entry(
            &store,
            "file.bin",
            payload,
            target_ok.join("file.bin"),
            [0usize, 1usize],
        );
        let entry_denied = LaneEntry {
            dest: target_denied.join("file.bin"),
            ..entry.clone()
        };
        lanes[0].sender().send(entry).unwrap();
        lanes[1].sender().send(entry_denied).unwrap();

        let mut saw_copy_failure = false;
        let mut saw_verified = false;
        for _ in 0..2 {
            match rx_next_terminal_or_error(&event_rx) {
                WorkerEvent::Verified { .. } => saw_verified = true,
                WorkerEvent::Error { failure, .. } => {
                    assert_eq!(failure.dest, Some(target_denied.join("file.bin")));
                    saw_copy_failure = true;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(saw_copy_failure, "denied target must report a copy failure");
        assert!(saw_verified, "unaffected target must still verify");

        // Restore permissions so TempDir's Drop can clean up.
        fs::set_permissions(&target_denied, fs::Permissions::from_mode(0o755)).unwrap();

        drop(event_tx);
        for lane in lanes {
            lane.join(&unbounded().0);
        }

        store.reserve(1024).unwrap();
    }

    fn rx_next_terminal_or_error(rx: &Receiver<WorkerEvent>) -> WorkerEvent {
        loop {
            match rx
                .recv_timeout(Duration::from_secs(10))
                .expect("timed out waiting for event")
            {
                event @ (WorkerEvent::Verified { .. } | WorkerEvent::Error { .. }) => return event,
                _ => continue,
            }
        }
    }

    #[test]
    fn lane_panic_mid_drain_releases_pending_entries_and_leaves_other_lanes_unaffected() {
        let _hook_guard = after_copy_test_hook_guard();
        let temp = TempDir::new("panic-safety");
        let store = Arc::new(
            SpoolStore::open(temp.path().join("spool").as_path(), "job", None, 0).unwrap(),
        );
        let panicking_target = temp.path().join("panicking");
        let healthy_target = temp.path().join("healthy");
        let (event_tx, event_rx) = unbounded::<WorkerEvent>();
        let layout = LaneWorkerLayout::new(1, 2);

        let lanes = start_target_lanes(
            &[panicking_target.clone(), healthy_target.clone()],
            Arc::clone(&store),
            layout,
            event_tx.clone(),
        );

        clear_lane_copy_test_hook();
        set_lane_copy_test_hook(Arc::new(|entry: &LaneEntry| {
            if entry.display_name == "panic.bin" {
                panic!("forced test panic mid-drain");
            }
        }));

        // Lane 0 (panicking_target): a.bin completes fine, panic.bin
        // triggers the panic while in-flight, c.bin never even gets
        // dequeued -- all three must still evict.
        let a = stage_entry(
            &store,
            "a.bin",
            b"a-payload",
            panicking_target.join("a.bin"),
            [0usize],
        );
        let panic_entry = stage_entry(
            &store,
            "panic.bin",
            b"panic-payload",
            panicking_target.join("panic.bin"),
            [0usize],
        );
        let c = stage_entry(
            &store,
            "c.bin",
            b"c-payload",
            panicking_target.join("c.bin"),
            [0usize],
        );
        let a_path = a.spool_path.clone();
        let panic_path = panic_entry.spool_path.clone();
        let c_path = c.spool_path.clone();
        lanes[0].sender().send(a).unwrap();
        lanes[0].sender().send(panic_entry).unwrap();
        lanes[0].sender().send(c).unwrap();

        // Lane 1 (healthy_target): fully independent, must be unaffected.
        let healthy = stage_entry(
            &store,
            "healthy.bin",
            b"healthy-payload",
            healthy_target.join("healthy.bin"),
            [1usize],
        );
        let healthy_path = healthy.spool_path.clone();
        lanes[1].sender().send(healthy).unwrap();

        // Wait for every spool file to disappear (evicted), rather than
        // asserting immediately -- the panic-triggered release and the
        // healthy lane's normal completion both happen asynchronously.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let all_gone = ![&a_path, &panic_path, &c_path, &healthy_path]
                .into_iter()
                .any(|p| p.exists());
            if all_gone {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "spool entries were not all released after the panic: a={} panic={} c={} healthy={}",
                a_path.exists(),
                panic_path.exists(),
                c_path.exists(),
                healthy_path.exists(),
            );
            thread::sleep(Duration::from_millis(20));
        }

        clear_lane_copy_test_hook();
        drop(event_tx);

        let mut lanes = lanes.into_iter();
        let panicking_lane = lanes.next().unwrap();
        let healthy_lane = lanes.next().unwrap();

        let (report_tx, report_rx) = unbounded::<WorkerEvent>();
        panicking_lane.join(&report_tx);
        healthy_lane.join(&report_tx);
        drop(report_tx);

        let mut saw_systemic_panic_error = false;
        while let Ok(event) = report_rx.recv() {
            if let WorkerEvent::Error { failure, .. } = event {
                assert_eq!(failure.classification, CopyFailureClassification::Systemic);
                assert_eq!(failure.dest, Some(panicking_target.clone()));
                saw_systemic_panic_error = true;
            }
        }
        assert!(
            saw_systemic_panic_error,
            "joining a lane whose worker panicked must report one systemic WorkerEvent::Error"
        );

        // The healthy lane's own events (Verified etc.) went to `event_tx`
        // above and were already implicitly proven by the eviction wait,
        // but double-check no failure was recorded for it.
        while let Ok(event) = event_rx.try_recv() {
            if let WorkerEvent::Error { failure, .. }
            | WorkerEvent::VerificationFailed { failure, .. } = event
            {
                assert_ne!(
                    failure.dest.as_deref(),
                    Some(healthy_target.join("healthy.bin").as_path()),
                    "healthy lane must not report any failure"
                );
            }
        }

        // Capacity fully released despite the panic.
        store.reserve(1024).unwrap();
    }

    #[test]
    fn copy_worker_normal_completion_does_not_prematurely_release_entry_awaiting_verify() {
        let _hook_guard = after_copy_test_hook_guard();
        let temp = TempDir::new("no-premature-release");
        let store = Arc::new(
            SpoolStore::open(temp.path().join("spool").as_path(), "job", None, 0).unwrap(),
        );
        let target = temp.path().join("target");
        let (event_tx, event_rx) = unbounded::<WorkerEvent>();
        let layout = LaneWorkerLayout::new(1, 1);

        let lanes = start_target_lanes(
            std::slice::from_ref(&target),
            Arc::clone(&store),
            layout,
            event_tx.clone(),
        );

        // Gate: hold the verify worker on "gated.bin" until the test
        // explicitly releases it. This lets us observe state strictly
        // between "the copy worker finished this entry and moved on" and
        // "the verify worker actually verified it" -- real blocking on a
        // channel, not a sleep-based guess.
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = StdMutex::new(release_rx);
        clear_lane_verify_test_hook();
        set_lane_verify_test_hook(Arc::new(move |entry: &LaneEntry| {
            if entry.display_name == "gated.bin" {
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .expect("verify worker must be released by the test, not time out");
            }
        }));

        // Register the entry pending on target 0 (the real lane under
        // test) *and* a second, phantom target index (1) that no lane
        // services. Target 1 stays deliberately pending until the test
        // resolves it directly below, which turns "was target 0 already
        // (wrongly) marked terminal by the copy worker's guard?" into an
        // observable eviction question: resolving only the phantom target
        // evicts the entry if and only if target 0 had *also* already
        // gone terminal.
        let payload = b"gated-payload";
        let entry = stage_entry(
            &store,
            "gated.bin",
            payload,
            target.join("gated.bin"),
            [0usize, 1usize],
        );
        let spool_path = entry.spool_path.clone();
        let entry_id = entry.entry_id;
        lanes[0].sender().send(entry).unwrap();

        // Wait for the copy step to finish -- proves the copy worker's
        // guard already disarmed and handed the entry to the verify
        // queue -- all while the verify worker is still parked in the
        // hook above, i.e. strictly before target 0 could legitimately go
        // terminal.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut saw_finished = false;
        while std::time::Instant::now() < deadline {
            match event_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(WorkerEvent::Finished { .. }) => {
                    saw_finished = true;
                    break;
                }
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(
            saw_finished,
            "copy step must finish (and hand off to verify) before verify is released"
        );

        // Drive the copy worker to a genuine normal (non-panicking) loop
        // exit -- closing its queue -- while the verify worker remains
        // stalled on this very entry, exercising the copy worker's
        // guard's normal-exit `Drop` path (a no-op per the fix) at
        // exactly the moment it matters. `TargetLane::join` blocks on the
        // verify worker too, so run it on a background thread.
        let mut lanes = lanes.into_iter();
        let lane = lanes.next().unwrap();
        let (report_tx, report_rx) = unbounded::<WorkerEvent>();
        let join_handle = thread::spawn(move || {
            lane.join(&report_tx);
        });
        thread::sleep(Duration::from_millis(100));

        // The crux of the test: target 0 must not yet be terminal for this
        // entry. Resolve only the phantom target (1) and confirm the spool
        // file still exists -- if the copy worker's normal completion had
        // wrongly released target 0 too (the bug this test guards
        // against), the entry would have both targets terminal right now
        // and evict, deleting the spool file before verify ever ran.
        store.mark_terminal(entry_id, 1);
        assert!(
            spool_path.exists(),
            "entry must not evict before the verify worker actually verifies target 0 -- \
             a premature release would mean the copy worker's normal completion wrongly \
             triggered its guard's bulk-release logic"
        );

        // Now let verify actually run and finish for real.
        release_tx.send(()).unwrap();
        join_handle.join().unwrap();
        while let Ok(event) = report_rx.try_recv() {
            if let WorkerEvent::Error { .. } = event {
                panic!("unexpected worker error/panic in normal-completion test");
            }
        }

        clear_lane_verify_test_hook();
        drop(event_tx);

        // Both targets are now terminal (1 resolved above, 0 resolved by
        // the real verify pass) -- the entry must evict for real.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while spool_path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "entry never evicted after verify genuinely completed"
            );
            thread::sleep(Duration::from_millis(20));
        }

        store.reserve(1024).unwrap();
    }
}
