use pathsync::progress_model::{
    CategoryRowModel, ErrorRowModel, PhaseKind, ProgressBarModel, ProgressSnapshot, SummaryMetric,
    TransferCategory, WorkerRowModel, active_worker_slots, eta, overall_message, phase_label,
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

    assert!(overall_message(&success).contains("copy complete"));
    assert!(overall_message(&failure).contains("copy failed"));
    assert!(overall_message(&in_progress).contains("copying large files"));
    assert!(overall_message(&in_progress).contains("2 active"));
}

#[test]
fn canonical_screen_model_constructors_preserve_display_values() {
    let metric = SummaryMetric::new("Scanned", "2,941");
    let progress = ProgressBarModel::new(43, 24);
    let active = WorkerRowModel::active("W01", 64, "clip.mp4", "8.2 GB", "4s");
    let idle = WorkerRowModel::idle("W04");
    let category = CategoryRowModel::new("copied mp4", 204, "128.4 GB", "67.1%", "16m09s");
    let error = ErrorRowModel::new("[local] GX010193.MP4", "permission denied");

    assert_eq!(metric.label, "Scanned");
    assert_eq!(metric.value, "2,941");
    assert_eq!(progress.percent, 43);
    assert_eq!(progress.width, 24);
    assert_eq!(active.worker_tag, "W01");
    assert_eq!(active.percent, 64);
    assert!(!active.idle);
    assert_eq!(idle.item, "idle");
    assert!(idle.idle);
    assert_eq!(category.label, "copied mp4");
    assert_eq!(category.files, 204);
    assert_eq!(error.detail, "permission denied");
}

#[test]
fn transfer_category_labels_match_canonical_ui_taxonomy() {
    assert_eq!(TransferCategory::SkippedExisting.as_label(), "skipped existing");
    assert_eq!(TransferCategory::CopiedMp4.as_label(), "copied mp4");
    assert_eq!(TransferCategory::CopiedJpg.as_label(), "copied jpg");
    assert_eq!(TransferCategory::FailedPermission.as_label(), "failed permission");
    assert_eq!(TransferCategory::FailedCollision.as_label(), "failed collision");
}
