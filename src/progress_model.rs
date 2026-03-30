use std::time::Duration;

use crate::format::{format_duration, human_bytes, human_rate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseKind {
    Adaptive,
    LargeFiles,
    SmallFiles,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRowModel {
    pub worker_tag: String,
    pub percent: usize,
    pub item: String,
    pub size: String,
    pub time: String,
    pub idle: bool,
}

impl WorkerRowModel {
    pub fn active(
        worker_tag: impl Into<String>,
        percent: usize,
        item: impl Into<String>,
        size: impl Into<String>,
        time: impl Into<String>,
    ) -> Self {
        Self {
            worker_tag: worker_tag.into(),
            percent,
            item: item.into(),
            size: size.into(),
            time: time.into(),
            idle: false,
        }
    }

    pub fn idle(worker_tag: impl Into<String>) -> Self {
        Self {
            worker_tag: worker_tag.into(),
            percent: 0,
            item: "idle".to_string(),
            size: "--".to_string(),
            time: "--".to_string(),
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
    pub label: String,
    pub detail: String,
}

impl ErrorRowModel {
    pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveScreenModel {
    pub job_name: String,
    pub status: String,
    pub summary: Vec<SummaryMetric>,
    pub overall_label: String,
    pub overall_progress: ProgressBarModel,
    pub phase_label: String,
    pub phase_progress_text: String,
    pub workers: Vec<WorkerRowModel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostRunScreenModel {
    pub job_name: String,
    pub status: String,
    pub summary: Vec<SummaryMetric>,
    pub completion_label: String,
    pub completion_progress: ProgressBarModel,
    pub categories: Vec<CategoryRowModel>,
    pub errors: Vec<ErrorRowModel>,
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
