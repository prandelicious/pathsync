use pathsync::progress_model::{
    PhaseKind, ProgressSnapshot, active_worker_slots, eta, overall_message, phase_label,
};
use std::time::Duration;

#[test]
fn phase_labels_are_human_readable() {
    assert_eq!(phase_label(PhaseKind::LargeFiles), "large files");
    assert_eq!(phase_label(PhaseKind::SmallFiles), "small files");
}

#[test]
fn eta_is_none_when_no_rate_is_available() {
    assert_eq!(eta(0, 100, Duration::from_secs(0)), None);
}

#[test]
fn eta_uses_elapsed_rate_when_progress_exists() {
    assert_eq!(
        eta(50, 100, Duration::from_secs(5)),
        Some(Duration::from_secs(5))
    );
}

#[test]
fn active_worker_slots_respects_phase_size() {
    assert_eq!(active_worker_slots(4, 0), 0);
    assert_eq!(active_worker_slots(4, 2), 2);
    assert_eq!(active_worker_slots(4, 10), 4);
}

#[test]
fn overall_message_reports_success_and_failure_outcomes() {
    let success = ProgressSnapshot {
        completed: 3,
        task_count: 3,
        active_workers: 0,
        bytes_done: 300,
        bytes_total: 300,
        elapsed: Duration::from_secs(3),
        phase: PhaseKind::SmallFiles,
        failed: false,
    };
    let failure = ProgressSnapshot {
        failed: true,
        ..success.clone()
    };
    let in_progress = ProgressSnapshot {
        completed: 1,
        task_count: 3,
        active_workers: 2,
        bytes_done: 100,
        bytes_total: 300,
        elapsed: Duration::from_secs(1),
        phase: PhaseKind::LargeFiles,
        failed: false,
    };

    assert!(overall_message(&success).contains("all copies complete"));
    assert!(!overall_message(&success).contains("phase"));
    assert!(overall_message(&failure).contains("copy failed"));
    assert!(overall_message(&in_progress).contains("copying"));
    assert!(overall_message(&in_progress).contains("2 active"));
}
