//! Source-to-spool staging pipeline (U3): copies a planned source file into
//! the spool store, computing its `xxh3_128` signature in the same pass
//! (one source read total), then read-back-verifies the spool copy and
//! re-checks the source is unchanged before registering a spool entry.
//!
//! This module only stages one file at a time into an already-open
//! [`SpoolStore`]; it owns none of the run orchestration (U5) or the
//! spool-to-target drain lanes (U4) that later consume the entries it
//! registers.
//!
//! Not yet wired into `run_copy`: U4 and U5 are the production consumers
//! of this module's public surface. Directly covered by this module's own
//! tests in the meantime -- matches the `#![allow(dead_code)]` convention
//! already used in `copy_fast_path.rs` for the same reason.
#![allow(dead_code)]

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use filetime::{FileTime, set_file_mtime};

use crate::copy::{
    FileSignature, classify_failure, ensure_source_unchanged, hash_file_xxh3_128,
    run_after_copy_test_hook, source_signature_key, temp_path_for,
};
use crate::copy_fast_path::{CopyTransferError, CopyTransferOperation, copy_file_data_hashing};
use crate::error::{CopyFailure, CopyOperation, SpoolError};
use crate::spool::{EntryId, SpoolStore};

/// Monotonic counter feeding unique spool file names, since several source
/// files can share a basename across different source subdirectories.
static SPOOL_ENTRY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A second after-copy test hook, private to this module, mirroring
/// `src/copy.rs`'s `AFTER_COPY_TEST_HOOK`/`set_after_copy_test_hook`/
/// `run_after_copy_test_hook` pattern exactly. Kept separate from the
/// `copy.rs` hook (rather than sharing its single slot for both failure
/// tests) so that only this module's own tests can ever arm or consume it;
/// see `run_staging_copy`'s call site for why both hooks fire at the same
/// point.
#[cfg(test)]
static AFTER_STAGE_COPY_TEST_HOOK: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn set_after_stage_copy_test_hook(hook: Box<dyn FnOnce() + Send>) {
    *AFTER_STAGE_COPY_TEST_HOOK.lock().unwrap() = Some(hook);
}

#[cfg(test)]
fn run_after_stage_copy_test_hook() {
    if let Some(hook) = AFTER_STAGE_COPY_TEST_HOOK.lock().unwrap().take() {
        hook();
    }
}

#[cfg(not(test))]
fn run_after_stage_copy_test_hook() {}

/// A file successfully staged and verified into the spool.
///
/// `signature` is the `xxh3_128` computed in-flight during the staging copy
/// -- the integrity anchor for the rest of the pipeline. Per the plan's
/// "Integrity anchor is the source signature" decision, later hops (U4's
/// target verify) must compare against this value, never a hash re-derived
/// from the spool file.
#[derive(Debug, Clone)]
pub(crate) struct StagedFile {
    pub(crate) entry_id: EntryId,
    pub(crate) source: PathBuf,
    pub(crate) spool_path: PathBuf,
    pub(crate) size: u64,
    pub(crate) signature: FileSignature,
}

/// A file that reached a terminal *failed* staging outcome. Carries one
/// [`CopyFailure`] per target index from the caller-supplied
/// `pending_targets`, all sharing the same underlying cause, so a caller
/// can record a failure for every target that was waiting on this file (R9).
#[derive(Debug, Clone)]
pub(crate) struct StageFailure {
    pub(crate) source: PathBuf,
    pub(crate) target_failures: Vec<(usize, CopyFailure)>,
}

/// Why [`stage_file`] did not return a [`StagedFile`].
#[derive(Debug)]
pub(crate) enum StageError {
    /// `SpoolStore::reserve` could not grant the reservation. This is
    /// explicitly *not* a per-file terminal staging outcome: nothing was
    /// reserved, so there is nothing to release, and the caller (run
    /// orchestration) decides how to treat it -- e.g.
    /// `SpoolError::WouldDeadlock` is a run-ending systemic condition, not a
    /// failure to attribute to `pending_targets`.
    Reserve(SpoolError),
    /// Staging reached a terminal failed outcome for every entry in
    /// `pending_targets`. Any reservation made has already been released
    /// and the temp/spool file already cleaned up.
    Failed(StageFailure),
}

/// Stages one planned source file into `spool`: reserves capacity, copies
/// source -> spool temp file while hashing in-flight (single source read),
/// fsyncs, sets the spool file's mtime to the source's mtime, renames into
/// place, read-back-verifies the spool copy against the in-flight hash,
/// re-checks the source is unchanged, and registers the entry.
///
/// `pending_targets` is the set of target-lane indices that still need this
/// file; it is consumed at most once into an owned buffer so it can be used
/// both for the spool registration and, on failure, to build one
/// [`CopyFailure`] per pending target.
///
/// `progress` receives cumulative bytes copied during the source -> spool
/// copy (not during read-back verification).
pub(crate) fn stage_file(
    spool: &SpoolStore,
    source: &Path,
    pending_targets: impl IntoIterator<Item = usize>,
    mut progress: impl FnMut(u64),
) -> Result<StagedFile, StageError> {
    let targets: Vec<usize> = pending_targets.into_iter().collect();

    // Step 1: pre-copy source signature, so mid-copy mutation is detectable.
    let source_key = source_signature_key(source).map_err(|err| {
        StageError::Failed(no_reservation_failure(
            source,
            &targets,
            CopyOperation::OpenSource,
            &err,
            format!("failed to stat source file: {}", source.display()),
        ))
    })?;
    let size = source_key.size;

    // Step 2: reserve capacity. Errors here are the caller's problem: no
    // reservation was made, so there's nothing to release.
    spool.reserve(size).map_err(StageError::Reserve)?;

    let spool_path = unique_spool_path(spool.run_dir(), source);
    let temp_path = temp_path_for(&spool_path);

    let stage_result =
        run_staging_copy(source, &source_key, &temp_path, &spool_path, &mut progress);

    match stage_result {
        Ok(signature) => {
            let entry_id = spool.register(spool_path.clone(), size, targets);
            Ok(StagedFile {
                entry_id,
                source: source.to_path_buf(),
                spool_path,
                size,
                signature,
            })
        }
        Err(cause) => {
            // Best-effort cleanup: whichever of temp/final path exists.
            // Harmless no-op if already gone (e.g. never created, or
            // already removed by the failed rename itself).
            let _ = fs::remove_file(&temp_path);
            let _ = fs::remove_file(&spool_path);

            // Release the reservation through the spool store's normal
            // per-entry eviction path: register the (now-defunct) entry
            // with the caller-supplied pending targets, then immediately
            // mark each one terminal. `register`'s bytes were already
            // accounted by `reserve` above; `mark_terminal` driving the
            // pending set to empty is what triggers `evict_locked` to
            // release them.
            //
            // This deliberately does NOT use
            // `mark_all_remaining_terminal_for_target`: that hook releases
            // *every* live entry pending on a target index, which would be
            // wrong here -- other files may be mid-stage concurrently for
            // the same target lane, and this failure must only affect this
            // one entry. Per-entry `mark_terminal` (idempotent, entry-id
            // scoped) is the correct tool.
            let entry_id = spool.register(spool_path.clone(), size, targets.iter().copied());
            for &target in &targets {
                spool.mark_terminal(entry_id, target);
            }

            Err(StageError::Failed(StageFailure {
                source: source.to_path_buf(),
                target_failures: targets.into_iter().map(|t| (t, cause.clone())).collect(),
            }))
        }
    }
}

/// Runs steps 3-5 of staging (copy+hash, read-back verify, source recheck)
/// and returns the confirmed in-flight signature on success.
fn run_staging_copy(
    source: &Path,
    source_key: &crate::copy::SourceSignatureKey,
    temp_path: &Path,
    spool_path: &Path,
    progress: &mut impl FnMut(u64),
) -> Result<FileSignature, CopyFailure> {
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let copied = copy_file_data_hashing(source, temp_path, |chunk| hasher.update(chunk), progress)
        .map_err(|err| stage_transfer_failure(source, temp_path, err))?;

    if let Some(mtime) = source_key.mtime {
        set_file_mtime(temp_path, FileTime::from_system_time(mtime)).map_err(|err| {
            stage_copy_failure(
                source,
                Some(temp_path.to_path_buf()),
                CopyOperation::SetMtime,
                &err,
                format!("failed setting mtime on {}", temp_path.display()),
            )
        })?;
    }

    fs::rename(temp_path, spool_path).map_err(|err| {
        stage_copy_failure(
            source,
            Some(spool_path.to_path_buf()),
            CopyOperation::Rename,
            &err,
            format!(
                "failed to move staged temp file into place: {} -> {}",
                temp_path.display(),
                spool_path.display()
            ),
        )
    })?;

    // Test-only hooks, firing right after the spool copy is in its final
    // place and before either verification step below.
    //
    // `run_after_copy_test_hook` is `src/copy.rs`'s existing after-copy
    // failure-injection point, reused verbatim as instructed (exercised by
    // the source-recheck failure test: it mutates the source between here
    // and the source-unchanged recheck below).
    run_after_copy_test_hook();
    // `run_after_stage_copy_test_hook` is a second, stage.rs-local hook at
    // the same point, kept separate from the one above so it can only ever
    // be armed/consumed by this module's own tests (exercised by the
    // read-back-mismatch failure test: it corrupts the just-renamed spool
    // file before the read-back verify below runs).
    run_after_stage_copy_test_hook();

    // Step 4: read back the spool copy and compare against the in-flight
    // hash computed during the copy above.
    let readback_metadata = fs::metadata(spool_path).map_err(|err| {
        stage_copy_failure(
            source,
            Some(spool_path.to_path_buf()),
            CopyOperation::Verify,
            &err,
            format!(
                "staged-verify failed: could not stat spool copy {}",
                spool_path.display()
            ),
        )
    })?;
    let readback_signature = hash_file_xxh3_128(spool_path, readback_metadata.len(), |_| {})
        .map_err(|err| {
            stage_copy_failure(
                source,
                Some(spool_path.to_path_buf()),
                CopyOperation::Verify,
                &err,
                format!(
                    "staged-verify failed: could not read spool copy {}",
                    spool_path.display()
                ),
            )
        })?;

    // Deliberately uses `copied` (the byte count actually read from source
    // during the hash pass above), not `readback_metadata.len()`: the
    // in-flight signature must reflect only what was read from source, so a
    // post-rename spool corruption that also changes the file's length
    // can't accidentally make the two signatures' sizes agree.
    let inflight_signature = FileSignature {
        size: copied,
        xxh3_128: hasher.digest128(),
    };

    if inflight_signature != readback_signature {
        return Err(stage_copy_failure(
            source,
            Some(spool_path.to_path_buf()),
            CopyOperation::Verify,
            &io::Error::new(io::ErrorKind::InvalidData, "spool copy signature mismatch"),
            format!(
                "staged-verify failed: signature mismatch for {}",
                spool_path.display()
            ),
        ));
    }

    // Step 5: re-check the source is unchanged since step 1.
    ensure_source_unchanged(source, source_key).map_err(|err| {
        stage_copy_failure(
            source,
            None,
            CopyOperation::SourceChanged,
            &err,
            format!("source changed during staging: {}", source.display()),
        )
    })?;

    Ok(inflight_signature)
}

/// Builds a [`StageFailure`] for a cause that occurred *before* any
/// reservation was made (so there is nothing to release in the spool
/// store): one shared [`CopyFailure`] fanned out across `targets`.
fn no_reservation_failure(
    source: &Path,
    targets: &[usize],
    operation: CopyOperation,
    err: &io::Error,
    message: String,
) -> StageFailure {
    let failure = stage_copy_failure(source, None, operation, err, message);
    StageFailure {
        source: source.to_path_buf(),
        target_failures: targets.iter().map(|&t| (t, failure.clone())).collect(),
    }
}

fn stage_copy_failure(
    source: &Path,
    dest: Option<PathBuf>,
    operation: CopyOperation,
    err: &io::Error,
    message: String,
) -> CopyFailure {
    let kind = err.kind();
    let raw_os_error = err.raw_os_error();
    CopyFailure {
        source: source.to_path_buf(),
        dest,
        operation,
        kind,
        raw_os_error,
        classification: classify_failure(kind, raw_os_error, operation),
        message,
    }
}

fn stage_transfer_failure(
    source: &Path,
    temp_path: &Path,
    error: CopyTransferError,
) -> CopyFailure {
    match error {
        CopyTransferError::Io {
            operation,
            source: err,
        } => {
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
                    format!("failed to open source file: {}", source.display())
                }
                CopyOperation::CreateTemp => {
                    format!("failed to create spool temp file: {}", temp_path.display())
                }
                CopyOperation::Read => format!("failed reading {}", source.display()),
                CopyOperation::Flush => format!("failed flushing {}", temp_path.display()),
                _ => format!(
                    "failed staging {} -> {}",
                    source.display(),
                    temp_path.display()
                ),
            };
            stage_copy_failure(source, Some(temp_path.to_path_buf()), op, &err, message)
        }
        CopyTransferError::UnsupportedNative { .. } => stage_copy_failure(
            source,
            Some(temp_path.to_path_buf()),
            CopyOperation::Write,
            &io::Error::other("staging copy unexpectedly reported native-unsupported"),
            format!(
                "failed staging {} -> {}",
                source.display(),
                temp_path.display()
            ),
        ),
    }
}

/// A unique path under `run_dir` for this staged file, keeping the source's
/// original file name for debuggability while avoiding collisions when
/// multiple planned files share a basename (different source subdirs).
fn unique_spool_path(run_dir: &Path, source: &Path) -> PathBuf {
    let counter = SPOOL_ENTRY_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut file_name = OsString::from(format!("{counter:016x}-"));
    match source.file_name() {
        Some(name) => file_name.push(name),
        None => file_name.push("staged"),
    }
    run_dir.join(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copy::{after_copy_test_hook_guard, set_after_copy_test_hook};
    use crate::error::CopyFailureClassification;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "pathsync-stage-{prefix}-{nanos}-{}",
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

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create parent directory");
        }
        fs::write(path, contents).expect("failed to write file");
    }

    /// Non-lockfile entries in `run_dir` -- i.e. staged/temp files only,
    /// excluding the spool store's own `.pathsync-lock` (which lives
    /// directly in the run dir for the store's whole lifetime and isn't
    /// staging residue).
    fn run_dir_entries(run_dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(run_dir)
            .expect("failed to read run dir")
            .map(|entry| entry.expect("failed to read dir entry").path())
            .filter(|path| path.file_name().and_then(|n| n.to_str()) != Some(".pathsync-lock"))
            .collect()
    }

    /// Reserve must succeed promptly (not hang or error) -- proves the
    /// spool store's capacity was actually released.
    fn assert_reservation_released(store: Arc<SpoolStore>, bytes: u64) {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(store.reserve(bytes));
        });
        let result = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("reserve() must return promptly after release, not hang");
        assert!(
            result.is_ok(),
            "expected reservation to be released and re-reservable, got {result:?}"
        );
    }

    #[test]
    fn happy_path_staged_file_matches_source_and_returns_source_signature() {
        // Every `stage_file` call fires the after-copy test hooks
        // unconditionally (as a no-op when nothing is armed), so this test
        // must still serialize against the hook-arming tests below --
        // otherwise it could steal a hook one of them just armed.
        let _hook_guard = after_copy_test_hook_guard();
        let temp = TempDir::new("happy");
        let source = temp.path().join("source/photo.jpg");
        let payload = b"some source bytes for staging";
        write_file(&source, payload);
        // Give the source a distinctive, non-"now" mtime so the "spool copy
        // mtime matches source mtime" assertion below is meaningful.
        let source_mtime = FileTime::from_unix_time(1_700_000_000, 0);
        filetime::set_file_mtime(&source, source_mtime).unwrap();

        let store = SpoolStore::open(temp.path().join("spool").as_path(), "job", None, 0).unwrap();

        let mut progress_calls = Vec::new();
        let staged = stage_file(&store, &source, [0usize, 1usize], |copied| {
            progress_calls.push(copied);
        })
        .expect("staging should succeed");

        assert_eq!(staged.source, source);
        assert_eq!(staged.size, payload.len() as u64);
        assert_eq!(fs::read(&staged.spool_path).unwrap(), payload);

        let spool_mtime =
            FileTime::from_last_modification_time(&fs::metadata(&staged.spool_path).unwrap());
        assert_eq!(
            spool_mtime, source_mtime,
            "spool copy mtime must match source"
        );

        let expected_signature = FileSignature {
            size: payload.len() as u64,
            xxh3_128: xxhash_rust::xxh3::xxh3_128(payload),
        };
        assert_eq!(
            staged.signature, expected_signature,
            "returned signature must equal an independently computed xxh3_128 of the source"
        );
        assert_eq!(progress_calls.last().copied(), Some(payload.len() as u64));
    }

    #[test]
    fn source_mutated_mid_staging_fails_and_releases_reservation() {
        let _hook_guard = after_copy_test_hook_guard();
        let temp = TempDir::new("source-mutated");
        let source = temp.path().join("source/photo.jpg");
        let original = b"original-bytes";
        write_file(&source, original);

        let store = Arc::new(
            SpoolStore::open(
                temp.path().join("spool").as_path(),
                "job",
                Some(original.len() as u64),
                0,
            )
            .unwrap(),
        );
        let run_dir = store.run_dir().to_path_buf();

        set_after_copy_test_hook(Box::new({
            let source = source.clone();
            move || write_file(&source, b"a-different-length-payload")
        }));

        let err = stage_file(&store, &source, [0usize], |_| {})
            .expect_err("staging must fail when source mutates mid-copy");

        let StageError::Failed(failure) = err else {
            panic!("expected StageError::Failed, got a reservation error");
        };
        assert_eq!(failure.source, source);
        assert_eq!(failure.target_failures.len(), 1);
        assert_eq!(failure.target_failures[0].0, 0);
        assert_eq!(
            failure.target_failures[0].1.operation,
            CopyOperation::SourceChanged
        );

        assert!(
            run_dir_entries(&run_dir).is_empty(),
            "no temp or spool residue must remain after a staging failure"
        );

        assert_reservation_released(store, original.len() as u64);
    }

    #[test]
    fn spool_readback_mismatch_fails_and_releases_reservation() {
        let _hook_guard = after_copy_test_hook_guard();
        let temp = TempDir::new("readback-mismatch");
        let source = temp.path().join("source/clip.mov");
        let payload = b"payload-that-will-be-corrupted-in-the-spool";
        write_file(&source, payload);

        let store = Arc::new(
            SpoolStore::open(
                temp.path().join("spool").as_path(),
                "job",
                Some(payload.len() as u64),
                0,
            )
            .unwrap(),
        );
        let run_dir = store.run_dir().to_path_buf();

        // Uses the stage.rs-local hook (not the one shared with copy.rs),
        // since this failure point doesn't need the shared mechanism -- it
        // only needs *a* hook at the same call site.
        set_after_stage_copy_test_hook(Box::new({
            let run_dir = run_dir.clone();
            move || {
                // The just-renamed spool file is the only staged entry in
                // the run dir at this point in a single-file test; corrupt
                // it without needing to know its generated name up front.
                for path in run_dir_entries(&run_dir) {
                    fs::write(&path, b"corrupted-and-different-length").unwrap();
                }
            }
        }));

        let err = stage_file(&store, &source, [0usize, 1usize], |_| {})
            .expect_err("staging must fail when the spool copy read-back mismatches");

        let StageError::Failed(failure) = err else {
            panic!("expected StageError::Failed, got a reservation error");
        };
        assert_eq!(failure.target_failures.len(), 2);
        for (target, copy_failure) in &failure.target_failures {
            assert!(*target == 0 || *target == 1);
            assert_eq!(copy_failure.operation, CopyOperation::Verify);
            assert_eq!(
                copy_failure.classification,
                CopyFailureClassification::Local
            );
        }

        assert!(
            run_dir_entries(&run_dir).is_empty(),
            "no temp or spool residue must remain after a staging failure"
        );

        assert_reservation_released(store, payload.len() as u64);
    }

    #[test]
    fn multiple_files_have_independent_terminal_outcomes() {
        let _hook_guard = after_copy_test_hook_guard();
        let temp = TempDir::new("multi-file");
        let source_a = temp.path().join("source/a.jpg");
        let source_c = temp.path().join("source/c.jpg");
        // `b` deliberately does not exist, so it fails at the pre-reserve
        // signature step without touching the spool store at all -- a
        // simple, hook-free failure that keeps this test independent of
        // the shared after-copy test hook.
        let source_b = temp.path().join("source/missing-b.jpg");
        write_file(&source_a, b"payload-a");
        write_file(&source_c, b"payload-c-longer");

        let store = SpoolStore::open(temp.path().join("spool").as_path(), "job", None, 0).unwrap();

        let staged_a = stage_file(&store, &source_a, [0usize], |_| {}).expect("a should succeed");
        let failed_b = stage_file(&store, &source_b, [0usize, 1usize], |_| {})
            .expect_err("b should fail: source does not exist");
        let staged_c = stage_file(&store, &source_c, [1usize], |_| {}).expect("c should succeed");

        assert_ne!(
            staged_a.entry_id, staged_c.entry_id,
            "each staged file must get its own entry id"
        );
        assert_eq!(staged_a.source, source_a);
        assert_eq!(staged_c.source, source_c);

        let StageError::Failed(failure_b) = failed_b else {
            panic!("expected StageError::Failed for the missing source");
        };
        assert_eq!(failure_b.target_failures.len(), 2);

        // b's failure must not have touched a's or c's entries: both
        // remain registered (not yet evicted -- no target has marked them
        // terminal) with their staged spool files intact.
        assert!(staged_a.spool_path.exists());
        assert!(staged_c.spool_path.exists());
        assert_eq!(fs::read(&staged_a.spool_path).unwrap(), b"payload-a");
        assert_eq!(fs::read(&staged_c.spool_path).unwrap(), b"payload-c-longer");

        let entries = run_dir_entries(store.run_dir());
        assert_eq!(
            entries.len(),
            2,
            "only a's and c's spool files should remain; b never reserved/registered anything"
        );
    }
}
