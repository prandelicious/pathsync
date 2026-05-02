use console::Term;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use filetime::{FileTime, set_file_mtime};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, BufReader, ErrorKind, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::ResolvedJob;
use crate::copy_fast_path::{CopyTransferError, CopyTransferOperation, copy_file_data};
use crate::error::{CopyError, CopyFailure, CopyFailureClassification, CopyOperation};
use crate::format::{format_duration, human_bytes, human_rate};
use crate::plan::{PlanningStats, TransferPlan};
use crate::policy::TransferPolicy;
use crate::progress_format::{
    GlyphSet, plain_progress_line, render_live_screen_with_width_and_glyphs,
    render_post_run_screen_with_glyphs, worker_label, worker_line, worker_prefix,
};
use crate::progress_model::{
    CategoryRowModel, ErrorRowModel, LiveScreenModel, PhaseKind, PostRunScreenModel,
    ProgressBarModel, ProgressSnapshot, SummaryMetric, TargetProgressRowModel,
    TargetResultRowModel, TransferCategory, TransferRowPhase, WorkerRowModel, active_worker_slots,
    phase_label,
};

const WORKER_NAME_WIDTH: usize = 36;
const PLAIN_PROGRESS_UPDATE_INTERVAL: Duration = Duration::from_millis(250);
const SPINNER_REDRAW_INTERVAL: Duration = Duration::from_millis(80);
const SUMMARY_FILE_PREVIEW_LIMIT: usize = 8;
const SUMMARY_FAILURE_PREVIEW_LIMIT: usize = 20;
const BRAILLE_SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
#[allow(dead_code)]
const HASH_BUFFER_SIZE: usize = 1024 * 1024;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileSignature {
    size: u64,
    xxh3_128: u128,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SourceSignatureKey {
    path: PathBuf,
    size: u64,
    mtime: Option<std::time::SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct SignedSource {
    key: SourceSignatureKey,
    signature: FileSignature,
}

#[allow(dead_code)]
type SourceSignatureCache = Arc<Mutex<HashMap<SourceSignatureKey, SourceSignatureEntry>>>;

#[allow(dead_code)]
#[derive(Debug, Clone)]
enum SourceSignatureEntry {
    Ready(SignedSource),
    Failed(String),
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct CompletedTransfer {
    source: PathBuf,
    dest: PathBuf,
    target: PathBuf,
    display_name: String,
    size: u64,
    expected: FileSignature,
    worker: usize,
    transfer_id: usize,
    bucket: SizeBucket,
}

#[derive(Debug, Clone)]
struct CopyExecutionContext {
    source_signature_cache: SourceSignatureCache,
    completed_tx: Sender<CompletedTransfer>,
    target_roots: Arc<Vec<PathBuf>>,
    next_transfer_id: Arc<AtomicUsize>,
}

#[derive(Debug)]
enum WorkerEvent {
    PhaseStarted {
        phase: PhaseKind,
        worker_count: usize,
    },
    Started {
        worker: usize,
        transfer_id: usize,
        bucket: SizeBucket,
        phase: TransferRowPhase,
        name: String,
        source: PathBuf,
        dest: PathBuf,
        total: u64,
    },
    PhaseChanged {
        worker: usize,
        phase: TransferRowPhase,
        total: u64,
    },
    Progress {
        worker: usize,
        copied: u64,
    },
    Finished {
        worker: usize,
        bucket: SizeBucket,
        name: String,
        source: PathBuf,
        dest: PathBuf,
        bytes: u64,
    },
    Verified {
        worker: usize,
        target: PathBuf,
        size: u64,
    },
    VerificationFailed {
        worker: usize,
        failure: CopyFailure,
    },
    Error {
        worker: usize,
        failure: CopyFailure,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
enum SizeBucket {
    Large,
    #[default]
    Small,
}

#[derive(Debug, Default)]
struct WorkerState {
    tag: String,
    label: String,
    target: String,
    phase: TransferRowPhase,
    copied: u64,
    total: u64,
    started: Option<Instant>,
    bucket: SizeBucket,
}

#[derive(Debug)]
struct ProgressState {
    completed: usize,
    task_count: usize,
    active_workers: usize,
    bytes_done: u64,
    bytes_total: u64,
    phase: PhaseKind,
    failed: bool,
    failed_count: usize,
    started: Instant,
}

#[derive(Debug, Clone)]
struct CopiedFileRecord {
    file: String,
    size: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct PhaseTotals {
    files: usize,
    bytes: u64,
}

#[derive(Debug, Clone, Default)]
struct TargetResult {
    planned: usize,
    planned_bytes: u64,
    copied: usize,
    copied_bytes: u64,
    verified: usize,
    verified_bytes: u64,
    copy_failed: usize,
    verify_failed: usize,
}

#[derive(Debug, Default)]
struct CopyReport {
    duration: Duration,
    bytes_done: u64,
    copied_files: Vec<CopiedFileRecord>,
    failures: Vec<CopyFailure>,
    large: PhaseTotals,
    small: PhaseTotals,
    target_results: BTreeMap<String, TargetResult>,
    failed: bool,
    systemic_detected: bool,
}

#[derive(Debug, Clone)]
struct RenderContext {
    job_name: String,
    target: PathBuf,
    target_count: usize,
    source_root: PathBuf,
    task_count: usize,
    total_bytes: u64,
    planning_stats: PlanningStats,
    target_roots: Arc<Vec<PathBuf>>,
    target_results: BTreeMap<String, TargetResult>,
}

impl CopyReport {
    fn record_copy(&mut self, bucket: SizeBucket, file: String, size: u64) {
        self.copied_files.push(CopiedFileRecord { file, size });
        let totals = match bucket {
            SizeBucket::Large => &mut self.large,
            SizeBucket::Small => &mut self.small,
        };
        totals.files += 1;
        totals.bytes += size;
    }

    fn record_failure(&mut self, failure: CopyFailure) {
        self.failed = true;
        self.systemic_detected |= failure.classification == CopyFailureClassification::Systemic;
        self.failures.push(failure);
    }

    fn record_target_copy(&mut self, target: &Path, bytes: u64) {
        let entry = self.target_entry_mut(target);
        entry.copied += 1;
        entry.copied_bytes += bytes;
    }

    fn record_target_verified(&mut self, target: &Path, bytes: u64) {
        let entry = self.target_entry_mut(target);
        entry.verified += 1;
        entry.verified_bytes += bytes;
    }

    fn record_target_copy_failure(&mut self, target: &Path) {
        self.target_entry_mut(target).copy_failed += 1;
    }

    fn record_target_verify_failure(&mut self, target: &Path) {
        self.target_entry_mut(target).verify_failed += 1;
    }

    fn target_entry_mut(&mut self, target: &Path) -> &mut TargetResult {
        self.target_results
            .entry(target_result_label(target))
            .or_default()
    }
}

impl ProgressState {
    fn new(task_count: usize, bytes_total: u64) -> Self {
        Self {
            completed: 0,
            task_count,
            active_workers: 0,
            bytes_done: 0,
            bytes_total,
            phase: PhaseKind::Adaptive,
            failed: false,
            failed_count: 0,
            started: Instant::now(),
        }
    }

    fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            completed: self.completed,
            task_count: self.task_count,
            active_workers: self.active_workers,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            elapsed: self.started.elapsed(),
            phase: self.phase,
            failed: self.failed,
        }
    }
}

pub fn print_dry_run(job: &ResolvedJob, plans: &[TransferPlan]) {
    let (large_files, small_files) = plan_breakdown(job, plans);
    println!(
        "dry run for job `{}`: {} file(s), {} total",
        job.name,
        plans.len(),
        human_bytes(plans.iter().map(|plan| plan.size).sum())
    );
    println!("transfer : {}", transfer_policy_label(&job.transfer_policy));
    if let TransferPolicy::Adaptive { .. } = job.transfer_policy {
        println!("buckets  : {} large, {} small", large_files, small_files);
    }

    for plan in plans {
        println!("{} -> {}", plan.source.display(), plan.dest.display());
    }
}

pub fn run_copy(
    job: &ResolvedJob,
    plans: Vec<TransferPlan>,
    planning_stats: PlanningStats,
) -> Result<(), CopyError> {
    let total_bytes: u64 = plans.iter().map(|plan| plan.size).sum();
    let task_count = plans.len();
    let large_file_count = count_large_files(job, &plans);
    let target_roots = Arc::new(job.targets.clone());
    let target_results = initial_target_results(&plans, &target_roots);
    let (event_tx, event_rx) = unbounded::<WorkerEvent>();
    let queue_capacity = std::cmp::max(16, job.parallel * 4);
    let (verify_tx, verify_rx) = bounded::<CompletedTransfer>(queue_capacity);
    let execution_context = CopyExecutionContext {
        source_signature_cache: SourceSignatureCache::default(),
        completed_tx: verify_tx.clone(),
        target_roots: target_roots.clone(),
        next_transfer_id: Arc::new(AtomicUsize::new(1)),
    };
    let source_root = job.source.clone();
    let job_name = job.name.clone();
    let target = job.primary_target().to_path_buf();
    let use_tty = io::stdout().is_terminal();
    let render_context = RenderContext {
        job_name,
        target,
        target_count: job.targets.len(),
        source_root,
        task_count,
        total_bytes,
        planning_stats,
        target_roots,
        target_results,
    };
    let ui_handle = if use_tty {
        thread::spawn(move || render_progress_tty(event_rx, render_context))
    } else {
        print_header_lines_plain(job, task_count, total_bytes, large_file_count);
        thread::spawn(move || render_progress_plain(event_rx, render_context))
    };
    let verify_parallel = std::cmp::min(job.targets.len().max(1), 2);
    let verifier_handles = start_verifier_workers(verify_parallel, verify_rx, event_tx.clone());

    match job.transfer_policy {
        TransferPolicy::Standard => {
            execute_phase(
                PhaseKind::SmallFiles,
                SizeBucket::Small,
                job.parallel,
                plans,
                event_tx.clone(),
                execution_context.clone(),
            );
        }
        TransferPolicy::Adaptive { .. } => {
            execute_adaptive(job, plans, event_tx.clone(), execution_context.clone());
        }
    }
    drop(execution_context);
    drop(verify_tx);
    join_verifier_workers(verifier_handles, &event_tx);
    drop(event_tx);

    ui_handle.join().map_err(|_| CopyError::UiThreadPanicked)?
}

fn start_verifier_workers(
    worker_count: usize,
    rx: Receiver<CompletedTransfer>,
    event_tx: Sender<WorkerEvent>,
) -> Vec<thread::JoinHandle<()>> {
    (0..worker_count)
        .map(|worker| {
            let worker_rx = rx.clone();
            let tx = event_tx.clone();
            thread::spawn(move || {
                verifier_loop(worker, worker_rx, tx);
            })
        })
        .collect()
}

fn verifier_loop(_worker: usize, rx: Receiver<CompletedTransfer>, tx: Sender<WorkerEvent>) {
    while let Ok(transfer) = rx.recv() {
        let _ = tx.send(WorkerEvent::Started {
            worker: transfer.worker,
            transfer_id: transfer.transfer_id,
            bucket: transfer.bucket,
            phase: TransferRowPhase::Verifying,
            name: transfer.display_name.clone(),
            source: transfer.source.clone(),
            dest: transfer.dest.clone(),
            total: transfer.size,
        });
        match verify_completed_transfer(&transfer, |done| {
            let _ = tx.send(WorkerEvent::Progress {
                worker: transfer.worker,
                copied: done,
            });
        }) {
            Ok(()) => {
                let _ = tx.send(WorkerEvent::Verified {
                    worker: transfer.worker,
                    target: transfer.target,
                    size: transfer.size,
                });
            }
            Err(failure) => {
                let _ = tx.send(WorkerEvent::VerificationFailed {
                    worker: transfer.worker,
                    failure,
                });
            }
        }
    }
}

fn join_verifier_workers(handles: Vec<thread::JoinHandle<()>>, event_tx: &Sender<WorkerEvent>) {
    for (worker, handle) in handles.into_iter().enumerate() {
        if handle.join().is_err() {
            let _ = event_tx.send(WorkerEvent::Error {
                worker: verifier_event_worker(worker),
                failure: panic_failure(worker, CopyOperation::WorkerPanic),
            });
        }
    }
}

fn verifier_event_worker(_worker: usize) -> usize {
    0
}

fn execute_phase(
    phase: PhaseKind,
    bucket: SizeBucket,
    configured_parallel: usize,
    plans: Vec<TransferPlan>,
    event_tx: Sender<WorkerEvent>,
    execution_context: CopyExecutionContext,
) {
    if plans.is_empty() {
        return;
    }

    let worker_count = active_worker_slots(configured_parallel, plans.len());
    let _ = event_tx.send(WorkerEvent::PhaseStarted {
        phase,
        worker_count,
    });

    let rx = receiver_from(plans);
    let mut handles = Vec::new();
    for worker in 0..worker_count {
        let worker_rx = rx.clone();
        let tx = event_tx.clone();
        let context = execution_context.clone();
        handles.push(thread::spawn(move || {
            worker_loop(worker, bucket, worker_rx, tx, context)
        }));
    }
    drop(rx);

    for (worker, handle) in handles.into_iter().enumerate() {
        if handle.join().is_err() {
            let _ = event_tx.send(WorkerEvent::Error {
                worker,
                failure: panic_failure(worker, CopyOperation::WorkerPanic),
            });
        }
    }
}

fn execute_adaptive(
    job: &ResolvedJob,
    plans: Vec<TransferPlan>,
    event_tx: Sender<WorkerEvent>,
    execution_context: CopyExecutionContext,
) {
    if plans.is_empty() {
        return;
    }

    let worker_count = active_worker_slots(job.parallel, plans.len());
    let _ = event_tx.send(WorkerEvent::PhaseStarted {
        phase: PhaseKind::Adaptive,
        worker_count,
    });

    let mut pending = sort_adaptive_plans(job, plans);
    let mut idle_workers: Vec<usize> = (0..worker_count).rev().collect();
    let mut active = Vec::<(usize, Option<ActiveLargePlan>, thread::JoinHandle<()>)>::new();
    let mut active_large_sources = HashMap::<PathBuf, ActiveLargeSource>::new();
    let mut active_large_targets = HashMap::<PathBuf, usize>::new();
    let mut target_lane_credits = HashSet::<PathBuf>::new();
    let mut active_slots = 0_usize;
    let (done_tx, done_rx) = unbounded::<usize>();

    while !pending.is_empty() || !active.is_empty() {
        while !idle_workers.is_empty() {
            let available_slots = job.parallel.saturating_sub(active_slots);
            let Some(index) = next_schedulable_index(
                &pending,
                &job.transfer_policy,
                available_slots,
                &active_large_sources,
                &active_large_targets,
                &target_lane_credits,
                &job.targets,
            ) else {
                break;
            };

            let plan = pending.remove(index);
            let worker = idle_workers.pop().expect("idle worker should exist");
            let bucket = bucket_for_plan(job, &plan);
            let (slot_cost, large_plan) = reserve_adaptive_slots(
                &job.transfer_policy,
                &plan,
                &mut active_large_sources,
                &mut active_large_targets,
                &mut target_lane_credits,
                &job.targets,
            );
            active_slots += slot_cost;

            let tx = event_tx.clone();
            let done = done_tx.clone();
            let context = execution_context.clone();
            let handle = thread::spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let transfer_id = context.next_transfer_id.fetch_add(1, Ordering::Relaxed);
                    run_plan(worker, transfer_id, bucket, plan, tx.clone(), context);
                }));
                if outcome.is_err() {
                    let _ = tx.send(WorkerEvent::Error {
                        worker,
                        failure: panic_failure(worker, CopyOperation::WorkerPanic),
                    });
                }
                let _ = done.send(worker);
            });
            active.push((worker, large_plan, handle));
        }

        if active.is_empty() {
            break;
        }

        let finished_worker = done_rx
            .recv()
            .expect("adaptive worker completion channel should stay open");
        if let Some(index) = active
            .iter()
            .position(|(worker, _, _)| *worker == finished_worker)
        {
            let (worker, large_plan, handle) = active.swap_remove(index);
            let _ = handle.join();
            let released_slots = release_adaptive_slots(
                large_plan,
                &mut active_large_sources,
                &mut active_large_targets,
                &mut target_lane_credits,
            );
            active_slots = active_slots.saturating_sub(released_slots);
            idle_workers.push(worker);
        }
    }
}

fn receiver_from(plans: Vec<TransferPlan>) -> Receiver<TransferPlan> {
    let (tx, rx) = unbounded();
    for plan in plans {
        tx.send(plan).expect("channel send should not fail");
    }
    rx
}

fn worker_loop(
    worker: usize,
    bucket: SizeBucket,
    rx: Receiver<TransferPlan>,
    tx: Sender<WorkerEvent>,
    execution_context: CopyExecutionContext,
) {
    while let Ok(plan) = rx.recv() {
        let transfer_id = execution_context
            .next_transfer_id
            .fetch_add(1, Ordering::Relaxed);
        run_plan(
            worker,
            transfer_id,
            bucket,
            plan,
            tx.clone(),
            execution_context.clone(),
        );
    }
}

fn run_plan(
    worker: usize,
    transfer_id: usize,
    bucket: SizeBucket,
    plan: TransferPlan,
    tx: Sender<WorkerEvent>,
    execution_context: CopyExecutionContext,
) {
    let started_name = plan.display_name.clone();
    let started_source = plan.source.clone();
    let started_dest = plan.dest.clone();
    let _ = tx.send(WorkerEvent::Started {
        worker,
        transfer_id,
        bucket,
        phase: if bucket == SizeBucket::Large {
            TransferRowPhase::Hashing
        } else {
            TransferRowPhase::Copying
        },
        name: started_name,
        source: started_source,
        dest: started_dest,
        total: plan.size,
    });

    let result = (|| -> Result<(u64, SignedSource), CopyFailure> {
        let signed_source = get_or_compute_source_signature(
            &execution_context.source_signature_cache,
            &plan,
            |_| {},
        )?;
        let _ = tx.send(WorkerEvent::PhaseChanged {
            worker,
            phase: TransferRowPhase::Copying,
            total: plan.size,
        });
        ensure_source_unchanged(&plan.source, &signed_source.key).map_err(|err| {
            copy_failure(
                &plan,
                CopyOperation::SourceChanged,
                err,
                format!("source changed during run: {}", plan.source.display()),
            )
        })?;

        let bytes = copy_file(&plan, worker, &tx)?;
        run_after_copy_test_hook();
        ensure_source_unchanged(&plan.source, &signed_source.key).map_err(|err| {
            let _ = fs::remove_file(&plan.dest);
            copy_failure(
                &plan,
                CopyOperation::SourceChanged,
                err,
                format!("source changed during run: {}", plan.source.display()),
            )
        })?;

        Ok((bytes, signed_source))
    })();

    match result {
        Ok((bytes, signed_source)) => {
            let target = target_root_for_plan(&plan, &execution_context.target_roots)
                .unwrap_or_else(|| plan.dest.clone());
            let _ = tx.send(WorkerEvent::Finished {
                worker,
                bucket,
                name: plan.display_name.clone(),
                source: plan.source.clone(),
                dest: plan.dest.clone(),
                bytes,
            });
            let _ = execution_context.completed_tx.send(CompletedTransfer {
                source: plan.source,
                dest: plan.dest,
                target,
                display_name: plan.display_name,
                size: plan.size,
                expected: signed_source.signature,
                worker,
                transfer_id,
                bucket,
            });
        }
        Err(failure) => {
            let _ = tx.send(WorkerEvent::Error { worker, failure });
        }
    }
}

fn copy_file(
    plan: &TransferPlan,
    worker: usize,
    tx: &Sender<WorkerEvent>,
) -> Result<u64, CopyFailure> {
    if let Some(parent) = plan.dest.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            copy_failure(
                plan,
                CopyOperation::CreateDir,
                err,
                format!("failed to create parent directory: {}", parent.display()),
            )
        })?;
    }

    let temp_dest = temp_path_for(&plan.dest);
    if temp_dest.exists() {
        fs::remove_file(&temp_dest).map_err(|err| {
            copy_failure(
                plan,
                CopyOperation::CleanupTemp,
                err,
                format!("failed to remove stale temp file: {}", temp_dest.display()),
            )
        })?;
    }

    let copy_result = (|| -> Result<u64, CopyFailure> {
        let transfer = copy_file_data(&plan.source, &temp_dest, |copied| {
            let _ = tx.send(WorkerEvent::Progress { worker, copied });
        })
        .map_err(|err| copy_transfer_failure(plan, err, &temp_dest))?;
        let metadata = transfer.metadata;

        fs::set_permissions(&temp_dest, metadata.permissions()).map_err(|err| {
            copy_failure(
                plan,
                CopyOperation::SetPermissions,
                err,
                format!("failed setting permissions on {}", temp_dest.display()),
            )
        })?;

        if let Ok(modified) = metadata.modified() {
            set_file_mtime(&temp_dest, FileTime::from_system_time(modified)).map_err(|err| {
                copy_failure(
                    plan,
                    CopyOperation::SetMtime,
                    err,
                    format!("failed setting mtime on {}", temp_dest.display()),
                )
            })?;
        }

        fs::rename(&temp_dest, &plan.dest).map_err(|err| {
            copy_failure(
                plan,
                CopyOperation::Rename,
                err,
                format!(
                    "failed to move temp file into place: {} -> {}",
                    temp_dest.display(),
                    plan.dest.display()
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

#[cfg(test)]
static AFTER_COPY_TEST_HOOK: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

#[cfg(test)]
fn set_after_copy_test_hook(hook: Box<dyn FnOnce() + Send>) {
    *AFTER_COPY_TEST_HOOK.lock().unwrap() = Some(hook);
}

#[cfg(test)]
fn run_after_copy_test_hook() {
    if let Some(hook) = AFTER_COPY_TEST_HOOK.lock().unwrap().take() {
        hook();
    }
}

#[cfg(not(test))]
fn run_after_copy_test_hook() {}

fn copy_failure(
    plan: &TransferPlan,
    operation: CopyOperation,
    err: io::Error,
    message: String,
) -> CopyFailure {
    let kind = err.kind();
    let raw_os_error = err.raw_os_error();
    CopyFailure {
        source: plan.source.clone(),
        dest: Some(plan.dest.clone()),
        operation,
        kind,
        raw_os_error,
        classification: classify_failure(kind, raw_os_error, operation),
        message,
    }
}

fn completed_transfer_failure(
    transfer: &CompletedTransfer,
    err: io::Error,
    message: String,
) -> CopyFailure {
    let kind = err.kind();
    let raw_os_error = err.raw_os_error();
    CopyFailure {
        source: transfer.source.clone(),
        dest: Some(transfer.dest.clone()),
        operation: CopyOperation::Verify,
        kind,
        raw_os_error,
        classification: classify_failure(kind, raw_os_error, CopyOperation::Verify),
        message,
    }
}

fn verify_completed_transfer<F>(
    transfer: &CompletedTransfer,
    progress: F,
) -> Result<(), CopyFailure>
where
    F: FnMut(u64),
{
    let metadata = fs::metadata(&transfer.dest).map_err(|err| {
        completed_transfer_failure(
            transfer,
            err,
            format!(
                "verification failed: could not stat destination {}",
                transfer.dest.display()
            ),
        )
    })?;
    let actual = hash_file_xxh3_128(&transfer.dest, metadata.len(), progress).map_err(|err| {
        completed_transfer_failure(
            transfer,
            err,
            format!(
                "verification failed: could not read destination {}",
                transfer.dest.display()
            ),
        )
    })?;

    if actual == transfer.expected {
        Ok(())
    } else {
        Err(completed_transfer_failure(
            transfer,
            io::Error::new(ErrorKind::InvalidData, "destination signature mismatch"),
            format!(
                "verification failed: signature mismatch for {}",
                transfer.dest.display()
            ),
        ))
    }
}

fn copy_transfer_failure(
    plan: &TransferPlan,
    error: CopyTransferError,
    temp_dest: &Path,
) -> CopyFailure {
    match error {
        CopyTransferError::UnsupportedNative { reason } => copy_failure(
            plan,
            CopyOperation::Read,
            io::Error::new(
                ErrorKind::Unsupported,
                format!("native copy unsupported: {reason:?}"),
            ),
            format!(
                "failed copying {} -> {}",
                plan.source.display(),
                temp_dest.display()
            ),
        ),
        CopyTransferError::Io { operation, source } => {
            let operation = match operation {
                CopyTransferOperation::StatSource | CopyTransferOperation::OpenSource => {
                    CopyOperation::OpenSource
                }
                CopyTransferOperation::CreateDestination => CopyOperation::CreateTemp,
                CopyTransferOperation::ReadSource => CopyOperation::Read,
                CopyTransferOperation::WriteDestination => CopyOperation::Write,
                CopyTransferOperation::FlushDestination => CopyOperation::Flush,
                CopyTransferOperation::NativeCopy => CopyOperation::Write,
            };

            let message = match operation {
                CopyOperation::OpenSource => {
                    format!("failed to open source file: {}", plan.source.display())
                }
                CopyOperation::CreateTemp => {
                    format!("failed to create temp file: {}", temp_dest.display())
                }
                CopyOperation::Read => format!("failed reading {}", plan.source.display()),
                CopyOperation::Write => {
                    format!(
                        "failed copying {} -> {}",
                        plan.source.display(),
                        temp_dest.display()
                    )
                }
                CopyOperation::Flush => format!("failed flushing {}", temp_dest.display()),
                _ => unreachable!("transfer failure maps only to data-path operations"),
            };

            copy_failure(plan, operation, source, message)
        }
    }
}

fn panic_failure(worker: usize, operation: CopyOperation) -> CopyFailure {
    CopyFailure {
        source: PathBuf::from(format!("<worker-{worker}>")),
        dest: None,
        operation,
        kind: ErrorKind::Other,
        raw_os_error: None,
        classification: CopyFailureClassification::Systemic,
        message: format!("worker-{worker} panicked"),
    }
}

fn classify_failure(
    kind: ErrorKind,
    raw_os_error: Option<i32>,
    operation: CopyOperation,
) -> CopyFailureClassification {
    if matches!(
        operation,
        CopyOperation::WorkerPanic | CopyOperation::UiPanic
    ) {
        return CopyFailureClassification::Systemic;
    }

    if matches!(
        kind,
        ErrorKind::StorageFull
            | ErrorKind::QuotaExceeded
            | ErrorKind::ReadOnlyFilesystem
            | ErrorKind::StaleNetworkFileHandle
    ) {
        return CopyFailureClassification::Systemic;
    }

    if matches!(raw_os_error, Some(5 | 6 | 19)) {
        return CopyFailureClassification::Systemic;
    }

    CopyFailureClassification::Local
}

fn temp_path_for(dest: &Path) -> PathBuf {
    dest.with_extension(format!(
        "{}.pathsync-part",
        dest.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("tmp")
    ))
}

#[allow(dead_code)]
fn source_signature_key(path: &Path) -> io::Result<SourceSignatureKey> {
    let metadata = fs::metadata(path)?;
    Ok(SourceSignatureKey {
        path: path.to_path_buf(),
        size: metadata.len(),
        mtime: metadata.modified().ok(),
        #[cfg(unix)]
        dev: {
            use std::os::unix::fs::MetadataExt;
            metadata.dev()
        },
        #[cfg(unix)]
        ino: {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        },
    })
}

#[allow(dead_code)]
fn ensure_source_unchanged(path: &Path, expected: &SourceSignatureKey) -> io::Result<()> {
    let actual = source_signature_key(path)?;
    if &actual == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            ErrorKind::InvalidData,
            "source changed during run",
        ))
    }
}

#[allow(dead_code)]
fn hash_file_xxh3_128<F>(path: &Path, size: u64, mut progress: F) -> io::Result<FileSignature>
where
    F: FnMut(u64),
{
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let mut buffer = [0_u8; HASH_BUFFER_SIZE];
    let mut done = 0_u64;

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        done += read as u64;
        progress(done);
    }

    Ok(FileSignature {
        size,
        xxh3_128: hasher.digest128(),
    })
}

#[allow(dead_code)]
fn get_or_compute_source_signature<F>(
    cache: &SourceSignatureCache,
    plan: &TransferPlan,
    mut progress: F,
) -> Result<SignedSource, CopyFailure>
where
    F: FnMut(u64),
{
    let key = source_signature_key(&plan.source).map_err(|err| {
        copy_failure(
            plan,
            CopyOperation::HashSource,
            err,
            format!(
                "failed hashing source {}: could not read source metadata",
                plan.source.display()
            ),
        )
    })?;

    if let Some(entry) = cache
        .lock()
        .expect("source signature cache lock should not be poisoned")
        .get(&key)
        .cloned()
    {
        return source_signature_entry_result(plan, entry);
    }

    let signature = match hash_file_xxh3_128(&plan.source, key.size, &mut progress) {
        Ok(signature) => signature,
        Err(err) => {
            let message = format!("failed hashing source {}", plan.source.display());
            let mut guard = cache
                .lock()
                .expect("source signature cache lock should not be poisoned");
            if let Some(entry) = guard.get(&key).cloned() {
                return source_signature_entry_result(plan, entry);
            }
            guard.insert(key, SourceSignatureEntry::Failed(message.clone()));
            return Err(copy_failure(plan, CopyOperation::HashSource, err, message));
        }
    };

    if let Err(err) = ensure_source_unchanged(&plan.source, &key) {
        let message = format!("source changed while hashing {}", plan.source.display());
        let mut guard = cache
            .lock()
            .expect("source signature cache lock should not be poisoned");
        if let Some(entry) = guard.get(&key).cloned() {
            return source_signature_entry_result(plan, entry);
        }
        guard.insert(key, SourceSignatureEntry::Failed(message.clone()));
        return Err(copy_failure(plan, CopyOperation::HashSource, err, message));
    }

    let signed_source = SignedSource {
        key: key.clone(),
        signature,
    };
    let mut guard = cache
        .lock()
        .expect("source signature cache lock should not be poisoned");
    if let Some(entry) = guard.get(&key).cloned() {
        return source_signature_entry_result(plan, entry);
    }
    guard.insert(key, SourceSignatureEntry::Ready(signed_source.clone()));

    Ok(signed_source)
}

#[allow(dead_code)]
fn source_signature_entry_result(
    plan: &TransferPlan,
    entry: SourceSignatureEntry,
) -> Result<SignedSource, CopyFailure> {
    match entry {
        SourceSignatureEntry::Ready(signed_source) => Ok(signed_source),
        SourceSignatureEntry::Failed(message) => Err(copy_failure(
            plan,
            CopyOperation::HashSource,
            io::Error::other(message.clone()),
            message,
        )),
    }
}

fn render_progress_tty(rx: Receiver<WorkerEvent>, context: RenderContext) -> Result<(), CopyError> {
    let term = Term::stdout();
    let glyphs = tty_glyph_set();
    let mut state = ProgressState::new(context.task_count, context.total_bytes);
    let mut report = CopyReport {
        target_results: context.target_results.clone(),
        ..CopyReport::default()
    };
    let mut permission_failures = 0_usize;
    let mut worker_states: Vec<WorkerState> = Vec::new();
    let mut last_line_count = 0_usize;

    loop {
        let should_redraw = match rx.recv_timeout(SPINNER_REDRAW_INTERVAL) {
            Ok(event) => match event {
                WorkerEvent::PhaseStarted {
                    phase,
                    worker_count,
                } => {
                    state.phase = phase;
                    worker_states = (0..worker_count).map(|_| WorkerState::default()).collect();
                    true
                }
                WorkerEvent::Started {
                    worker,
                    transfer_id,
                    bucket,
                    phase,
                    name,
                    source,
                    dest,
                    total,
                } => {
                    let label =
                        worker_label(&name, &source, &context.source_root, WORKER_NAME_WIDTH);
                    let target = target_volume_label(&dest);
                    let worker_state = &mut worker_states[worker];
                    worker_state.tag = worker_prefix(transfer_id.saturating_sub(1));
                    worker_state.bucket = bucket;
                    worker_state.phase = phase;
                    worker_state.label = label.clone();
                    worker_state.target = target;
                    worker_state.copied = 0;
                    worker_state.total = total;
                    worker_state.started = Some(Instant::now());
                    state.active_workers += 1;
                    true
                }
                WorkerEvent::PhaseChanged {
                    worker,
                    phase,
                    total,
                } => {
                    let worker_state = &mut worker_states[worker];
                    worker_state.phase = phase;
                    worker_state.copied = 0;
                    worker_state.total = total;
                    true
                }
                WorkerEvent::Progress { worker, copied } => {
                    let worker_state = &mut worker_states[worker];
                    worker_state.copied = copied;
                    true
                }
                WorkerEvent::Finished {
                    worker,
                    bucket,
                    name: _,
                    source,
                    dest,
                    bytes,
                } => {
                    let worker_state = &mut worker_states[worker];
                    worker_state.copied = 0;
                    worker_state.total = 0;
                    worker_state.label.clear();
                    worker_state.target.clear();
                    worker_state.started = None;
                    state.active_workers = state.active_workers.saturating_sub(1);
                    report.record_copy(
                        bucket,
                        relative_file_label(&context.source_root, &source),
                        bytes,
                    );
                    if let Some(target) = target_root_for_dest(&dest, &context.target_roots) {
                        report.record_target_copy(&target, bytes);
                    }
                    true
                }
                WorkerEvent::Verified {
                    worker,
                    target,
                    size,
                } => {
                    let worker_state = &mut worker_states[worker];
                    worker_state.copied = 0;
                    worker_state.total = 0;
                    worker_state.label.clear();
                    worker_state.target.clear();
                    worker_state.started = None;
                    state.active_workers = state.active_workers.saturating_sub(1);
                    state.bytes_done += size;
                    state.completed += 1;
                    report.record_target_verified(&target, size);
                    true
                }
                WorkerEvent::VerificationFailed {
                    worker,
                    mut failure,
                } => {
                    apply_failure_classification(&mut failure, &mut permission_failures);
                    let worker_state = &mut worker_states[worker];
                    worker_state.copied = 0;
                    worker_state.total = 0;
                    worker_state.label.clear();
                    worker_state.target.clear();
                    worker_state.started = None;
                    state.active_workers = state.active_workers.saturating_sub(1);
                    if let Some(target) = failure_target_root(&failure, &context.target_roots) {
                        report.record_target_verify_failure(&target);
                    }
                    state.failed = true;
                    state.failed_count += 1;
                    report.record_failure(failure.clone());
                    true
                }
                WorkerEvent::Error {
                    worker,
                    mut failure,
                } => {
                    apply_failure_classification(&mut failure, &mut permission_failures);
                    let worker_state = &mut worker_states[worker];
                    worker_state.copied = 0;
                    worker_state.total = 0;
                    worker_state.label.clear();
                    worker_state.target.clear();
                    worker_state.started = None;
                    state.active_workers = state.active_workers.saturating_sub(1);
                    state.failed = true;
                    state.failed_count += 1;
                    if let Some(target) = failure_target_root(&failure, &context.target_roots) {
                        report.record_target_copy_failure(&target);
                    }
                    report.record_failure(failure.clone());
                    true
                }
            },
            Err(RecvTimeoutError::Timeout) => state.active_workers > 0,
            Err(RecvTimeoutError::Disconnected) => break,
        };

        if should_redraw {
            let (_, columns) = term.size();
            let lines = render_live_screen_with_width_and_glyphs(
                &build_live_screen_model(&context, &state, &worker_states, &report, Instant::now()),
                usize::from(columns),
                glyphs,
            );
            draw_frame(&term, &lines, &mut last_line_count)?;
        }
    }

    report.duration = state.started.elapsed();
    report.bytes_done = state.bytes_done;
    report.failed = state.failed;
    let lines = render_post_run_screen_with_glyphs(
        &build_post_run_screen_model(
            &context,
            &report,
            context.planning_stats.skipped_existing_files,
            context.planning_stats.skipped_existing_bytes,
        ),
        glyphs,
    );
    draw_frame(&term, &lines, &mut last_line_count)?;

    if report.failures.is_empty() {
        Ok(())
    } else {
        Err(CopyError::RunFailed {
            failures_len: report.failures.len(),
            failures: report.failures.clone(),
            systemic_detected: report.systemic_detected,
        })
    }
}

fn tty_glyph_set() -> GlyphSet {
    match std::env::var("PATHSYNC_ASCII") {
        Ok(value) if matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES") => {
            GlyphSet::Ascii
        }
        _ => GlyphSet::Unicode,
    }
}

fn render_progress_plain(
    rx: Receiver<WorkerEvent>,
    context: RenderContext,
) -> Result<(), CopyError> {
    let mut state = ProgressState::new(context.task_count, context.total_bytes);
    let mut report = CopyReport {
        target_results: context.target_results.clone(),
        ..CopyReport::default()
    };
    let mut permission_failures = 0_usize;
    let mut worker_states: Vec<WorkerState> = Vec::new();
    let mut last_progress_line = Instant::now();

    for event in rx {
        match event {
            WorkerEvent::PhaseStarted {
                phase,
                worker_count,
            } => {
                state.phase = phase;
                worker_states = (0..worker_count).map(|_| WorkerState::default()).collect();
                println!("phase    : {}", phase_label(phase));
                println!("{}", plain_progress_line(&state.snapshot()));
                last_progress_line = Instant::now();
            }
            WorkerEvent::Started {
                worker,
                transfer_id,
                bucket,
                phase,
                name,
                source,
                dest,
                total: _total,
            } => {
                let label = worker_label(&name, &source, &context.source_root, WORKER_NAME_WIDTH);
                let target = target_volume_label(&dest);
                let worker_state = &mut worker_states[worker];
                worker_state.tag = worker_prefix(transfer_id.saturating_sub(1));
                worker_state.bucket = bucket;
                worker_state.phase = phase;
                worker_state.label = label.clone();
                worker_state.target = target;
                worker_state.copied = 0;
                worker_state.total = _total;
                worker_state.started = Some(Instant::now());
                state.active_workers += 1;
                println!(
                    "{}: {}",
                    worker_state.tag,
                    worker_line(&label, 0, Duration::ZERO)
                );
            }
            WorkerEvent::PhaseChanged {
                worker,
                phase,
                total,
            } => {
                let worker_state = &mut worker_states[worker];
                worker_state.phase = phase;
                worker_state.copied = 0;
                worker_state.total = total;
            }
            WorkerEvent::Progress { worker, copied } => {
                let worker_state = &mut worker_states[worker];
                worker_state.copied = copied;
                if last_progress_line.elapsed() >= PLAIN_PROGRESS_UPDATE_INTERVAL {
                    println!("{}", plain_progress_line(&state.snapshot()));
                    last_progress_line = Instant::now();
                }
            }
            WorkerEvent::Finished {
                worker,
                bucket,
                name,
                source,
                dest,
                bytes,
            } => {
                let label = current_worker_label(
                    &worker_states,
                    worker,
                    &name,
                    &source,
                    &context.source_root,
                );
                let worker_state = &mut worker_states[worker];
                let worker_tag = worker_state.tag.clone();
                worker_state.copied = 0;
                worker_state.label.clear();
                worker_state.target.clear();
                worker_state.started = None;
                state.active_workers = state.active_workers.saturating_sub(1);
                report.record_copy(
                    bucket,
                    relative_file_label(&context.source_root, &source),
                    bytes,
                );
                if let Some(target) = target_root_for_dest(&dest, &context.target_roots) {
                    report.record_target_copy(&target, bytes);
                }
                println!("{worker_tag}: done: {label}");
                println!("{}", plain_progress_line(&state.snapshot()));
                last_progress_line = Instant::now();
            }
            WorkerEvent::Verified {
                worker,
                target,
                size,
            } => {
                let worker_state = &mut worker_states[worker];
                worker_state.copied = 0;
                worker_state.label.clear();
                worker_state.target.clear();
                worker_state.started = None;
                state.active_workers = state.active_workers.saturating_sub(1);
                state.bytes_done += size;
                state.completed += 1;
                report.record_target_verified(&target, size);
                for line in
                    plain_target_progress_lines(&report, &worker_states, state.snapshot().elapsed)
                {
                    println!("{line}");
                }
            }
            WorkerEvent::VerificationFailed {
                worker,
                mut failure,
            } => {
                apply_failure_classification(&mut failure, &mut permission_failures);
                let worker_state = &mut worker_states[worker];
                worker_state.copied = 0;
                worker_state.label.clear();
                worker_state.target.clear();
                worker_state.started = None;
                state.active_workers = state.active_workers.saturating_sub(1);
                if let Some(target) = failure_target_root(&failure, &context.target_roots) {
                    report.record_target_verify_failure(&target);
                }
                state.failed = true;
                report.record_failure(failure.clone());
                println!("verify error: {}", failure.message);
                println!("{}", plain_progress_line(&state.snapshot()));
                last_progress_line = Instant::now();
            }
            WorkerEvent::Error {
                worker,
                mut failure,
            } => {
                apply_failure_classification(&mut failure, &mut permission_failures);
                let source = failure.source.clone();
                let name = source
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("<unknown>")
                    .to_string();
                let label = current_worker_label(
                    &worker_states,
                    worker,
                    &name,
                    &source,
                    &context.source_root,
                );
                let worker_state = &mut worker_states[worker];
                let worker_tag = worker_state.tag.clone();
                worker_state.copied = 0;
                worker_state.label.clear();
                worker_state.target.clear();
                worker_state.started = None;
                state.active_workers = state.active_workers.saturating_sub(1);
                state.failed = true;
                if let Some(target) = failure_target_root(&failure, &context.target_roots) {
                    report.record_target_copy_failure(&target);
                }
                report.record_failure(failure.clone());
                println!("{} error: {label}: {}", worker_tag, failure.message);
                println!("{}", plain_progress_line(&state.snapshot()));
                last_progress_line = Instant::now();
            }
        }
    }

    println!("{}", plain_progress_line(&state.snapshot()));
    report.duration = state.started.elapsed();
    report.bytes_done = state.bytes_done;
    report.failed = state.failed;
    for line in plain_target_progress_lines(&report, &worker_states, report.duration) {
        println!("{line}");
    }
    print_copy_report_plain(summary_lines(
        &context.job_name,
        &context.target,
        &context.source_root,
        &report,
        context.task_count,
        context.total_bytes,
        &context.target_roots,
    ));

    if report.failures.is_empty() {
        Ok(())
    } else {
        Err(CopyError::RunFailed {
            failures_len: report.failures.len(),
            failures: report.failures.clone(),
            systemic_detected: report.systemic_detected,
        })
    }
}

fn plain_target_progress_lines(
    report: &CopyReport,
    worker_states: &[WorkerState],
    elapsed: Duration,
) -> Vec<String> {
    build_target_progress_rows(report, worker_states, elapsed)
        .into_iter()
        .map(|target| {
            format!(
                "target {} | {} | rate {} | active {}",
                target.target, target.bytes, target.rate, target.active_workers
            )
        })
        .collect()
}

fn current_worker_label(
    worker_states: &[WorkerState],
    worker: usize,
    name: &str,
    source: &Path,
    source_root: &Path,
) -> String {
    worker_states
        .get(worker)
        .map(|state| state.label.as_str())
        .filter(|label| !label.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| worker_label(name, source, source_root, WORKER_NAME_WIDTH))
}

fn relative_file_label(source_root: &Path, source: &Path) -> String {
    source
        .strip_prefix(source_root)
        .ok()
        .and_then(|path| path.to_str())
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| {
            source
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("<unknown>")
        })
        .to_string()
}

fn target_volume_label(dest: &Path) -> String {
    let mut components = dest.components();
    if components.next() != Some(std::path::Component::RootDir) {
        return String::new();
    }

    let Some(std::path::Component::Normal(first)) = components.next() else {
        return String::new();
    };
    if first != "Volumes" {
        return String::new();
    }

    let Some(std::path::Component::Normal(volume)) = components.next() else {
        return String::new();
    };

    volume.to_string_lossy().into_owned()
}

fn target_result_label(target: &Path) -> String {
    let volume = target_volume_label(target);
    if !volume.is_empty() {
        return volume;
    }

    target
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| target.display().to_string())
}

fn initial_target_results(
    plans: &[TransferPlan],
    targets: &[PathBuf],
) -> BTreeMap<String, TargetResult> {
    let mut results = BTreeMap::new();
    for target in targets {
        results.insert(target_result_label(target), TargetResult::default());
    }

    for plan in plans {
        if let Some(target) = target_root_for_plan(plan, targets) {
            results
                .entry(target_result_label(&target))
                .or_default()
                .planned += 1;
            results
                .entry(target_result_label(&target))
                .or_default()
                .planned_bytes += plan.size;
        }
    }

    results
}

fn draw_frame(term: &Term, lines: &[String], last_line_count: &mut usize) -> Result<(), CopyError> {
    if *last_line_count > 0 {
        term.clear_last_lines(*last_line_count)
            .map_err(|err| CopyError::Internal {
                message: err.to_string(),
            })?;
    }

    for line in lines {
        term.write_line(line).map_err(|err| CopyError::Internal {
            message: err.to_string(),
        })?;
    }

    *last_line_count = lines.len();
    Ok(())
}

fn build_live_screen_model(
    context: &RenderContext,
    state: &ProgressState,
    worker_states: &[WorkerState],
    report: &CopyReport,
    render_now: Instant,
) -> LiveScreenModel {
    let snapshot = state.snapshot();
    let display_phase = display_phase(snapshot.phase, worker_states);
    let rate = if snapshot.elapsed.is_zero() {
        "--".to_string()
    } else {
        human_rate(snapshot.bytes_done, snapshot.elapsed)
    };
    let eta_value =
        crate::progress_model::eta(snapshot.bytes_done, snapshot.bytes_total, snapshot.elapsed)
            .map(format_duration)
            .unwrap_or_else(|| "--".to_string());
    let phase_text = match display_phase {
        PhaseKind::LargeFiles => "copying large files",
        PhaseKind::SmallFiles => "copying small files",
        PhaseKind::Adaptive => "copying files",
    };

    let workers = worker_states
        .iter()
        .enumerate()
        .map(|(worker, worker_state)| {
            if worker_state.label.is_empty() || worker_state.total == 0 {
                WorkerRowModel::idle(worker_prefix(worker))
            } else {
                let percent = progress_percent(worker_state.copied, worker_state.total);
                let elapsed = worker_state
                    .started
                    .map(|started| render_now.saturating_duration_since(started))
                    .unwrap_or(Duration::ZERO);
                let worker_rate = if worker_state.copied == 0 || elapsed.is_zero() {
                    "--".to_string()
                } else {
                    human_rate(worker_state.copied, elapsed)
                };
                WorkerRowModel::active_with_phase(
                    worker_spinner_frame(worker_state.started, worker, render_now),
                    if worker_state.tag.is_empty() {
                        worker_prefix(worker)
                    } else {
                        worker_state.tag.clone()
                    },
                    worker_state.phase,
                    percent,
                    worker_state.label.clone(),
                    human_bytes(worker_state.total),
                    worker_rate,
                    worker_state.target.clone(),
                )
            }
        })
        .collect();

    LiveScreenModel {
        job_name: context.job_name.clone(),
        status: match display_phase {
            PhaseKind::LargeFiles => "LIVE / COPY-LARGE".to_string(),
            PhaseKind::SmallFiles => "LIVE / COPY-SMALL".to_string(),
            PhaseKind::Adaptive => "LIVE / COPY".to_string(),
        },
        summary: vec![
            SummaryMetric::new(
                "Scanned",
                format_count(context.planning_stats.scanned_files),
            ),
            SummaryMetric::new(
                "Planned",
                format_count(context.planning_stats.planned_files),
            ),
            SummaryMetric::new("Copied", format_count(snapshot.completed)),
            SummaryMetric::new("Verified", format_count(snapshot.completed)),
            SummaryMetric::new("Failed", format_count(report.failures.len())),
            SummaryMetric::new(
                "Bytes",
                format!(
                    "{} / {}",
                    human_bytes(snapshot.bytes_done),
                    human_bytes(snapshot.bytes_total)
                ),
            ),
            SummaryMetric::new("Rate", rate),
            SummaryMetric::new("Elapsed", format_duration(snapshot.elapsed)),
            SummaryMetric::new("ETA", eta_value.clone()),
            SummaryMetric::new("Targets", format_count(context.target_count)),
        ],
        overall_label: "Copying".to_string(),
        overall_progress: ProgressBarModel::new(
            progress_percent(snapshot.bytes_done, snapshot.bytes_total),
            30,
        ),
        overall_progress_text: format!(
            "{} verified of {}   ETA {}",
            human_bytes(snapshot.bytes_done),
            human_bytes(snapshot.bytes_total),
            eta_value
        ),
        phase_label: format!("overall  {phase_text}"),
        workers,
        target_progress: build_target_progress_rows(report, worker_states, snapshot.elapsed),
    }
}

fn build_target_progress_rows(
    report: &CopyReport,
    worker_states: &[WorkerState],
    elapsed: Duration,
) -> Vec<TargetProgressRowModel> {
    report
        .target_results
        .iter()
        .map(|(target, result)| {
            let active_workers = worker_states
                .iter()
                .filter(|worker| !worker.label.is_empty() && worker.target == *target)
                .count();
            TargetProgressRowModel::new(
                target,
                progress_percent(result.verified_bytes, result.planned_bytes),
                format!(
                    "{} / {}",
                    human_bytes(result.verified_bytes),
                    human_bytes(result.planned_bytes)
                ),
                if result.verified_bytes == 0 || elapsed.is_zero() {
                    "--".to_string()
                } else {
                    human_rate(result.verified_bytes, elapsed)
                },
                active_workers,
            )
        })
        .collect()
}

fn braille_spinner_frame(spinner_tick: usize) -> char {
    BRAILLE_SPINNER_FRAMES[spinner_tick % BRAILLE_SPINNER_FRAMES.len()]
}

fn worker_spinner_frame(started: Option<Instant>, worker: usize, render_now: Instant) -> char {
    let started = started.unwrap_or(render_now);
    let elapsed = render_now.saturating_duration_since(started);
    let frame = (elapsed.as_millis() / SPINNER_REDRAW_INTERVAL.as_millis()) as usize + worker;
    braille_spinner_frame(frame)
}

fn display_phase(snapshot_phase: PhaseKind, worker_states: &[WorkerState]) -> PhaseKind {
    if snapshot_phase != PhaseKind::Adaptive {
        return snapshot_phase;
    }

    let active_bucket = worker_states
        .iter()
        .find(|worker| !worker.label.is_empty() && worker.total > 0)
        .map(|worker| worker.bucket);

    match active_bucket {
        Some(SizeBucket::Large) => PhaseKind::LargeFiles,
        Some(SizeBucket::Small) => PhaseKind::SmallFiles,
        None => PhaseKind::Adaptive,
    }
}

fn build_post_run_screen_model(
    context: &RenderContext,
    report: &CopyReport,
    skipped_existing_files: usize,
    skipped_existing_bytes: u64,
) -> PostRunScreenModel {
    let copied_count = report.copied_files.len();
    let copied_bytes = report.bytes_done;
    let verified_count: usize = report
        .target_results
        .values()
        .map(|result| result.verified)
        .sum();
    let verified_bytes: u64 = report
        .target_results
        .values()
        .map(|result| result.verified_bytes)
        .sum();
    let skipped_rate = if context.planning_stats.scanned_files == 0 {
        "0.0%".to_string()
    } else {
        format!(
            "{:.1}%",
            (skipped_existing_files as f64 / context.planning_stats.scanned_files as f64) * 100.0
        )
    };

    let mut copied_mp4 = (0_usize, 0_u64);
    let mut copied_jpg = (0_usize, 0_u64);
    for file in &report.copied_files {
        let lower = file.file.to_ascii_lowercase();
        if lower.ends_with(".mp4") {
            copied_mp4.0 += 1;
            copied_mp4.1 += file.size;
        } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
            copied_jpg.0 += 1;
            copied_jpg.1 += file.size;
        }
    }

    let failed_permission = report
        .failures
        .iter()
        .filter(|failure| failure.kind == ErrorKind::PermissionDenied)
        .count();
    let failed_collision = report
        .failures
        .iter()
        .filter(|failure| single_line_error(&failure.message).contains("collision"))
        .count();

    let mut categories = vec![CategoryRowModel::new(
        TransferCategory::SkippedExisting.as_label(),
        skipped_existing_files,
        human_bytes(skipped_existing_bytes),
        skipped_rate.clone(),
        "0.0s",
    )];

    if copied_mp4.0 > 0 {
        categories.push(CategoryRowModel::new(
            TransferCategory::CopiedMp4.as_label(),
            copied_mp4.0,
            human_bytes(copied_mp4.1),
            percent_string(copied_mp4.1, context.total_bytes),
            format_duration(report.duration),
        ));
    }
    if copied_jpg.0 > 0 {
        categories.push(CategoryRowModel::new(
            TransferCategory::CopiedJpg.as_label(),
            copied_jpg.0,
            human_bytes(copied_jpg.1),
            percent_string(copied_jpg.1, context.total_bytes),
            format_duration(report.duration),
        ));
    }
    if failed_permission > 0 {
        categories.push(CategoryRowModel::new(
            TransferCategory::FailedPermission.as_label(),
            failed_permission,
            "0 B",
            "0.0%",
            "--",
        ));
    }
    if failed_collision > 0 {
        categories.push(CategoryRowModel::new(
            TransferCategory::FailedCollision.as_label(),
            failed_collision,
            "0 B",
            "0.0%",
            "--",
        ));
    }

    let errors = report
        .failures
        .iter()
        .map(|failure| {
            let target = failure_target_root(failure, &context.target_roots)
                .map(|target| target_result_label(&target))
                .or_else(|| failure.dest.as_deref().map(target_result_label))
                .unwrap_or_else(|| "--".to_string());
            ErrorRowModel::new(
                target,
                failure.operation.to_string(),
                failure
                    .source
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("<unknown>"),
                single_line_error(&failure.message),
            )
        })
        .collect();
    let target_results = report
        .target_results
        .iter()
        .map(|(target, result)| {
            TargetResultRowModel::new(
                target,
                result.planned,
                result.copied,
                result.verified,
                result.copy_failed,
                result.verify_failed,
            )
        })
        .collect();

    PostRunScreenModel {
        job_name: context.job_name.clone(),
        status: if report.systemic_detected {
            "FAILED".to_string()
        } else if report.failed {
            "ATTENTION".to_string()
        } else {
            "VERIFIED".to_string()
        },
        summary: vec![
            SummaryMetric::new(
                "Scanned",
                format_count(context.planning_stats.scanned_files),
            ),
            SummaryMetric::new(
                "Planned",
                format_count(context.planning_stats.planned_files),
            ),
            SummaryMetric::new("Copied", format_count(copied_count)),
            SummaryMetric::new("Verified", format_count(verified_count)),
            SummaryMetric::new("Failed", format_count(report.failures.len())),
            SummaryMetric::new(
                "Bytes",
                format!(
                    "{} / {}",
                    human_bytes(verified_bytes),
                    human_bytes(context.total_bytes)
                ),
            ),
            SummaryMetric::new("Rate", average_rate(copied_bytes, report.duration)),
            SummaryMetric::new("Elapsed", format_duration(report.duration)),
            SummaryMetric::new("ETA", "--"),
            SummaryMetric::new("Targets", format_count(context.target_count)),
        ],
        completion_label: "Verified".to_string(),
        completion_progress: ProgressBarModel::new(
            progress_percent(verified_bytes, context.total_bytes),
            30,
        ),
        categories,
        target_results,
        errors,
        copied_preview_count: report.copied_files.len().min(SUMMARY_FILE_PREVIEW_LIMIT),
        copied_preview_total: report.copied_files.len(),
    }
}

fn progress_percent(done: u64, total: u64) -> usize {
    done.min(total)
        .checked_mul(100)
        .and_then(|percent| percent.checked_div(total))
        .unwrap_or(0) as usize
}

fn percent_string(done: u64, total: u64) -> String {
    if total == 0 {
        "0.0%".to_string()
    } else {
        format!("{:.1}%", (done as f64 / total as f64) * 100.0)
    }
}

fn format_count(value: usize) -> String {
    let digits = value.to_string();
    let mut result = String::new();
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

fn print_copy_report_plain(lines: Vec<String>) {
    for line in lines {
        println!("{line}");
    }
}

fn summary_lines(
    job_name: &str,
    target: &Path,
    source_root: &Path,
    report: &CopyReport,
    task_count: usize,
    total_bytes: u64,
    target_roots: &[PathBuf],
) -> Vec<String> {
    let title = if report.systemic_detected {
        "FAILED"
    } else if report.failed {
        "ATTENTION"
    } else {
        "VERIFIED"
    };
    let copied_bytes: u64 = report.copied_files.iter().map(|file| file.size).sum();
    let verified_count: usize = report
        .target_results
        .values()
        .map(|result| result.verified)
        .sum();
    let verified_bytes: u64 = report
        .target_results
        .values()
        .map(|result| result.verified_bytes)
        .sum();
    let avg_rate = average_rate(report.bytes_done, report.duration);
    let mut lines = vec![
        String::new(),
        main_header(title),
        String::new(),
        format!(
            "verified | {} / {} | rate {} | elapsed {}",
            human_bytes(verified_bytes),
            human_bytes(total_bytes),
            avg_rate,
            format_duration(report.duration)
        ),
        String::new(),
        "Target Results".to_string(),
        section_divider(),
        format!(
            "{:<22} {:>7} {:>7} {:>8} {:>9} {:>11}   {}",
            "Target", "Planned", "Copied", "Verified", "CopyFail", "VerifyFail", "Result"
        ),
    ];

    for (target, result) in &report.target_results {
        let result_label = if result.copy_failed == 0
            && result.verify_failed == 0
            && result.planned == result.copied
            && result.planned == result.verified
        {
            "verified"
        } else {
            "attention"
        };
        lines.push(format!(
            "{:<22} {:>7} {:>7} {:>8} {:>9} {:>11}   {}",
            truncate_right(target, 22),
            result.planned,
            result.copied,
            result.verified,
            result.copy_failed,
            result.verify_failed,
            result_label
        ));
    }

    if !report.failures.is_empty() {
        lines.push(String::new());
        lines.push("Failures".to_string());
        lines.push(section_divider());
        lines.push(format!(
            "{:<18} {:<8} {:<28} {}",
            "Target", "Phase", "File", "Error"
        ));
        for failure in report.failures.iter().take(SUMMARY_FAILURE_PREVIEW_LIMIT) {
            let target = failure
                .dest
                .as_deref()
                .and_then(|dest| target_root_for_dest(dest, target_roots))
                .map(|target| target_result_label(&target))
                .or_else(|| failure.dest.as_deref().map(target_result_label))
                .unwrap_or_else(|| "--".to_string());
            lines.push(format!(
                "{:<18} {:<8} {:<28} {}",
                truncate_right(&target, 18),
                failure_phase_label(failure),
                truncate_right(&relative_file_label(source_root, &failure.source), 28),
                single_line_error(&failure.message)
            ));
        }
        if report.failures.len() > SUMMARY_FAILURE_PREVIEW_LIMIT {
            lines.push(String::new());
            lines.push(format!(
                "showing {} of {} failures",
                SUMMARY_FAILURE_PREVIEW_LIMIT,
                report.failures.len()
            ));
        }
    }

    lines.extend([
        String::new(),
        "Run".to_string(),
        section_divider(),
        summary_row("Job", job_name),
        summary_row("Target", &target.display().to_string()),
        summary_row("Result", title),
        summary_row("Duration", &format_duration(report.duration)),
        summary_row("Avg Rate", &avg_rate),
        summary_row(
            "Systemic",
            if report.systemic_detected {
                "yes"
            } else {
                "no"
            },
        ),
        String::new(),
        "Breakdown".to_string(),
        section_divider(),
        count_row("Copied", report.copied_files.len(), copied_bytes),
        count_row("Verified", verified_count, verified_bytes),
        count_row("Planned", task_count, total_bytes),
        count_row("Failed", report.failures.len(), 0),
        count_row("Large", report.large.files, report.large.bytes),
        count_row("Small", report.small.files, report.small.bytes),
    ]);

    if !report.copied_files.is_empty() {
        lines.push(String::new());
        lines.push("Copied file preview".to_string());
        lines.push(section_divider());
        lines.push(format!("{:<3} {:<44} {:>10}", "#", "File", "Size"));
        for (index, file) in report
            .copied_files
            .iter()
            .take(SUMMARY_FILE_PREVIEW_LIMIT)
            .enumerate()
        {
            lines.push(format!(
                "{:<3} {:<44} {:>10}",
                index + 1,
                truncate_right(&file.file, 44),
                human_bytes(file.size)
            ));
        }
        if report.copied_files.len() > SUMMARY_FILE_PREVIEW_LIMIT {
            lines.push(String::new());
            lines.push(format!(
                "showing {} of {} copied files",
                SUMMARY_FILE_PREVIEW_LIMIT,
                report.copied_files.len()
            ));
        }
    }

    lines
}

fn failure_phase_label(failure: &CopyFailure) -> &'static str {
    match failure.operation {
        CopyOperation::Verify => "verify",
        CopyOperation::HashSource | CopyOperation::SourceChanged => "hash",
        _ => "copy",
    }
}

fn main_header(title: &str) -> String {
    format!("============================== {title} ==============================")
}

fn section_divider() -> String {
    "------------------------------------------------------------------------".to_string()
}

fn summary_row(label: &str, value: &str) -> String {
    format!("{label:<12} {value}")
}

fn count_row(label: &str, files: usize, bytes: u64) -> String {
    format!("{label:<12} {files:>3} files   {:>10}", human_bytes(bytes))
}

fn average_rate(bytes: u64, duration: Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds <= 0.0 {
        return "0 B/s".to_string();
    }
    human_rate(bytes, duration)
}

fn single_line_error(message: &str) -> String {
    message.lines().next().unwrap_or(message).to_string()
}

fn truncate_right(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max_chars {
        return value.to_string();
    }

    if max_chars <= 1 {
        return "…".to_string();
    }

    let mut result: String = chars[..max_chars - 1].iter().collect();
    result.push('…');
    result
}

fn print_header_lines_plain(
    job: &ResolvedJob,
    task_count: usize,
    total_bytes: u64,
    large_file_count: usize,
) {
    for line in header_lines(job, task_count, total_bytes, large_file_count) {
        println!("{line}");
    }
}

fn header_lines(
    job: &ResolvedJob,
    task_count: usize,
    total_bytes: u64,
    large_file_count: usize,
) -> Vec<String> {
    vec![
        format!("job      : {}", job.name),
        format!("source   : {}", job.source.display()),
        format!("target   : {}", job.primary_target().display()),
        format!("layout   : {}", job.template),
        format!("transfer : {}", transfer_policy_label(&job.transfer_policy)),
        format!("parallel : {}", job.parallel),
        format!("filters  : {}", job.extensions.join(", ")),
        format!(
            "pending  : {} file(s), {}",
            task_count,
            human_bytes(total_bytes)
        ),
        format!("large    : {} file(s)", large_file_count),
        String::new(),
    ]
}

fn transfer_policy_label(policy: &TransferPolicy) -> String {
    match policy {
        TransferPolicy::Standard => "standard".to_string(),
        TransferPolicy::Adaptive {
            large_file_threshold_bytes,
            large_file_slots,
            max_large_per_target,
        } => format!(
            "adaptive (large >= {}, slots {}, max/target {})",
            human_bytes(*large_file_threshold_bytes),
            large_file_slots,
            max_large_per_target
        ),
    }
}

fn is_large_file(job: &ResolvedJob, plan: &TransferPlan) -> bool {
    match job.transfer_policy {
        TransferPolicy::Standard => false,
        TransferPolicy::Adaptive {
            large_file_threshold_bytes,
            ..
        } => plan.size >= large_file_threshold_bytes,
    }
}

fn bucket_for_plan(job: &ResolvedJob, plan: &TransferPlan) -> SizeBucket {
    if is_large_file(job, plan) {
        SizeBucket::Large
    } else {
        SizeBucket::Small
    }
}

fn slot_cost(policy: &TransferPolicy, plan: &TransferPlan) -> usize {
    match policy {
        TransferPolicy::Standard => 1,
        TransferPolicy::Adaptive {
            large_file_threshold_bytes,
            large_file_slots,
            ..
        } => {
            if plan.size >= *large_file_threshold_bytes {
                *large_file_slots
            } else {
                1
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveLargeSource {
    active_count: usize,
    held_slots: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveLargePlan {
    source: PathBuf,
    target: PathBuf,
}

fn adaptive_slot_cost(
    policy: &TransferPolicy,
    plan: &TransferPlan,
    active_large_sources: &HashMap<PathBuf, ActiveLargeSource>,
    active_large_targets: &HashMap<PathBuf, usize>,
    target_lane_credits: &HashSet<PathBuf>,
    targets: &[PathBuf],
) -> usize {
    if is_large_plan(policy, plan)
        && target_root_for_plan(plan, targets)
            .as_ref()
            .is_some_and(|target| {
                target_large_capacity_remaining(policy, target, active_large_targets) == 0
            })
    {
        usize::MAX
    } else if is_large_plan(policy, plan) && active_large_sources.contains_key(&plan.source) {
        0
    } else if is_large_plan(policy, plan)
        && target_root_for_plan(plan, targets)
            .as_ref()
            .is_some_and(|target| target_lane_credits.contains(target))
    {
        1
    } else {
        slot_cost(policy, plan)
    }
}

fn reserve_adaptive_slots(
    policy: &TransferPolicy,
    plan: &TransferPlan,
    active_large_sources: &mut HashMap<PathBuf, ActiveLargeSource>,
    active_large_targets: &mut HashMap<PathBuf, usize>,
    target_lane_credits: &mut HashSet<PathBuf>,
    targets: &[PathBuf],
) -> (usize, Option<ActiveLargePlan>) {
    if !is_large_plan(policy, plan) {
        return (1, None);
    }

    let target = target_root_for_plan(plan, targets).unwrap_or_else(|| plan.dest.clone());
    *active_large_targets.entry(target.clone()).or_default() += 1;
    if let Some(active_source) = active_large_sources.get_mut(&plan.source) {
        active_source.active_count += 1;
        return (
            0,
            Some(ActiveLargePlan {
                source: plan.source.clone(),
                target,
            }),
        );
    }

    let cost = if target_lane_credits.remove(&target) {
        1
    } else {
        slot_cost(policy, plan)
    };
    active_large_sources.insert(
        plan.source.clone(),
        ActiveLargeSource {
            active_count: 1,
            held_slots: cost,
        },
    );
    (
        cost,
        Some(ActiveLargePlan {
            source: plan.source.clone(),
            target,
        }),
    )
}

fn release_adaptive_slots(
    large_plan: Option<ActiveLargePlan>,
    active_large_sources: &mut HashMap<PathBuf, ActiveLargeSource>,
    active_large_targets: &mut HashMap<PathBuf, usize>,
    target_lane_credits: &mut HashSet<PathBuf>,
) -> usize {
    let Some(large_plan) = large_plan else {
        return 1;
    };
    let source = large_plan.source;
    decrement_active_large_target(active_large_targets, &large_plan.target);

    let Some(active_source) = active_large_sources.get_mut(&source) else {
        return 0;
    };

    if active_source.active_count > 1 {
        active_source.active_count -= 1;
        if active_source.held_slots > 1 {
            active_source.held_slots -= 1;
            target_lane_credits.insert(large_plan.target);
            1
        } else {
            0
        }
    } else if active_source.active_count == 0 {
        active_large_sources.remove(&source);
        0
    } else {
        active_large_sources
            .remove(&source)
            .map(|active_source| active_source.held_slots)
            .unwrap_or(0)
    }
}

fn count_large_files(job: &ResolvedJob, plans: &[TransferPlan]) -> usize {
    plans.iter().filter(|plan| is_large_file(job, plan)).count()
}

fn plan_breakdown(job: &ResolvedJob, plans: &[TransferPlan]) -> (usize, usize) {
    let large = count_large_files(job, plans);
    (large, plans.len().saturating_sub(large))
}

fn sort_adaptive_plans(job: &ResolvedJob, mut plans: Vec<TransferPlan>) -> Vec<TransferPlan> {
    plans.sort_by(|a, b| {
        bucket_for_plan(job, a)
            .cmp(&bucket_for_plan(job, b))
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.dest.cmp(&b.dest))
    });
    plans
}

fn next_schedulable_index(
    pending: &[TransferPlan],
    policy: &TransferPolicy,
    available_slots: usize,
    active_large_sources: &HashMap<PathBuf, ActiveLargeSource>,
    active_large_targets: &HashMap<PathBuf, usize>,
    target_lane_credits: &HashSet<PathBuf>,
    targets: &[PathBuf],
) -> Option<usize> {
    pending.iter().position(|plan| {
        adaptive_slot_cost(
            policy,
            plan,
            active_large_sources,
            active_large_targets,
            target_lane_credits,
            targets,
        ) <= available_slots
    })
}

fn target_large_capacity_remaining(
    policy: &TransferPolicy,
    target: &Path,
    active_large_targets: &HashMap<PathBuf, usize>,
) -> usize {
    let TransferPolicy::Adaptive {
        max_large_per_target,
        ..
    } = policy
    else {
        return usize::MAX;
    };
    max_large_per_target.saturating_sub(*active_large_targets.get(target).unwrap_or(&0))
}

fn decrement_active_large_target(
    active_large_targets: &mut HashMap<PathBuf, usize>,
    target: &Path,
) {
    if let Some(active_count) = active_large_targets.get_mut(target) {
        *active_count = active_count.saturating_sub(1);
        if *active_count == 0 {
            active_large_targets.remove(target);
        }
    }
}

fn is_large_plan(policy: &TransferPolicy, plan: &TransferPlan) -> bool {
    match policy {
        TransferPolicy::Standard => false,
        TransferPolicy::Adaptive {
            large_file_threshold_bytes,
            ..
        } => plan.size >= *large_file_threshold_bytes,
    }
}

fn target_root_for_plan(plan: &TransferPlan, targets: &[PathBuf]) -> Option<PathBuf> {
    target_root_for_dest(&plan.dest, targets)
}

fn target_root_for_dest(dest: &Path, targets: &[PathBuf]) -> Option<PathBuf> {
    targets
        .iter()
        .find(|target| dest.starts_with(target))
        .cloned()
}

fn failure_target_root(failure: &CopyFailure, targets: &[PathBuf]) -> Option<PathBuf> {
    failure
        .dest
        .as_deref()
        .and_then(|dest| target_root_for_dest(dest, targets))
}

fn apply_failure_classification(failure: &mut CopyFailure, permission_failures: &mut usize) {
    if failure.kind == ErrorKind::PermissionDenied {
        *permission_failures += 1;
        if *permission_failures > 3 {
            failure.classification = CopyFailureClassification::Systemic;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("{name}-{unique}"));
            fs::create_dir_all(&path).unwrap();
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

    fn write_test_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = File::create(path).unwrap();
        file.write_all(contents).unwrap();
    }

    fn completed_transfer_for(
        source: PathBuf,
        dest: PathBuf,
        target: PathBuf,
        expected_payload: &[u8],
    ) -> CompletedTransfer {
        CompletedTransfer {
            source,
            dest,
            target,
            display_name: "photo.jpg".to_string(),
            size: expected_payload.len() as u64,
            expected: FileSignature {
                size: expected_payload.len() as u64,
                xxh3_128: xxhash_rust::xxh3::xxh3_128(expected_payload),
            },
            worker: 0,
            transfer_id: 1,
            bucket: SizeBucket::Small,
        }
    }

    #[test]
    fn verify_completed_transfer_accepts_matching_destination_signature() {
        let temp = TempDir::new("pathsync-verify-completed-transfer-match");
        let source = temp.path().join("source/photo.jpg");
        let target = temp.path().join("target");
        let dest = target.join("photo.jpg");
        let payload = b"destination bytes";
        write_test_file(&source, payload);
        write_test_file(&dest, payload);
        let transfer = completed_transfer_for(source, dest, target, payload);
        let mut progress = Vec::new();

        verify_completed_transfer(&transfer, |done| progress.push(done)).unwrap();

        assert_eq!(progress.last().copied(), Some(payload.len() as u64));
    }

    #[test]
    fn verify_completed_transfer_rejects_mismatched_destination_signature() {
        let temp = TempDir::new("pathsync-verify-completed-transfer-mismatch");
        let source = temp.path().join("source/photo.jpg");
        let target = temp.path().join("target");
        let dest = target.join("photo.jpg");
        let expected = b"expected destination bytes";
        write_test_file(&source, expected);
        write_test_file(&dest, b"corrupt destination bytes");
        let transfer = completed_transfer_for(source, dest, target, expected);

        let failure = verify_completed_transfer(&transfer, |_| {}).unwrap_err();

        assert_eq!(failure.operation, CopyOperation::Verify);
        assert_eq!(failure.kind, ErrorKind::InvalidData);
        assert!(failure.message.contains("signature mismatch"));
    }

    fn plan(name: &str, size: u64) -> TransferPlan {
        TransferPlan {
            source: PathBuf::from(format!("/source/{name}")),
            dest: PathBuf::from(format!("/target/{name}")),
            size,
            display_name: name.to_string(),
        }
    }

    #[test]
    fn source_changed_detection_returns_invalid_data_after_file_length_changes() {
        let temp = TempDir::new("pathsync-source-changed");
        let source = temp.path().join("photo.jpg");
        write_test_file(&source, b"before");
        let key = source_signature_key(&source).unwrap();

        write_test_file(&source, b"after-with-a-different-length");

        let error = ensure_source_unchanged(&source, &key).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn hash_file_xxh3_128_returns_signature_and_reports_cumulative_progress() {
        let temp = TempDir::new("pathsync-hash-source");
        let source = temp.path().join("photo.jpg");
        let payload = b"small source bytes";
        write_test_file(&source, payload);

        let final_progress = AtomicU64::new(0);
        let signature = hash_file_xxh3_128(&source, payload.len() as u64, |done| {
            final_progress.store(done, Ordering::SeqCst);
        })
        .unwrap();

        assert_eq!(signature.size, payload.len() as u64);
        assert_eq!(signature.xxh3_128, xxhash_rust::xxh3::xxh3_128(payload));
        assert_eq!(final_progress.load(Ordering::SeqCst), payload.len() as u64);
    }

    #[test]
    fn source_signature_cache_reuses_ready_signature() {
        let temp = TempDir::new("pathsync-source-signature-cache");
        let source = temp.path().join("photo.jpg");
        let dest_a = temp.path().join("target-a/photo.jpg");
        let dest_b = temp.path().join("target-b/photo.jpg");
        let payload = b"shared source bytes";
        write_test_file(&source, payload);

        let cache = SourceSignatureCache::default();
        let plan_a = TransferPlan {
            source: source.clone(),
            dest: dest_a,
            size: payload.len() as u64,
            display_name: "photo-a.jpg".to_string(),
        };
        let plan_b = TransferPlan {
            source: source.clone(),
            dest: dest_b,
            size: payload.len() as u64,
            display_name: "photo-b.jpg".to_string(),
        };

        let first = get_or_compute_source_signature(&cache, &plan_a, |_| {}).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&source).unwrap().permissions();
            permissions.set_mode(0o000);
            fs::set_permissions(&source, permissions).unwrap();
        }

        let second = get_or_compute_source_signature(&cache, &plan_b, |_| {}).unwrap();

        assert_eq!(first.key, second.key);
        assert_eq!(first.signature, second.signature);
        assert_eq!(cache.lock().unwrap().len(), 1);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&source).unwrap().permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(&source, permissions).unwrap();
        }
    }

    #[test]
    fn source_signature_cache_reports_hash_failure() {
        let temp = TempDir::new("pathsync-source-signature-cache-failure");
        let missing = temp.path().join("missing.jpg");
        let plan = TransferPlan {
            source: missing,
            dest: temp.path().join("target/missing.jpg"),
            size: 42,
            display_name: "missing.jpg".to_string(),
        };
        let cache = SourceSignatureCache::default();

        let failure = get_or_compute_source_signature(&cache, &plan, |_| {}).unwrap_err();

        assert_eq!(failure.operation, CopyOperation::HashSource);
        assert_eq!(failure.kind, ErrorKind::NotFound);
        assert!(failure.message.contains("failed hashing source"));
    }

    #[test]
    fn source_signature_cache_reports_source_change() {
        let temp = TempDir::new("pathsync-source-signature-cache-source-change");
        let source = temp.path().join("photo.jpg");
        let payload = b"shared source bytes";
        write_test_file(&source, payload);
        let plan = TransferPlan {
            source: source.clone(),
            dest: temp.path().join("target/photo.jpg"),
            size: payload.len() as u64,
            display_name: "photo.jpg".to_string(),
        };
        let cache = SourceSignatureCache::default();
        let mut changed = false;

        let failure = get_or_compute_source_signature(&cache, &plan, |_| {
            if !changed {
                write_test_file(&source, b"changed source bytes with a different length");
                changed = true;
            }
        })
        .unwrap_err();

        assert_eq!(failure.operation, CopyOperation::HashSource);
        assert_eq!(failure.kind, ErrorKind::InvalidData);
        assert!(failure.message.contains("source changed while hashing"));
    }

    #[test]
    fn source_signature_cache_returns_cached_failure() {
        let temp = TempDir::new("pathsync-source-signature-cache-cached-failure");
        let source = temp.path().join("photo.jpg");
        let payload = b"shared source bytes";
        write_test_file(&source, payload);
        let plan = TransferPlan {
            source: source.clone(),
            dest: temp.path().join("target/photo.jpg"),
            size: payload.len() as u64,
            display_name: "photo.jpg".to_string(),
        };
        let key = source_signature_key(&source).unwrap();
        let cache = SourceSignatureCache::default();
        cache.lock().unwrap().insert(
            key,
            SourceSignatureEntry::Failed("previous hash failure".to_string()),
        );

        let failure = get_or_compute_source_signature(&cache, &plan, |_| {}).unwrap_err();

        assert_eq!(failure.operation, CopyOperation::HashSource);
        assert_eq!(failure.kind, ErrorKind::Other);
        assert_eq!(failure.message, "previous hash failure");
    }

    #[test]
    fn completed_transfer_successful_run_plan_enqueues_expected_signature() {
        let temp = TempDir::new("pathsync-completed-transfer-success");
        let source = temp.path().join("source/photo.jpg");
        let target = temp.path().join("target");
        let dest = target.join("photo.jpg");
        let payload = b"verified source bytes";
        write_test_file(&source, payload);
        let plan = TransferPlan {
            source: source.clone(),
            dest: dest.clone(),
            size: payload.len() as u64,
            display_name: "photo.jpg".to_string(),
        };
        let cache = SourceSignatureCache::default();
        let (event_tx, event_rx) = unbounded();
        let (completed_tx, completed_rx) = unbounded();

        run_plan(
            0,
            1,
            SizeBucket::Small,
            plan,
            event_tx,
            CopyExecutionContext {
                source_signature_cache: cache,
                completed_tx,
                target_roots: Arc::new(vec![target.clone()]),
                next_transfer_id: Arc::new(AtomicUsize::new(2)),
            },
        );

        let completed = completed_rx.try_recv().unwrap();
        assert_eq!(completed.source, source);
        assert_eq!(completed.dest, dest);
        assert_eq!(completed.target, target);
        assert_eq!(completed.display_name, "photo.jpg");
        assert_eq!(completed.size, payload.len() as u64);
        assert_eq!(
            completed.expected,
            FileSignature {
                size: payload.len() as u64,
                xxh3_128: xxhash_rust::xxh3::xxh3_128(payload),
            }
        );
        verify_completed_transfer(&completed, |_| {}).unwrap();
        assert!(matches!(
            event_rx.try_iter().last(),
            Some(WorkerEvent::Finished { .. })
        ));
    }

    #[test]
    fn source_changed_after_copy_removes_destination_and_sends_failure() {
        let temp = TempDir::new("pathsync-source-changed-post-copy");
        let source = temp.path().join("source/photo.jpg");
        let target = temp.path().join("target");
        let dest = target.join("photo.jpg");
        write_test_file(&source, b"before");
        let plan = TransferPlan {
            source: source.clone(),
            dest: dest.clone(),
            size: 6,
            display_name: "photo.jpg".to_string(),
        };
        let cache = SourceSignatureCache::default();
        let (event_tx, event_rx) = unbounded();
        let (completed_tx, completed_rx) = unbounded();

        set_after_copy_test_hook(Box::new({
            let source = source.clone();
            move || write_test_file(&source, b"after-with-different-size")
        }));

        run_plan(
            0,
            1,
            SizeBucket::Small,
            plan,
            event_tx,
            CopyExecutionContext {
                source_signature_cache: cache,
                completed_tx,
                target_roots: Arc::new(vec![target]),
                next_transfer_id: Arc::new(AtomicUsize::new(2)),
            },
        );

        assert!(!dest.exists());
        assert!(completed_rx.try_recv().is_err());
        let failure = event_rx
            .try_iter()
            .find_map(|event| match event {
                WorkerEvent::Error { failure, .. } => Some(failure),
                _ => None,
            })
            .unwrap();
        assert_eq!(failure.operation, CopyOperation::SourceChanged);
        assert!(failure.message.contains("source changed during run"));
    }

    #[test]
    fn classify_failure_marks_read_only_and_storage_full_as_systemic() {
        assert_eq!(
            classify_failure(ErrorKind::ReadOnlyFilesystem, None, CopyOperation::Write),
            CopyFailureClassification::Systemic
        );
        assert_eq!(
            classify_failure(ErrorKind::StorageFull, None, CopyOperation::Write),
            CopyFailureClassification::Systemic
        );
        assert_eq!(
            classify_failure(ErrorKind::QuotaExceeded, None, CopyOperation::Write),
            CopyFailureClassification::Systemic
        );
    }

    #[test]
    fn permission_failures_promote_after_three_prior_failures() {
        let mut permission_failures = 0;
        let mut failure = CopyFailure {
            source: PathBuf::from("blocked/photo.jpg"),
            dest: Some(PathBuf::from("target/blocked/photo.jpg")),
            operation: CopyOperation::Write,
            kind: ErrorKind::PermissionDenied,
            raw_os_error: None,
            classification: CopyFailureClassification::Local,
            message: "permission denied".to_string(),
        };

        apply_failure_classification(&mut failure, &mut permission_failures);
        assert_eq!(failure.classification, CopyFailureClassification::Local);
        apply_failure_classification(&mut failure, &mut permission_failures);
        assert_eq!(failure.classification, CopyFailureClassification::Local);
        apply_failure_classification(&mut failure, &mut permission_failures);
        assert_eq!(failure.classification, CopyFailureClassification::Local);
        apply_failure_classification(&mut failure, &mut permission_failures);
        assert_eq!(failure.classification, CopyFailureClassification::Systemic);
    }

    #[test]
    fn adaptive_scheduler_backfills_small_work_when_large_item_does_not_fit() {
        let active_large_sources = HashMap::new();
        let active_large_targets = HashMap::new();
        let target_lane_credits = HashSet::new();
        let targets = vec![PathBuf::from("/target")];
        let pending = vec![
            plan("large-a.jpg", 600),
            plan("small-a.jpg", 40),
            plan("small-b.jpg", 20),
        ];

        assert_eq!(
            next_schedulable_index(
                &pending,
                &TransferPolicy::Adaptive {
                    large_file_threshold_bytes: 100,
                    large_file_slots: 3,
                    max_large_per_target: 2,
                },
                1,
                &active_large_sources,
                &active_large_targets,
                &target_lane_credits,
                &targets,
            ),
            Some(1)
        );
    }

    #[test]
    fn adaptive_scheduler_allows_parallel_target_copies_for_same_large_source() {
        let source = PathBuf::from("/source/large-a.jpg");
        let policy = TransferPolicy::Adaptive {
            large_file_threshold_bytes: 100,
            large_file_slots: 4,
            max_large_per_target: 2,
        };
        let mut active_large_sources = HashMap::new();
        let active_large_targets = HashMap::new();
        let target_lane_credits = HashSet::new();
        let targets = vec![PathBuf::from("/target-b")];
        active_large_sources.insert(
            source.clone(),
            ActiveLargeSource {
                active_count: 1,
                held_slots: 4,
            },
        );
        let pending = vec![TransferPlan {
            source,
            dest: PathBuf::from("/target-b/large-a.jpg"),
            size: 600,
            display_name: "large-a.jpg".to_string(),
        }];

        assert_eq!(
            next_schedulable_index(
                &pending,
                &policy,
                0,
                &active_large_sources,
                &active_large_targets,
                &target_lane_credits,
                &targets,
            ),
            Some(0)
        );
    }

    #[test]
    fn adaptive_large_source_releases_slot_when_one_parallel_target_finishes() {
        let source = PathBuf::from("/source/large-a.jpg");
        let policy = TransferPolicy::Adaptive {
            large_file_threshold_bytes: 100,
            large_file_slots: 2,
            max_large_per_target: 2,
        };
        let mut active_large_sources = HashMap::new();
        let mut active_large_targets = HashMap::new();
        let mut target_lane_credits = HashSet::new();
        let targets = vec![PathBuf::from("/target")];
        active_large_sources.insert(
            source.clone(),
            ActiveLargeSource {
                active_count: 2,
                held_slots: 2,
            },
        );
        let pending = vec![plan("small-a.jpg", 40)];

        let released_slots = release_adaptive_slots(
            Some(ActiveLargePlan {
                source,
                target: PathBuf::from("/target"),
            }),
            &mut active_large_sources,
            &mut active_large_targets,
            &mut target_lane_credits,
        );

        assert_eq!(released_slots, 1);
        assert_eq!(
            next_schedulable_index(
                &pending,
                &policy,
                released_slots,
                &active_large_sources,
                &active_large_targets,
                &target_lane_credits,
                &targets,
            ),
            Some(0)
        );
    }

    #[test]
    fn adaptive_scheduler_feeds_faster_target_lane_with_next_large_file() {
        let slow_source = PathBuf::from("/source/large-a.jpg");
        let fast_target = PathBuf::from("/target-a");
        let targets = vec![fast_target.clone(), PathBuf::from("/target-b")];
        let policy = TransferPolicy::Adaptive {
            large_file_threshold_bytes: 100,
            large_file_slots: 4,
            max_large_per_target: 2,
        };
        let mut active_large_sources = HashMap::new();
        let mut active_large_targets = HashMap::new();
        let mut target_lane_credits = HashSet::new();
        active_large_sources.insert(
            slow_source.clone(),
            ActiveLargeSource {
                active_count: 2,
                held_slots: 4,
            },
        );

        let released_slots = release_adaptive_slots(
            Some(ActiveLargePlan {
                source: slow_source,
                target: fast_target.clone(),
            }),
            &mut active_large_sources,
            &mut active_large_targets,
            &mut target_lane_credits,
        );

        let pending = vec![TransferPlan {
            source: PathBuf::from("/source/large-b.jpg"),
            dest: fast_target.join("large-b.jpg"),
            size: 600,
            display_name: "large-b.jpg".to_string(),
        }];

        assert_eq!(released_slots, 1);
        assert_eq!(
            next_schedulable_index(
                &pending,
                &policy,
                released_slots,
                &active_large_sources,
                &active_large_targets,
                &target_lane_credits,
                &targets,
            ),
            Some(0)
        );
    }

    #[test]
    fn adaptive_scheduler_respects_max_large_per_target() {
        let target = PathBuf::from("/target-a");
        let targets = vec![target.clone()];
        let policy = TransferPolicy::Adaptive {
            large_file_threshold_bytes: 100,
            large_file_slots: 4,
            max_large_per_target: 2,
        };
        let active_large_sources = HashMap::new();
        let mut active_large_targets = HashMap::new();
        let target_lane_credits = HashSet::new();
        active_large_targets.insert(target.clone(), 2);

        let pending = vec![TransferPlan {
            source: PathBuf::from("/source/large-c.jpg"),
            dest: target.join("large-c.jpg"),
            size: 600,
            display_name: "large-c.jpg".to_string(),
        }];

        assert_eq!(
            next_schedulable_index(
                &pending,
                &policy,
                4,
                &active_large_sources,
                &active_large_targets,
                &target_lane_credits,
                &targets,
            ),
            None
        );
    }

    #[test]
    fn target_volume_label_uses_macos_volume_root_when_available() {
        assert_eq!(
            target_volume_label(Path::new("/Volumes/T7/Videos/clip.mp4")),
            "T7"
        );
        assert_eq!(target_volume_label(Path::new("/tmp/target/clip.mp4")), "");
    }

    #[test]
    fn initial_target_results_counts_planned_files_by_target() {
        let targets = vec![PathBuf::from("/target-a"), PathBuf::from("/target-b")];
        let plans = vec![
            TransferPlan {
                source: PathBuf::from("/source/a.jpg"),
                dest: PathBuf::from("/target-a/a.jpg"),
                size: 10,
                display_name: "a.jpg".to_string(),
            },
            TransferPlan {
                source: PathBuf::from("/source/b.jpg"),
                dest: PathBuf::from("/target-b/b.jpg"),
                size: 20,
                display_name: "b.jpg".to_string(),
            },
            TransferPlan {
                source: PathBuf::from("/source/c.jpg"),
                dest: PathBuf::from("/target-b/c.jpg"),
                size: 30,
                display_name: "c.jpg".to_string(),
            },
        ];

        let results = initial_target_results(&plans, &targets);

        assert_eq!(results["target-a"].planned, 1);
        assert_eq!(results["target-b"].planned, 2);
    }

    #[test]
    fn target_results_account_for_copy_verify_and_failures() {
        let mut report = CopyReport::default();
        let target = PathBuf::from("/target");
        report.target_results = initial_target_results(
            &[
                TransferPlan {
                    source: PathBuf::from("/source/a.jpg"),
                    dest: PathBuf::from("/target/a.jpg"),
                    size: 10,
                    display_name: "a.jpg".to_string(),
                },
                TransferPlan {
                    source: PathBuf::from("/source/b.jpg"),
                    dest: PathBuf::from("/target/b.jpg"),
                    size: 20,
                    display_name: "b.jpg".to_string(),
                },
            ],
            std::slice::from_ref(&target),
        );

        report.record_target_copy(&target, 10);
        report.record_target_verified(&target, 10);
        report.record_target_copy_failure(&target);
        report.record_target_verify_failure(&target);

        let result = &report.target_results["target"];
        assert_eq!(result.planned, 2);
        assert_eq!(result.planned_bytes, 30);
        assert_eq!(result.copied, 1);
        assert_eq!(result.copied_bytes, 10);
        assert_eq!(result.verified, 1);
        assert_eq!(result.verified_bytes, 10);
        assert_eq!(result.copy_failed, 1);
        assert_eq!(result.verify_failed, 1);
    }

    #[test]
    fn live_screen_model_uses_canonical_status_and_worker_rows() {
        let mut state = ProgressState::new(3, 1_300);
        state.phase = PhaseKind::LargeFiles;
        state.active_workers = 1;
        state.bytes_done = 584;

        let mut worker_states = vec![WorkerState::default(), WorkerState::default()];
        worker_states[0].label = "b/photo2.jpg".to_string();
        worker_states[0].copied = 600;
        worker_states[0].total = 1_000;
        worker_states[0].started = Some(Instant::now() - Duration::from_secs(4));

        let context = RenderContext {
            job_name: "demo".to_string(),
            target: PathBuf::from("/target"),
            target_count: 1,
            source_root: PathBuf::from("/source"),
            task_count: 3,
            total_bytes: 1_300,
            planning_stats: PlanningStats {
                scanned_files: 3,
                planned_files: 3,
                planned_bytes: 1_300,
                skipped_existing_files: 0,
                skipped_existing_bytes: 0,
            },
            target_roots: Arc::new(vec![PathBuf::from("/target")]),
            target_results: BTreeMap::new(),
        };

        let render_now = Instant::now();
        worker_states[0].started = Some(render_now - Duration::from_secs(4));
        let report = CopyReport {
            target_results: context.target_results.clone(),
            ..CopyReport::default()
        };
        let model = build_live_screen_model(&context, &state, &worker_states, &report, render_now);

        assert_eq!(model.status, "LIVE / COPY-LARGE");
        assert_eq!(model.summary[0].label, "Scanned");
        assert_eq!(model.summary[1].label, "Planned");
        assert_eq!(model.overall_label, "Copying");
        assert_eq!(model.workers[0].spinner_frame, Some('⠋'));
        assert_eq!(model.workers[0].worker_tag, "T01");
        assert!(!model.workers[0].idle);
        assert!(model.workers[1].idle);
    }

    #[test]
    fn live_screen_model_maps_adaptive_runs_to_active_large_bucket() {
        let mut state = ProgressState::new(3, 1_300);
        state.phase = PhaseKind::Adaptive;
        state.active_workers = 1;
        state.bytes_done = 584;

        let mut worker_states = vec![WorkerState::default(), WorkerState::default()];
        worker_states[0].label = "b/photo2.jpg".to_string();
        worker_states[0].bucket = SizeBucket::Large;
        worker_states[0].copied = 600;
        worker_states[0].total = 1_000;
        let render_now = Instant::now();
        worker_states[0].started = Some(render_now - Duration::from_secs(4));

        let context = RenderContext {
            job_name: "demo".to_string(),
            target: PathBuf::from("/target"),
            target_count: 1,
            source_root: PathBuf::from("/source"),
            task_count: 3,
            total_bytes: 1_300,
            planning_stats: PlanningStats {
                scanned_files: 3,
                planned_files: 3,
                planned_bytes: 1_300,
                skipped_existing_files: 0,
                skipped_existing_bytes: 0,
            },
            target_roots: Arc::new(vec![PathBuf::from("/target")]),
            target_results: BTreeMap::new(),
        };

        let report = CopyReport {
            target_results: context.target_results.clone(),
            ..CopyReport::default()
        };
        let model = build_live_screen_model(&context, &state, &worker_states, &report, render_now);

        assert_eq!(model.status, "LIVE / COPY-LARGE");
        assert_eq!(model.phase_label, "overall  copying large files");
        assert_eq!(model.workers[0].spinner_frame, Some('⠋'));
        assert_eq!(model.workers[0].size, "1000 B");
        assert_eq!(model.workers[0].time, "150 B/s");
    }

    #[test]
    fn adaptive_live_screen_uses_active_worker_bucket_for_display_phase() {
        let mut state = ProgressState::new(3, 1_300);
        state.phase = PhaseKind::Adaptive;
        state.active_workers = 1;
        state.bytes_done = 584;

        let mut worker_states = vec![WorkerState::default(), WorkerState::default()];
        worker_states[0].bucket = SizeBucket::Large;
        worker_states[0].label = "clip.mp4".to_string();
        worker_states[0].copied = 600;
        worker_states[0].total = 1_000;
        let render_now = Instant::now();
        worker_states[0].started = Some(render_now - Duration::from_secs(4));

        let context = RenderContext {
            job_name: "demo".to_string(),
            target: PathBuf::from("/target"),
            target_count: 1,
            source_root: PathBuf::from("/source"),
            task_count: 3,
            total_bytes: 1_300,
            planning_stats: PlanningStats {
                scanned_files: 3,
                planned_files: 3,
                planned_bytes: 1_300,
                skipped_existing_files: 0,
                skipped_existing_bytes: 0,
            },
            target_roots: Arc::new(vec![PathBuf::from("/target")]),
            target_results: BTreeMap::new(),
        };

        let report = CopyReport {
            target_results: context.target_results.clone(),
            ..CopyReport::default()
        };
        let model = build_live_screen_model(&context, &state, &worker_states, &report, render_now);

        assert_eq!(model.status, "LIVE / COPY-LARGE");
        assert_eq!(model.phase_label, "overall  copying large files");
    }

    #[test]
    fn braille_spinner_frame_cycles_through_braille_sequence() {
        assert_eq!(braille_spinner_frame(0), '⠋');
        assert_eq!(braille_spinner_frame(1), '⠙');
        assert_eq!(braille_spinner_frame(9), '⠏');
        assert_eq!(braille_spinner_frame(10), '⠋');
    }

    #[test]
    fn worker_spinner_frame_offsets_each_worker_independently() {
        let render_now = Instant::now();
        let started = render_now - Duration::from_millis(10);

        assert_eq!(worker_spinner_frame(Some(started), 0, render_now), '⠋');
        assert_eq!(worker_spinner_frame(Some(started), 1, render_now), '⠙');
        assert_eq!(worker_spinner_frame(Some(started), 2, render_now), '⠹');
    }

    #[test]
    fn post_run_screen_model_groups_categories_and_errors() {
        let context = RenderContext {
            job_name: "demo".to_string(),
            target: PathBuf::from("/target"),
            target_count: 1,
            source_root: PathBuf::from("/source"),
            task_count: 4,
            total_bytes: 2_000,
            planning_stats: PlanningStats {
                scanned_files: 4,
                planned_files: 4,
                planned_bytes: 2_000,
                skipped_existing_files: 2,
                skipped_existing_bytes: 800,
            },
            target_roots: Arc::new(vec![PathBuf::from("/target")]),
            target_results: BTreeMap::new(),
        };
        let report = CopyReport {
            duration: Duration::from_secs(8),
            bytes_done: 1_200,
            copied_files: vec![
                CopiedFileRecord {
                    file: "one/video.mp4".to_string(),
                    size: 1_000,
                },
                CopiedFileRecord {
                    file: "two/image.jpg".to_string(),
                    size: 200,
                },
            ],
            failures: vec![CopyFailure {
                source: PathBuf::from("/source/GX010193.MP4"),
                dest: Some(PathBuf::from("/target/GX010193.MP4")),
                operation: CopyOperation::Write,
                kind: ErrorKind::PermissionDenied,
                raw_os_error: None,
                classification: CopyFailureClassification::Local,
                message: "permission denied".to_string(),
            }],
            large: PhaseTotals::default(),
            small: PhaseTotals::default(),
            target_results: BTreeMap::new(),
            failed: true,
            systemic_detected: false,
        };

        let model = build_post_run_screen_model(&context, &report, 2, 800);

        assert_eq!(model.status, "ATTENTION");
        assert!(model.categories.iter().any(|row| row.label == "copied mp4"));
        assert!(model.categories.iter().any(|row| row.label == "copied jpg"));
        assert!(
            model
                .categories
                .iter()
                .any(|row| row.label == "failed permission")
        );
        assert_eq!(model.errors[0].error, "permission denied");
    }

    #[test]
    fn plain_summary_caps_failure_preview_and_includes_target_phase_columns() {
        let target = PathBuf::from("/target");
        let mut failures = Vec::new();
        for index in 0..(SUMMARY_FAILURE_PREVIEW_LIMIT + 1) {
            failures.push(CopyFailure {
                source: PathBuf::from(format!("/source/file-{index}.jpg")),
                dest: Some(target.join(format!("file-{index}.jpg"))),
                operation: CopyOperation::Verify,
                kind: ErrorKind::InvalidData,
                raw_os_error: None,
                classification: CopyFailureClassification::Local,
                message: "signature mismatch".to_string(),
            });
        }
        let report = CopyReport {
            duration: Duration::from_secs(1),
            failures,
            failed: true,
            ..CopyReport::default()
        };

        let lines = summary_lines(
            "demo",
            &target,
            Path::new("/source"),
            &report,
            21,
            210,
            std::slice::from_ref(&target),
        );

        assert!(
            lines
                .iter()
                .any(|line| line.contains("Target") && line.contains("Phase"))
        );
        assert!(lines.iter().any(|line| line.contains("verify")));
        assert!(lines.iter().any(|line| {
            line == &format!(
                "showing {} of {} failures",
                SUMMARY_FAILURE_PREVIEW_LIMIT,
                SUMMARY_FAILURE_PREVIEW_LIMIT + 1
            )
        }));
    }

    #[test]
    fn copy_transfer_errors_map_to_existing_copy_failures() {
        let plan = plan("photo.jpg", 48);
        let error = CopyTransferError::io(
            CopyTransferOperation::NativeCopy,
            io::Error::new(ErrorKind::BrokenPipe, "boom"),
        );

        let temp_dest = PathBuf::from("/target/photo.jpg.pathsync-part");
        let failure = copy_transfer_failure(&plan, error, &temp_dest);

        assert_eq!(failure.source, plan.source);
        assert_eq!(failure.dest, Some(plan.dest));
        assert_eq!(failure.operation, CopyOperation::Write);
        assert_eq!(failure.kind, ErrorKind::BrokenPipe);
        assert_eq!(failure.raw_os_error, None);
        assert_eq!(failure.classification, CopyFailureClassification::Local);
    }
}
