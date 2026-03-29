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
            "all copies complete"
        }
        Outcome::Success => "copying",
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
