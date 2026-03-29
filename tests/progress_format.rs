use std::path::Path;
use std::time::Duration;

use pathsync::progress_format::{
    overall_line, plain_progress_line, worker_label, worker_line, worker_prefix,
};
use pathsync::progress_model::{PhaseKind, ProgressSnapshot, phase_label};

#[test]
fn worker_labels_use_relative_path_to_disambiguate_duplicates() {
    let label = worker_label(
        "photo.jpg",
        Path::new("/media/cards/a/nested/photo.jpg"),
        Path::new("/media/cards/a"),
        36,
    );

    assert!(label.contains("nested"));
    assert!(label.contains("photo.jpg"));
}

#[test]
fn worker_labels_keep_prefix_and_filename_when_truncated() {
    let label = worker_label(
        "photo.jpg",
        Path::new("/media/cards/a/very/long/prefix/that/needs/truncation/photo.jpg"),
        Path::new("/media/cards/a"),
        24,
    );

    assert!(label.contains("very"));
    assert!(label.contains("photo.jpg"));
}

#[test]
fn overall_line_omits_phase_and_includes_eta_when_available() {
    let snapshot = ProgressSnapshot {
        phase: PhaseKind::LargeFiles,
        completed: 3,
        task_count: 10,
        active_workers: 2,
        bytes_done: 4 * 1024 * 1024,
        bytes_total: 10 * 1024 * 1024,
        elapsed: Duration::from_secs(4),
        failed: false,
    };

    let line = overall_line(&snapshot);
    assert!(!line.contains(phase_label(PhaseKind::LargeFiles)));
    assert!(line.contains("eta"));
}

#[test]
fn overall_line_reflects_failure_state() {
    let snapshot = ProgressSnapshot {
        phase: PhaseKind::SmallFiles,
        completed: 2,
        task_count: 2,
        active_workers: 0,
        bytes_done: 2048,
        bytes_total: 2048,
        elapsed: Duration::from_secs(2),
        failed: true,
    };

    let line = overall_line(&snapshot);
    assert!(line.contains("copy failed"));
}

#[test]
fn plain_text_progress_is_single_line() {
    let snapshot = ProgressSnapshot {
        phase: PhaseKind::SmallFiles,
        completed: 1,
        task_count: 2,
        active_workers: 1,
        bytes_done: 1024,
        bytes_total: 2048,
        elapsed: Duration::from_secs(1),
        failed: false,
    };

    let line = plain_progress_line(&snapshot);
    assert!(!line.contains('\n'));
    assert!(line.contains("1/2 files"));
    assert!(line.contains("1 active"));
}

#[test]
fn idle_worker_line_does_not_show_empty_numeric_fields() {
    let line = worker_line("waiting", 0, Duration::ZERO);
    assert!(line.contains("waiting"));
    assert!(!line.contains("rate          "));
}

#[test]
fn worker_prefix_is_compact_and_zero_padded() {
    assert_eq!(worker_prefix(0), "[W00]");
    assert_eq!(worker_prefix(7), "[W07]");
    assert_eq!(worker_prefix(12), "[W12]");
}
