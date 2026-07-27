use pathsync::progress_model::{
    CategoryRowModel, ErrorRowModel, PhaseKind, ProgressBarModel, ProgressSnapshot,
    SourceReleaseState, SummaryMetric, TargetResultRowModel, TransferCategory, WorkerRowModel,
    active_worker_slots, apply_source_released, eta, overall_message, phase_label,
    source_release_banner,
};
use std::time::Duration;

#[test]
fn phase_labels_are_human_readable() {
    assert_eq!(phase_label(PhaseKind::LargeFiles), "large files");
    assert_eq!(phase_label(PhaseKind::SmallFiles), "small files");
    assert_eq!(phase_label(PhaseKind::Staging), "staged relay");
}

#[test]
fn source_release_state_starts_pending_with_no_banner() {
    let state = SourceReleaseState::default();
    assert_eq!(state, SourceReleaseState::Pending);
    assert!(!state.is_released());
    assert_eq!(source_release_banner(state), None);
}

#[test]
fn apply_source_released_transitions_to_released_exactly_on_the_event() {
    let released = apply_source_released(SourceReleaseState::Pending, false);
    assert_eq!(
        released,
        SourceReleaseState::Released {
            had_failures: false
        }
    );
    assert!(released.is_released());
}

#[test]
fn apply_source_released_distinguishes_clean_from_with_failures() {
    let clean = apply_source_released(SourceReleaseState::Pending, false);
    let with_failures = apply_source_released(SourceReleaseState::Pending, true);

    let clean_banner = source_release_banner(clean).expect("clean release must have a banner");
    let failed_banner =
        source_release_banner(with_failures).expect("failed release must have a banner");

    assert!(clean_banner.contains("source released"));
    assert!(!clean_banner.contains("with staging failures"));
    assert!(failed_banner.contains("source released"));
    assert!(failed_banner.contains("with staging failures"));
}

#[test]
fn apply_source_released_never_regresses_back_to_pending() {
    let released = apply_source_released(SourceReleaseState::Pending, false);
    // A second observation (defensive against a hypothetical duplicate
    // event) must stay released, not bounce back to `Pending`.
    let still_released = apply_source_released(released, false);
    assert!(still_released.is_released());
    assert_eq!(still_released, released);
}

#[test]
fn apply_source_released_keeps_a_failure_once_observed() {
    let with_failures = apply_source_released(SourceReleaseState::Pending, true);
    // A later call reporting `had_failures: false` (shouldn't happen in
    // practice -- the tracker fires once -- but the model stays defensive)
    // must not erase the already-recorded failure.
    let still_failed = apply_source_released(with_failures, false);
    assert_eq!(
        still_failed,
        SourceReleaseState::Released { had_failures: true }
    );
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

    let staging = ProgressSnapshot {
        phase: PhaseKind::Staging,
        ..in_progress
    };
    assert!(overall_message(&staging).contains("relaying via spool"));
}

#[test]
fn canonical_screen_model_constructors_preserve_display_values() {
    let metric = SummaryMetric::new("Scanned", "2,941");
    let progress = ProgressBarModel::new(43, 24);
    let active = WorkerRowModel::active('⠋', "T01", 64, "clip.mp4", "8.2 GB", "4s", "T7");
    let idle = WorkerRowModel::idle("T04");
    let category = CategoryRowModel::new("copied mp4", 204, "128.4 GB", "67.1%", "16m09s");
    let error = ErrorRowModel::new("Archive", "copy", "GX010193.MP4", "permission denied");
    let target_result = TargetResultRowModel::new("T7", 10, 9, 8, 1, 1);

    assert_eq!(metric.label, "Scanned");
    assert_eq!(metric.value, "2,941");
    assert_eq!(progress.percent, 43);
    assert_eq!(progress.width, 24);
    assert_eq!(active.spinner_frame, Some('⠋'));
    assert_eq!(active.worker_tag, "T01");
    assert_eq!(active.percent, 64);
    assert_eq!(active.target, "T7");
    assert!(!active.idle);
    assert_eq!(idle.item, "idle");
    assert_eq!(idle.spinner_frame, None);
    assert!(idle.idle);
    assert_eq!(category.label, "copied mp4");
    assert_eq!(category.files, 204);
    assert_eq!(error.target, "Archive");
    assert_eq!(error.phase, "copy");
    assert_eq!(error.file, "GX010193.MP4");
    assert_eq!(error.error, "permission denied");
    assert_eq!(target_result.target, "T7");
    assert_eq!(target_result.planned, 10);
    assert_eq!(target_result.copied, 9);
    assert_eq!(target_result.verified, 8);
    assert_eq!(target_result.copy_failed, 1);
    assert_eq!(target_result.verify_failed, 1);
}

#[test]
fn error_row_models_preserve_target_specific_details() {
    let error = ErrorRowModel::new(
        "Archive",
        "verify",
        "GX010193.MP4",
        "/Volumes/Archive/Vlog/2026/03/GX010193.MP4: permission denied",
    );

    assert_eq!(error.target, "Archive");
    assert_eq!(error.phase, "verify");
    assert_eq!(error.file, "GX010193.MP4");
    assert_eq!(
        error.error,
        "/Volumes/Archive/Vlog/2026/03/GX010193.MP4: permission denied"
    );
}

#[test]
fn transfer_category_labels_match_canonical_ui_taxonomy() {
    assert_eq!(
        TransferCategory::SkippedExisting.as_label(),
        "skipped existing"
    );
    assert_eq!(TransferCategory::CopiedMp4.as_label(), "copied mp4");
    assert_eq!(TransferCategory::CopiedJpg.as_label(), "copied jpg");
    assert_eq!(
        TransferCategory::FailedPermission.as_label(),
        "failed permission"
    );
    assert_eq!(
        TransferCategory::FailedCollision.as_label(),
        "failed collision"
    );
}
