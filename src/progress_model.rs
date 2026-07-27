use std::time::Duration;

use crate::format::{format_duration, human_bytes, human_rate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseKind {
    Adaptive,
    LargeFiles,
    SmallFiles,
    /// Staged relay mode (U6): the single phase covering the whole staged
    /// run -- staging (source->spool) and every target lane (spool->target)
    /// running concurrently on one worker-id space, sized once up front by
    /// `LaneWorkerLayout::total_workers()`. Unlike `LargeFiles`/`SmallFiles`,
    /// this is not swapped mid-run: staged mode never fires more than one
    /// `WorkerEvent::PhaseStarted`, so there is nothing to switch between.
    Staging,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub completed: usize,
    pub task_count: usize,
    pub active_workers: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub elapsed: Duration,
    pub phase: PhaseKind,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryMetric {
    pub label: String,
    pub value: String,
}

impl SummaryMetric {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressBarModel {
    pub percent: usize,
    pub width: usize,
}

impl ProgressBarModel {
    pub fn new(percent: usize, width: usize) -> Self {
        Self { percent, width }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TransferRowPhase {
    Hashing,
    #[default]
    Copying,
    Verifying,
}

impl TransferRowPhase {
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Hashing => "hashing",
            Self::Copying => "copying",
            Self::Verifying => "verifying",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRowModel {
    pub spinner_frame: Option<char>,
    pub worker_tag: String,
    pub phase: Option<TransferRowPhase>,
    pub percent: usize,
    pub item: String,
    pub size: String,
    pub time: String,
    pub target: String,
    pub idle: bool,
}

impl WorkerRowModel {
    pub fn active(
        spinner_frame: char,
        worker_tag: impl Into<String>,
        percent: usize,
        item: impl Into<String>,
        size: impl Into<String>,
        time: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self::active_with_phase(
            spinner_frame,
            worker_tag,
            TransferRowPhase::Copying,
            percent,
            item,
            size,
            time,
            target,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn active_with_phase(
        spinner_frame: char,
        worker_tag: impl Into<String>,
        phase: TransferRowPhase,
        percent: usize,
        item: impl Into<String>,
        size: impl Into<String>,
        time: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            spinner_frame: Some(spinner_frame),
            worker_tag: worker_tag.into(),
            phase: Some(phase),
            percent,
            item: item.into(),
            size: size.into(),
            time: time.into(),
            target: target.into(),
            idle: false,
        }
    }

    pub fn idle(worker_tag: impl Into<String>) -> Self {
        Self {
            spinner_frame: None,
            worker_tag: worker_tag.into(),
            phase: None,
            percent: 0,
            item: "idle".to_string(),
            size: "--".to_string(),
            time: "--".to_string(),
            target: String::new(),
            idle: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferCategory {
    SkippedExisting,
    CopiedMp4,
    CopiedJpg,
    FailedPermission,
    FailedCollision,
}

impl TransferCategory {
    pub fn as_label(self) -> &'static str {
        match self {
            Self::SkippedExisting => "skipped existing",
            Self::CopiedMp4 => "copied mp4",
            Self::CopiedJpg => "copied jpg",
            Self::FailedPermission => "failed permission",
            Self::FailedCollision => "failed collision",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryRowModel {
    pub label: String,
    pub files: usize,
    pub bytes: String,
    pub percent: String,
    pub time: String,
}

impl CategoryRowModel {
    pub fn new(
        label: impl Into<String>,
        files: usize,
        bytes: impl Into<String>,
        percent: impl Into<String>,
        time: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            files,
            bytes: bytes.into(),
            percent: percent.into(),
            time: time.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorRowModel {
    pub target: String,
    pub phase: String,
    pub file: String,
    pub error: String,
}

impl ErrorRowModel {
    pub fn new(
        target: impl Into<String>,
        phase: impl Into<String>,
        file: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            target: target.into(),
            phase: phase.into(),
            file: file.into(),
            error: error.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetProgressRowModel {
    pub target: String,
    pub percent: usize,
    pub bytes: String,
    pub rate: String,
    pub active_workers: usize,
}

impl TargetProgressRowModel {
    pub fn new(
        target: impl Into<String>,
        percent: usize,
        bytes: impl Into<String>,
        rate: impl Into<String>,
        active_workers: usize,
    ) -> Self {
        Self {
            target: target.into(),
            percent,
            bytes: bytes.into(),
            rate: rate.into(),
            active_workers,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetResultRowModel {
    pub target: String,
    pub planned: usize,
    pub copied: usize,
    pub verified: usize,
    pub copy_failed: usize,
    pub verify_failed: usize,
}

impl TargetResultRowModel {
    pub fn new(
        target: impl Into<String>,
        planned: usize,
        copied: usize,
        verified: usize,
        copy_failed: usize,
        verify_failed: usize,
    ) -> Self {
        Self {
            target: target.into(),
            planned,
            copied,
            verified,
            copy_failed,
            verify_failed,
        }
    }
}

/// Staged mode only (R12): staging stats for the post-run summary --
/// staged files/bytes and peak spool usage always present for a staged
/// run, `released_after` present only once the source-release milestone
/// (R2) actually fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagingSummaryModel {
    pub staged_files: usize,
    pub staged_bytes: String,
    pub peak_spool_bytes: String,
    pub released_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveScreenModel {
    pub job_name: String,
    pub status: String,
    pub summary: Vec<SummaryMetric>,
    pub overall_label: String,
    pub overall_progress: ProgressBarModel,
    pub overall_progress_text: String,
    pub phase_label: String,
    pub workers: Vec<WorkerRowModel>,
    pub target_progress: Vec<TargetProgressRowModel>,
    /// Staged mode only (R2): the "source released" milestone text from
    /// [`source_release_banner`], or `None` before release / outside staged
    /// mode. Rendered as a prominent extra line when present, so direct
    /// (non-staged) runs -- which never observe `WorkerEvent::SourceReleased`
    /// -- render identically to before this field existed.
    pub release_banner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostRunScreenModel {
    pub job_name: String,
    pub status: String,
    pub summary: Vec<SummaryMetric>,
    pub completion_label: String,
    pub completion_progress: ProgressBarModel,
    pub categories: Vec<CategoryRowModel>,
    pub target_results: Vec<TargetResultRowModel>,
    pub errors: Vec<ErrorRowModel>,
    pub copied_preview_count: usize,
    pub copied_preview_total: usize,
    /// Staged mode only (R12, display half): the same release milestone
    /// text as [`LiveScreenModel::release_banner`], carried into the
    /// post-run screen so it stays visible after the live view is replaced.
    pub release_banner: Option<String>,
    /// Staged mode only (R12): `None` for direct (non-staged) runs and for
    /// staged runs with no staging activity, so existing output stays
    /// byte-for-byte unchanged unless a run actually staged something.
    pub staging: Option<StagingSummaryModel>,
}

impl ProgressSnapshot {
    pub fn outcome(&self) -> Outcome {
        if self.failed {
            Outcome::Failure
        } else {
            Outcome::Success
        }
    }
}

pub fn phase_label(phase: PhaseKind) -> &'static str {
    match phase {
        PhaseKind::Adaptive => "adaptive",
        PhaseKind::LargeFiles => "large files",
        PhaseKind::SmallFiles => "small files",
        PhaseKind::Staging => "staged relay",
    }
}

/// Run-level source-release status (R2), tracked alongside the per-tick
/// [`ProgressSnapshot`] but kept as its own small state machine rather than
/// a field on it: it only ever moves forward once, on
/// `WorkerEvent::SourceReleased`, and doesn't participate in the
/// bytes/files bookkeeping every `ProgressSnapshot` field otherwise shares.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SourceReleaseState {
    #[default]
    Pending,
    Released {
        had_failures: bool,
    },
}

impl SourceReleaseState {
    pub fn is_released(&self) -> bool {
        matches!(self, Self::Released { .. })
    }
}

/// Applies a `WorkerEvent::SourceReleased { had_failures }` observation to
/// the current release state. Idempotent and monotonic: once `Released`,
/// further calls never regress to `Pending`, and `had_failures` is OR'd in
/// rather than overwritten, so a later call can't downgrade an
/// already-observed failure back to a clean release. In practice
/// `StagingReleaseTracker` (`src/copy.rs`) fires the underlying event
/// exactly once per run, but the render loop applies this defensively.
pub fn apply_source_released(
    current: SourceReleaseState,
    had_failures: bool,
) -> SourceReleaseState {
    match current {
        SourceReleaseState::Released {
            had_failures: existing,
        } => SourceReleaseState::Released {
            had_failures: existing || had_failures,
        },
        SourceReleaseState::Pending => SourceReleaseState::Released { had_failures },
    }
}

/// The human-facing "source released" milestone text (R2), or `None` while
/// still pending. `had_failures` selects a distinct wording noting that not
/// every file made it off the source cleanly, without attempting an exact
/// failure count -- the render loop that observes this doesn't reliably
/// know how many of the failures already reported were staging failures
/// specifically versus later target-side ones.
pub fn source_release_banner(state: SourceReleaseState) -> Option<String> {
    match state {
        SourceReleaseState::Pending => None,
        SourceReleaseState::Released {
            had_failures: false,
        } => Some("source released \u{2014} safe to disconnect".to_string()),
        SourceReleaseState::Released { had_failures: true } => {
            Some("source released \u{2014} with staging failures, safe to disconnect".to_string())
        }
    }
}

pub fn active_worker_slots(configured_parallel: usize, phase_task_count: usize) -> usize {
    if phase_task_count == 0 {
        0
    } else {
        configured_parallel.max(1).min(phase_task_count)
    }
}

pub fn eta(bytes_done: u64, bytes_total: u64, elapsed: Duration) -> Option<Duration> {
    if bytes_total <= bytes_done || bytes_done == 0 || elapsed.is_zero() {
        return None;
    }

    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return None;
    }

    let rate = bytes_done as f64 / seconds;
    if rate <= 0.0 {
        return None;
    }

    let remaining = bytes_total.saturating_sub(bytes_done) as f64;
    let eta_seconds = remaining / rate;
    if eta_seconds <= 0.0 {
        return None;
    }

    Some(Duration::from_secs_f64(eta_seconds))
}

pub fn overall_message(snapshot: &ProgressSnapshot) -> String {
    let outcome = match snapshot.outcome() {
        Outcome::Failure => "copy failed",
        Outcome::Success
            if snapshot.completed >= snapshot.task_count && snapshot.active_workers == 0 =>
        {
            "copy complete"
        }
        Outcome::Success => match snapshot.phase {
            PhaseKind::LargeFiles => "copying large files",
            PhaseKind::SmallFiles => "copying small files",
            PhaseKind::Adaptive => "copying files",
            PhaseKind::Staging => "relaying via spool",
        },
    };

    let mut message = format!(
        "{outcome} | {}/{} files | {} active | {} / {} | rate {}",
        snapshot.completed,
        snapshot.task_count,
        snapshot.active_workers,
        human_bytes(snapshot.bytes_done),
        human_bytes(snapshot.bytes_total),
        human_rate(snapshot.bytes_done, snapshot.elapsed),
    );

    if let Some(eta) = eta(snapshot.bytes_done, snapshot.bytes_total, snapshot.elapsed) {
        message.push_str(&format!(" | eta {}", format_duration(eta)));
    }

    message
}
