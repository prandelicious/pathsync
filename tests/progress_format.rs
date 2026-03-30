use std::path::Path;

use pathsync::progress_format::{
    CANONICAL_WIDTH, render_live_screen, render_post_run_screen, worker_label,
};
use pathsync::progress_model::{
    CategoryRowModel, ErrorRowModel, LiveScreenModel, ProgressBarModel, SummaryMetric,
    TransferCategory, WorkerRowModel,
};

fn live_model() -> LiveScreenModel {
    LiveScreenModel {
        job_name: "vlog-sync".to_string(),
        status: "LIVE / COPY-LARGE".to_string(),
        summary: vec![
            SummaryMetric::new("Scanned", "2,941"),
            SummaryMetric::new("Planned", "318"),
            SummaryMetric::new("Copied", "141"),
            SummaryMetric::new("Failed", "1"),
            SummaryMetric::new("Bytes", "58.2 GB / 133.0 GB"),
            SummaryMetric::new("Rate", "142.4 MB/s"),
            SummaryMetric::new("Elapsed", "7m08s"),
            SummaryMetric::new("ETA", "8m46s"),
        ],
        overall_label: "Total copy progress".to_string(),
        overall_progress: ProgressBarModel::new(43, 24),
        phase_label: "overall  copying large files".to_string(),
        phase_progress_text: "58.2 GB / 133.0 GB".to_string(),
        workers: vec![
            WorkerRowModel::active("W01", 64, "A001_C014_0101AB.MP4", "8.2 GB", "4s"),
            WorkerRowModel::active("W02", 51, "A001_C015_0101AB.MP4", "7.9 GB", "4s"),
            WorkerRowModel::active("W03", 12, "GX010193.MP4", "2.1 GB", "4s"),
            WorkerRowModel::idle("W04"),
        ],
    }
}

fn post_run_model() -> PostRunScreenModel {
    PostRunScreenModel {
        job_name: "vlog-sync".to_string(),
        status: "COMPLETE WITH ERRORS".to_string(),
        summary: vec![
            SummaryMetric::new("Scanned", "2,941"),
            SummaryMetric::new("Planned", "318"),
            SummaryMetric::new("Copied", "316"),
            SummaryMetric::new("Failed", "2"),
            SummaryMetric::new("Bytes transferred", "131.6 GB"),
            SummaryMetric::new("Avg rate", "121.7 MB/s"),
            SummaryMetric::new("Elapsed", "18m01s"),
            SummaryMetric::new("Skip rate", "89.2%"),
        ],
        completion_label: "Copy completion".to_string(),
        completion_progress: ProgressBarModel::new(99, 24),
        categories: vec![
            CategoryRowModel::new("skipped existing", 2623, "0 B", "100.0%", "0.0s"),
            CategoryRowModel::new("copied mp4", 204, "128.4 GB", "67.1%", "16m09s"),
            CategoryRowModel::new("copied jpg", 112, "3.2 GB", "72.8%", "1m04s"),
            CategoryRowModel::new("failed permission", 1, "14.2 MB", "0.0%", "--"),
            CategoryRowModel::new("failed collision", 1, "8.7 MB", "0.0%", "--"),
        ],
        errors: vec![
            ErrorRowModel::new("[local] GX010193.MP4", "permission denied"),
            ErrorRowModel::new(
                "[local] GX010194.MP4",
                "destination collision after layout render",
            ),
        ],
    }
}

use pathsync::progress_model::PostRunScreenModel;

fn exact(line: &str) -> String {
    format!("{line:<width$}", width = CANONICAL_WIDTH)
}

fn exact_header(job_name: &str, status: &str) -> String {
    let left = format!("Pathsync ({job_name})");
    let gap = CANONICAL_WIDTH - left.chars().count() - status.chars().count();
    format!("{left}{}{status}", " ".repeat(gap))
}

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
fn live_large_file_screen_renders_exact_80_column_layout() {
    let lines = render_live_screen(&live_model());

    assert!(lines.iter().all(|line| line.chars().count() == CANONICAL_WIDTH));
    assert_eq!(
        lines[0],
        exact_header("vlog-sync", "LIVE / COPY-LARGE")
    );
    assert_eq!(lines[1], "────────────────────────────────────────────────────────────────────────────────");
    assert_eq!(
        lines[2],
        exact("Scanned: 2,941       Planned: 318        Copied: 141         Failed: 1")
    );
    assert_eq!(
        lines[3],
        exact("Bytes: 58.2 GB / 133.0 GB  Rate: 142.4 MB/s  Elapsed: 7m08s  ETA: 8m46s")
    );
    assert_eq!(
        lines[5],
        exact("Total copy progress  ██████████░░░░░░░░░░░░░░  43%")
    );
    assert_eq!(
        lines[7],
        exact("overall  copying large files                                  58.2 GB / 133.0 GB")
    );
    assert_eq!(
        lines[8],
        exact("W01  ██████████████░░░░░░░░  A001_C014_0101AB.MP4              8.2 GB        4s")
    );
    assert_eq!(
        lines[11],
        exact("W04  ░░░░░░░░░░░░░░░░░░░░░░  idle                                --        --")
    );
}

#[test]
fn post_run_error_screen_renders_exact_80_column_layout() {
    let lines = render_post_run_screen(&post_run_model());

    assert!(lines.iter().all(|line| line.chars().count() == CANONICAL_WIDTH));
    assert_eq!(
        lines[0],
        exact_header("vlog-sync", "COMPLETE WITH ERRORS")
    );
    assert_eq!(lines[1], "────────────────────────────────────────────────────────────────────────────────");
    assert_eq!(
        lines[2],
        exact("Scanned: 2,941       Planned: 318        Copied: 316         Failed: 2")
    );
    assert_eq!(
        lines[3],
        exact("Bytes transferred: 131.6 GB  Avg rate: 121.7 MB/s  Elapsed: 18m01s  Skip rate:")
    );
    assert_eq!(
        lines[5],
        exact("Copy completion      ███████████████████████░  99%")
    );
    assert_eq!(lines[7], exact("By Category"));
    assert_eq!(
        lines[9],
        exact("skipped existing      2,623 files        0 B   100.0%       0.0s")
    );
    assert_eq!(
        lines[13],
        exact("failed collision          1 file      8.7 MB     0.0%         --")
    );
    assert_eq!(lines[15], exact("Errors"));
    assert_eq!(
        lines[17],
        exact("[local] GX010193.MP4  permission denied")
    );
}

#[test]
fn transfer_category_labels_match_mockup_taxonomy() {
    assert_eq!(TransferCategory::SkippedExisting.as_label(), "skipped existing");
    assert_eq!(TransferCategory::CopiedMp4.as_label(), "copied mp4");
    assert_eq!(TransferCategory::CopiedJpg.as_label(), "copied jpg");
    assert_eq!(TransferCategory::FailedPermission.as_label(), "failed permission");
    assert_eq!(TransferCategory::FailedCollision.as_label(), "failed collision");
}
