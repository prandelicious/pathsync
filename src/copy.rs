use crossbeam_channel::{Receiver, Sender, unbounded};
use filetime::{FileTime, set_file_mtime};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, ErrorKind, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::ResolvedJob;
use crate::error::{CopyError, CopyFailure, CopyFailureClassification, CopyOperation};
use crate::format::{format_duration, human_bytes, human_rate};
use crate::plan::TransferPlan;
use crate::policy::TransferPolicy;
use crate::progress_format::{
    overall_line, plain_progress_line, worker_label, worker_line, worker_prefix,
};
use crate::progress_model::{PhaseKind, ProgressSnapshot, active_worker_slots, phase_label};

const COPY_BUFFER_SIZE: usize = 8 * 1024 * 1024;
const WORKER_NAME_WIDTH: usize = 36;
const PLAIN_PROGRESS_UPDATE_INTERVAL: Duration = Duration::from_millis(250);
const SUMMARY_FILE_PREVIEW_LIMIT: usize = 8;

#[derive(Debug)]
enum WorkerEvent {
    PhaseStarted {
        phase: PhaseKind,
        worker_count: usize,
    },
    Started {
        worker: usize,
        bucket: SizeBucket,
        name: String,
        source: PathBuf,
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
        bytes: u64,
    },
    Error {
        worker: usize,
        failure: CopyFailure,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SizeBucket {
    Large,
    Small,
}

impl Default for SizeBucket {
    fn default() -> Self {
        Self::Small
    }
}

#[derive(Debug, Default)]
struct WorkerState {
    label: String,
    copied: u64,
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

#[derive(Debug, Default)]
struct CopyReport {
    duration: Duration,
    bytes_done: u64,
    copied_files: Vec<CopiedFileRecord>,
    failures: Vec<CopyFailure>,
    large: PhaseTotals,
    small: PhaseTotals,
    failed: bool,
    systemic_detected: bool,
}

#[derive(Debug, Clone)]
struct RenderContext {
    job_name: String,
    target: PathBuf,
    source_root: PathBuf,
    task_count: usize,
    total_bytes: u64,
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

pub fn run_copy(job: &ResolvedJob, plans: Vec<TransferPlan>) -> Result<(), CopyError> {
    let total_bytes: u64 = plans.iter().map(|plan| plan.size).sum();
    let task_count = plans.len();
    let large_file_count = count_large_files(job, &plans);
    let (event_tx, event_rx) = unbounded::<WorkerEvent>();
    let source_root = job.source.clone();
    let job_name = job.name.clone();
    let target = job.target.clone();
    let use_tty = io::stdout().is_terminal();
    let render_context = RenderContext {
        job_name,
        target,
        source_root,
        task_count,
        total_bytes,
    };

    let ui_handle = if use_tty {
        let progress = MultiProgress::new();
        print_header_lines_tty(&progress, job, task_count, total_bytes, large_file_count)?;
        let overall = progress.add(ProgressBar::new(total_bytes));
        overall.set_style(overall_style()?);
        overall.set_prefix("total");
        overall.set_message(overall_line(
            &ProgressState::new(task_count, total_bytes).snapshot(),
        ));

        thread::spawn(move || render_progress_tty(progress, overall, event_rx, render_context))
    } else {
        print_header_lines_plain(job, task_count, total_bytes, large_file_count);
        thread::spawn(move || render_progress_plain(event_rx, render_context))
    };

    match job.transfer_policy {
        TransferPolicy::Standard => {
            execute_phase(
                PhaseKind::SmallFiles,
                SizeBucket::Small,
                job.parallel,
                plans,
                event_tx.clone(),
            );
        }
        TransferPolicy::Adaptive { .. } => {
            execute_adaptive(job, plans, event_tx.clone());
        }
    }
    drop(event_tx);

    ui_handle.join().map_err(|_| CopyError::UiThreadPanicked)?
}

fn execute_phase(
    phase: PhaseKind,
    bucket: SizeBucket,
    configured_parallel: usize,
    plans: Vec<TransferPlan>,
    event_tx: Sender<WorkerEvent>,
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
        handles.push(thread::spawn(move || {
            worker_loop(worker, bucket, worker_rx, tx)
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

fn execute_adaptive(job: &ResolvedJob, plans: Vec<TransferPlan>, event_tx: Sender<WorkerEvent>) {
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
    let mut active = Vec::<(usize, usize, thread::JoinHandle<()>)>::new();
    let mut active_slots = 0_usize;
    let (done_tx, done_rx) = unbounded::<usize>();

    while !pending.is_empty() || !active.is_empty() {
        while !idle_workers.is_empty() {
            let available_slots = job.parallel.saturating_sub(active_slots);
            let Some(index) =
                next_schedulable_index(&pending, &job.transfer_policy, available_slots)
            else {
                break;
            };

            let plan = pending.remove(index);
            let worker = idle_workers.pop().expect("idle worker should exist");
            let bucket = bucket_for_plan(job, &plan);
            let slot_cost = slot_cost(&job.transfer_policy, &plan);
            active_slots += slot_cost;

            let tx = event_tx.clone();
            let done = done_tx.clone();
            let handle = thread::spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_plan(worker, bucket, plan, tx.clone());
                }));
                if outcome.is_err() {
                    let _ = tx.send(WorkerEvent::Error {
                        worker,
                        failure: panic_failure(worker, CopyOperation::WorkerPanic),
                    });
                }
                let _ = done.send(worker);
            });
            active.push((worker, slot_cost, handle));
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
            let (worker, slot_cost, handle) = active.swap_remove(index);
            let _ = handle.join();
            active_slots = active_slots.saturating_sub(slot_cost);
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
) {
    loop {
        let Some(plan) = rx.recv().ok() else {
            break;
        };

        run_plan(worker, bucket, plan, tx.clone());
    }
}

fn run_plan(worker: usize, bucket: SizeBucket, plan: TransferPlan, tx: Sender<WorkerEvent>) {
    let started_name = plan.display_name.clone();
    let started_source = plan.source.clone();
    let _ = tx.send(WorkerEvent::Started {
        worker,
        bucket,
        name: started_name,
        source: started_source,
        total: plan.size,
    });

    match copy_file(&plan, worker, &tx) {
        Ok(bytes) => {
            let _ = tx.send(WorkerEvent::Finished {
                worker,
                bucket,
                name: plan.display_name,
                source: plan.source,
                bytes,
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
        let source_file = File::open(&plan.source).map_err(|err| {
            copy_failure(
                plan,
                CopyOperation::OpenSource,
                err,
                format!("failed to open source file: {}", plan.source.display()),
            )
        })?;
        let metadata = source_file.metadata().map_err(|err| {
            copy_failure(
                plan,
                CopyOperation::OpenSource,
                err,
                format!("failed to stat source file: {}", plan.source.display()),
            )
        })?;
        let mut reader = BufReader::with_capacity(COPY_BUFFER_SIZE, source_file);
        let temp_file = File::create(&temp_dest).map_err(|err| {
            copy_failure(
                plan,
                CopyOperation::CreateTemp,
                err,
                format!("failed to create temp file: {}", temp_dest.display()),
            )
        })?;
        let mut writer = BufWriter::with_capacity(COPY_BUFFER_SIZE, temp_file);
        let mut buffer = vec![0_u8; COPY_BUFFER_SIZE];
        let mut copied = 0_u64;

        loop {
            let read = reader.read(&mut buffer).map_err(|err| {
                copy_failure(
                    plan,
                    CopyOperation::Read,
                    err,
                    format!("failed reading {}", plan.source.display()),
                )
            })?;
            if read == 0 {
                break;
            }

            writer.write_all(&buffer[..read]).map_err(|err| {
                copy_failure(
                    plan,
                    CopyOperation::Write,
                    err,
                    format!("failed writing {}", temp_dest.display()),
                )
            })?;
            copied += read as u64;

            let _ = tx.send(WorkerEvent::Progress { worker, copied });
        }

        writer.flush().map_err(|err| {
            copy_failure(
                plan,
                CopyOperation::Flush,
                err,
                format!("failed flushing {}", temp_dest.display()),
            )
        })?;
        drop(writer);

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

        Ok(copied)
    })();

    if copy_result.is_err() && temp_dest.exists() {
        let _ = fs::remove_file(&temp_dest);
    }

    copy_result
}

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

fn render_progress_tty(
    progress: MultiProgress,
    overall: ProgressBar,
    rx: Receiver<WorkerEvent>,
    context: RenderContext,
) -> Result<(), CopyError> {
    let mut state = ProgressState::new(context.task_count, context.total_bytes);
    let mut report = CopyReport::default();
    let mut permission_failures = 0_usize;
    let mut worker_bars: Vec<ProgressBar> = Vec::new();
    let mut worker_states: Vec<WorkerState> = Vec::new();

    for event in rx {
        match event {
            WorkerEvent::PhaseStarted {
                phase,
                worker_count,
            } => {
                for bar in worker_bars.drain(..) {
                    bar.finish_and_clear();
                }

                state.phase = phase;
                worker_states = (0..worker_count).map(|_| WorkerState::default()).collect();
                progress
                    .println(format!("phase    : {}", phase_label(phase)))
                    .map_err(|err| CopyError::Internal {
                        message: err.to_string(),
                    })?;

                for worker in 0..worker_count {
                    let bar = progress.add(ProgressBar::new(0));
                    bar.set_style(worker_idle_style()?);
                    bar.set_prefix(worker_prefix(worker));
                    bar.set_message(worker_line("waiting", 0, Duration::ZERO));
                    worker_bars.push(bar);
                }
            }
            WorkerEvent::Started {
                worker,
                bucket,
                name,
                source,
                total,
            } => {
                let label = worker_label(&name, &source, &context.source_root, WORKER_NAME_WIDTH);
                let worker_state = &mut worker_states[worker];
                worker_state.bucket = bucket;
                worker_state.label = label.clone();
                worker_state.copied = 0;
                worker_state.started = Some(Instant::now());
                state.active_workers += 1;

                if let Some(bar) = worker_bars.get(worker) {
                    bar.set_style(worker_active_style()?);
                    bar.set_length(total);
                    bar.set_position(0);
                    bar.set_message(worker_line(&label, 0, Duration::ZERO));
                    bar.enable_steady_tick(Duration::from_millis(120));
                }
            }
            WorkerEvent::Progress { worker, copied } => {
                let worker_state = &mut worker_states[worker];
                if copied > worker_state.copied {
                    let delta = copied - worker_state.copied;
                    state.bytes_done += delta;
                    overall.inc(delta);
                }
                worker_state.copied = copied;

                let elapsed = worker_state
                    .started
                    .map(|started| started.elapsed())
                    .unwrap_or(Duration::ZERO);
                if let Some(bar) = worker_bars.get(worker) {
                    bar.set_position(copied);
                    bar.set_message(worker_line(&worker_state.label, copied, elapsed));
                }
            }
            WorkerEvent::Finished {
                worker,
                bucket,
                name,
                source,
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
                if bytes > worker_state.copied {
                    let delta = bytes - worker_state.copied;
                    state.bytes_done += delta;
                    overall.inc(delta);
                }

                worker_state.copied = 0;
                worker_state.label.clear();
                worker_state.started = None;
                state.active_workers = state.active_workers.saturating_sub(1);
                state.completed += 1;
                report.record_copy(
                    bucket,
                    relative_file_label(&context.source_root, &source),
                    bytes,
                );

                if let Some(bar) = worker_bars.get(worker) {
                    bar.disable_steady_tick();
                    bar.set_style(worker_idle_style()?);
                    bar.set_length(0);
                    bar.set_position(0);
                    bar.set_message(format!("done: {label}"));
                }
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
                worker_state.copied = 0;
                worker_state.label.clear();
                worker_state.started = None;
                state.active_workers = state.active_workers.saturating_sub(1);
                state.failed = true;
                report.record_failure(failure.clone());

                if let Some(bar) = worker_bars.get(worker) {
                    bar.disable_steady_tick();
                    bar.set_style(worker_idle_style()?);
                    bar.set_length(0);
                    bar.set_position(0);
                    bar.set_message(format!("failed: {label}"));
                }

                progress
                    .println(format!(
                        "{} error: {label}: {}",
                        worker_prefix(worker),
                        failure.message
                    ))
                    .map_err(|err| CopyError::Internal {
                        message: err.to_string(),
                    })?;
            }
        }

        overall.set_message(overall_line(&state.snapshot()));
    }

    clear_tty_progress(&overall, worker_bars);

    report.duration = state.started.elapsed();
    report.bytes_done = state.bytes_done;
    report.failed = state.failed;
    print_copy_report_tty(
        &progress,
        summary_lines(
            &context.job_name,
            &context.target,
            &context.source_root,
            &report,
            context.task_count,
            context.total_bytes,
        ),
    )?;

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

fn render_progress_plain(
    rx: Receiver<WorkerEvent>,
    context: RenderContext,
) -> Result<(), CopyError> {
    let mut state = ProgressState::new(context.task_count, context.total_bytes);
    let mut report = CopyReport::default();
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
                bucket,
                name,
                source,
                total: _total,
            } => {
                let label = worker_label(&name, &source, &context.source_root, WORKER_NAME_WIDTH);
                let worker_state = &mut worker_states[worker];
                worker_state.bucket = bucket;
                worker_state.label = label.clone();
                worker_state.copied = 0;
                worker_state.started = Some(Instant::now());
                state.active_workers += 1;
                println!(
                    "{}: {}",
                    worker_prefix(worker),
                    worker_line(&label, 0, Duration::ZERO)
                );
            }
            WorkerEvent::Progress { worker, copied } => {
                let worker_state = &mut worker_states[worker];
                if copied > worker_state.copied {
                    state.bytes_done += copied - worker_state.copied;
                }
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
                if bytes > worker_state.copied {
                    state.bytes_done += bytes - worker_state.copied;
                }
                worker_state.copied = 0;
                worker_state.label.clear();
                worker_state.started = None;
                state.active_workers = state.active_workers.saturating_sub(1);
                state.completed += 1;
                report.record_copy(
                    bucket,
                    relative_file_label(&context.source_root, &source),
                    bytes,
                );
                println!("{}: done: {label}", worker_prefix(worker));
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
                worker_state.copied = 0;
                worker_state.label.clear();
                worker_state.started = None;
                state.active_workers = state.active_workers.saturating_sub(1);
                state.failed = true;
                report.record_failure(failure.clone());
                println!(
                    "{} error: {label}: {}",
                    worker_prefix(worker),
                    failure.message
                );
                println!("{}", plain_progress_line(&state.snapshot()));
                last_progress_line = Instant::now();
            }
        }
    }

    println!("{}", plain_progress_line(&state.snapshot()));
    report.duration = state.started.elapsed();
    report.bytes_done = state.bytes_done;
    report.failed = state.failed;
    print_copy_report_plain(summary_lines(
        &context.job_name,
        &context.target,
        &context.source_root,
        &report,
        context.task_count,
        context.total_bytes,
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

fn print_copy_report_tty(progress: &MultiProgress, lines: Vec<String>) -> Result<(), CopyError> {
    for line in lines {
        progress.println(line).map_err(|err| CopyError::Internal {
            message: err.to_string(),
        })?;
    }
    Ok(())
}

fn print_copy_report_plain(lines: Vec<String>) {
    for line in lines {
        println!("{line}");
    }
}

fn clear_tty_progress(overall: &ProgressBar, worker_bars: Vec<ProgressBar>) {
    overall.finish_and_clear();
    for bar in worker_bars {
        bar.finish_and_clear();
    }
}

fn summary_lines(
    job_name: &str,
    target: &Path,
    source_root: &Path,
    report: &CopyReport,
    task_count: usize,
    total_bytes: u64,
) -> Vec<String> {
    let title = if report.failed {
        "ATTENTION ITEMS"
    } else {
        "SYNC COMPLETE"
    };
    let copied_bytes: u64 = report.copied_files.iter().map(|file| file.size).sum();
    let avg_rate = average_rate(report.bytes_done, report.duration);
    let mut lines = vec![
        String::new(),
        main_header(title),
        String::new(),
        "Summary".to_string(),
        section_divider(),
        summary_row("Job", job_name),
        summary_row("Target", &target.display().to_string()),
        summary_row(
            "Result",
            if report.failed {
                "copy failed"
            } else {
                "success"
            },
        ),
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
        "Counts".to_string(),
        section_divider(),
        count_row("Copied", report.copied_files.len(), copied_bytes),
        count_row("Planned", task_count, total_bytes),
        count_row("Failed", report.failures.len(), 0),
        String::new(),
        "Buckets".to_string(),
        section_divider(),
        count_row("Large", report.large.files, report.large.bytes),
        count_row("Small", report.small.files, report.small.bytes),
    ];

    if !report.copied_files.is_empty() {
        lines.push(String::new());
        lines.push("Copied Files".to_string());
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
                "Showing {} of {} copied files.",
                SUMMARY_FILE_PREVIEW_LIMIT,
                report.copied_files.len()
            ));
        }
    }

    if !report.failures.is_empty() {
        lines.push(String::new());
        lines.push("Failures".to_string());
        lines.push(section_divider());
        lines.push(format!("{:<44} {}", "File", "Error"));
        for failure in &report.failures {
            lines.push(format!(
                "{:<44} [{}] {}",
                truncate_right(&relative_file_label(source_root, &failure.source), 44),
                failure.classification,
                single_line_error(&failure.message)
            ));
        }
    }

    lines
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

fn overall_style() -> Result<ProgressStyle, CopyError> {
    ProgressStyle::with_template("{prefix:>8} [{bar:40.cyan/white.dim}] {percent:>3}% {msg}")
        .map(|style| style.progress_chars(dense_block_chars()))
        .map_err(|err| CopyError::Internal {
            message: err.to_string(),
        })
}

fn worker_active_style() -> Result<ProgressStyle, CopyError> {
    ProgressStyle::with_template(
        "{prefix:>6.cyan} {spinner:.green} [{bar:24.magenta/white.dim}] {percent:>3}% {msg}",
    )
    .map(|style| {
        style
            .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ")
            .progress_chars(dense_block_chars())
    })
    .map_err(|err| CopyError::Internal {
        message: err.to_string(),
    })
}

fn worker_idle_style() -> Result<ProgressStyle, CopyError> {
    ProgressStyle::with_template("{prefix:>6.cyan}          {msg}").map_err(|err| {
        CopyError::Internal {
            message: err.to_string(),
        }
    })
}

fn print_header_lines_tty(
    progress: &MultiProgress,
    job: &ResolvedJob,
    task_count: usize,
    total_bytes: u64,
    large_file_count: usize,
) -> Result<(), CopyError> {
    for line in header_lines(job, task_count, total_bytes, large_file_count) {
        progress.println(line).map_err(|err| CopyError::Internal {
            message: err.to_string(),
        })?;
    }

    Ok(())
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
        format!("target   : {}", job.target.display()),
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

fn dense_block_chars() -> &'static str {
    "█▉▊▋▌▍▎▏░"
}

fn transfer_policy_label(policy: &TransferPolicy) -> String {
    match policy {
        TransferPolicy::Standard => "standard".to_string(),
        TransferPolicy::Adaptive {
            large_file_threshold_bytes,
            large_file_slots,
        } => format!(
            "adaptive (large >= {}, slots {})",
            human_bytes(*large_file_threshold_bytes),
            large_file_slots
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
        } => {
            if plan.size >= *large_file_threshold_bytes {
                *large_file_slots
            } else {
                1
            }
        }
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
) -> Option<usize> {
    pending
        .iter()
        .position(|plan| slot_cost(policy, plan) <= available_slots)
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
    use indicatif::{InMemoryTerm, MultiProgress, ProgressDrawTarget};

    fn plan(name: &str, size: u64) -> TransferPlan {
        TransferPlan {
            source: PathBuf::from(format!("/source/{name}")),
            dest: PathBuf::from(format!("/target/{name}")),
            size,
            display_name: name.to_string(),
        }
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
    fn clear_tty_progress_removes_total_bar_before_summary_output() {
        let term = InMemoryTerm::new(20, 120);
        let progress =
            MultiProgress::with_draw_target(ProgressDrawTarget::term_like(Box::new(term.clone())));
        let overall = progress.add(ProgressBar::new(10));
        overall.set_style(overall_style().unwrap());
        overall.set_prefix("total");
        overall.set_position(10);
        overall.set_message("all copies complete");

        clear_tty_progress(&overall, Vec::new());
        progress.println("SYNC COMPLETE").unwrap();

        let contents = term.contents();
        assert!(contents.contains("SYNC COMPLETE"));
        assert!(!contents.contains("total"));
        assert!(!contents.contains("all copies complete"));
    }

    #[test]
    fn dense_block_chars_use_visible_incomplete_segment() {
        assert_eq!(dense_block_chars().chars().last(), Some('░'));
    }

    #[test]
    fn adaptive_scheduler_backfills_small_work_when_large_item_does_not_fit() {
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
                },
                1,
            ),
            Some(1)
        );
    }
}
