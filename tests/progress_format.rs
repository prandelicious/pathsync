use std::path::Path;

use pathsync::progress_format::{
    CANONICAL_WIDTH, GlyphSet, render_live_screen, render_live_screen_with_width,
    render_live_screen_with_width_and_glyphs, render_may4_live_screen_with_width,
    render_post_run_screen, render_post_run_screen_with_glyphs, worker_label,
};
use pathsync::progress_model::{
    CategoryRowModel, ErrorRowModel, LiveScreenModel, ProgressBarModel, SummaryMetric,
    TargetProgressRowModel, TargetResultRowModel, TransferCategory, TransferRowPhase,
    WorkerRowModel,
};

fn live_model() -> LiveScreenModel {
    LiveScreenModel {
        job_name: "vlog-sync".to_string(),
        status: "LIVE / COPY-LARGE".to_string(),
        summary: vec![
            SummaryMetric::new("Scanned", "2,941"),
            SummaryMetric::new("Planned", "318"),
            SummaryMetric::new("Copied", "141"),
            SummaryMetric::new("Verified", "141"),
            SummaryMetric::new("Failed", "1"),
            SummaryMetric::new("Bytes", "58.2 GB / 133.0 GB"),
            SummaryMetric::new("Rate", "142.4 MB/s"),
            SummaryMetric::new("Elapsed", "7m08s"),
            SummaryMetric::new("ETA", "8m46s"),
            SummaryMetric::new("Targets", "2"),
        ],
        overall_label: "Copying".to_string(),
        overall_progress: ProgressBarModel::new(43, 30),
        overall_progress_text: "58.2 GB copied of 133.0 GB   ETA 8m46s".to_string(),
        phase_label: "overall copying large files".to_string(),
        workers: vec![
            WorkerRowModel::active_with_phase(
                '⠋',
                "T01",
                TransferRowPhase::Hashing,
                64,
                "A001_C014_0101AB.MP4",
                "8.2 GB",
                "78.4 MB/s",
                "T7",
            ),
            WorkerRowModel::active(
                '⠙',
                "T02",
                51,
                "A001_C015_0101AB.MP4",
                "7.9 GB",
                "64.0 MB/s",
                "Archive",
            ),
            WorkerRowModel::active_with_phase(
                '⠹',
                "T03",
                TransferRowPhase::Verifying,
                12,
                "GX010193.MP4",
                "2.1 GB",
                "41.8 MB/s",
                "T7",
            ),
            WorkerRowModel::idle("T04"),
        ],
        target_progress: vec![
            TargetProgressRowModel::new("T7", 47, "31.0 GB / 66.5 GB", "78.4 MB/s", 2),
            TargetProgressRowModel::new("Archive", 41, "27.2 GB / 66.5 GB", "64.0 MB/s", 1),
        ],
        release_banner: None,
    }
}

fn post_run_model() -> PostRunScreenModel {
    PostRunScreenModel {
        job_name: "vlog-sync".to_string(),
        status: "ATTENTION".to_string(),
        summary: vec![
            SummaryMetric::new("Scanned", "2,941"),
            SummaryMetric::new("Planned", "318"),
            SummaryMetric::new("Copied", "316"),
            SummaryMetric::new("Verified", "314"),
            SummaryMetric::new("Failed", "3"),
            SummaryMetric::new("Bytes", "129.5 GB / 131.6 GB"),
            SummaryMetric::new("Rate", "121.7 MB/s"),
            SummaryMetric::new("Elapsed", "18m01s"),
            SummaryMetric::new("ETA", "--"),
            SummaryMetric::new("Targets", "2"),
        ],
        completion_label: "Verified".to_string(),
        completion_progress: ProgressBarModel::new(99, 30),
        categories: vec![
            CategoryRowModel::new("skipped existing", 2623, "0 B", "100.0%", "0.0s"),
            CategoryRowModel::new("copied mp4", 204, "128.4 GB", "67.1%", "16m09s"),
            CategoryRowModel::new("copied jpg", 112, "3.2 GB", "72.8%", "1m04s"),
            CategoryRowModel::new("failed permission", 1, "14.2 MB", "0.0%", "--"),
            CategoryRowModel::new("failed collision", 1, "8.7 MB", "0.0%", "--"),
        ],
        target_results: vec![
            TargetResultRowModel::new("T7", 159, 159, 159, 0, 0),
            TargetResultRowModel::new("Archive", 159, 157, 155, 1, 2),
        ],
        errors: vec![
            ErrorRowModel::new("Archive", "copy", "GX010194.MP4", "permission denied"),
            ErrorRowModel::new("Archive", "verify", "GX010193.MP4", "signature mismatch"),
        ],
        copied_preview: vec![],
        copied_preview_count: 20,
        copied_preview_total: 316,
        release_banner: None,
        staging: None,
    }
}

use pathsync::progress_model::PostRunScreenModel;

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

fn may4_transfer_section(lines: &[String]) -> Vec<&String> {
    let start = lines
        .iter()
        .position(|line| line.contains("Active transfers"))
        .expect("Active transfers heading")
        + 1;
    lines[start..]
        .iter()
        .take_while(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.contains("Targets") && !line.contains("─")
        })
        .collect()
}

fn is_may4_stacked_metrics_line(line: &str) -> bool {
    let indent = line.chars().take_while(|ch| *ch == ' ').count();
    indent >= 4 && !line.contains("idle")
}

fn char_index(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .map(|byte_index| haystack[..byte_index].chars().count())
        .unwrap_or_else(|| panic!("{needle:?} not found in {haystack:?}"))
}

fn may4_transfer_block<'a>(section: &[&'a String], tag: &str) -> Vec<&'a String> {
    let idx = section
        .iter()
        .position(|line| line.contains(tag))
        .unwrap_or_else(|| panic!("{tag} transfer row"));
    let mut block = vec![section[idx]];
    if let Some(next) = section.get(idx + 1)
        && is_may4_stacked_metrics_line(next)
    {
        block.push(*next);
    }
    block
}

#[test]
fn may4_live_preview_uses_full_width_metrics_without_run_box() {
    let lines = render_may4_live_screen_with_width(&live_model(), 140);
    let rendered = lines.join("\n");

    assert!(lines.iter().all(|line| line.chars().count() == 140));
    assert!(rendered.contains("58.2 / 133.0 GB"));
    assert!(rendered.contains("Active transfers"));
    assert!(rendered.contains("Copying large files"));
    assert!(!rendered.contains("┌ Run "));
    assert!(!rendered.contains("Workers"));
}

#[test]
fn may4_live_preview_keeps_full_archive_destination_at_width_90() {
    let lines = render_may4_live_screen_with_width(&live_model(), 90);
    let section = may4_transfer_section(&lines);
    let t02 = may4_transfer_block(&section, "T02");
    let t02_text = t02
        .iter()
        .map(|line| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        t02_text.contains("Archive"),
        "T02 destination must keep full Archive, got:\n{t02_text}"
    );
}

#[test]
fn may4_live_preview_keeps_size_and_rate_unit_columns_stable_when_qty_grows() {
    for width in [100usize, 140] {
        let mut small = live_model();
        small.workers[0].size = "9.9 GB".to_string();
        small.workers[0].time = "99.0 MB/s".to_string();
        let mut large = small.clone();
        large.workers[0].size = "10.0 GB".to_string();
        large.workers[0].time = "101.0 MB/s".to_string();

        let small_line = may4_transfer_block(
            &may4_transfer_section(&render_may4_live_screen_with_width(&small, width)),
            "T01",
        )[0]
        .clone();
        let large_line = may4_transfer_block(
            &may4_transfer_section(&render_may4_live_screen_with_width(&large, width)),
            "T01",
        )[0]
        .clone();

        assert_eq!(
            char_index(&small_line, "GB"),
            char_index(&large_line, "GB"),
            "GB shifted at width {width}:\n{small_line}\n{large_line}"
        );
        assert_eq!(
            char_index(&small_line, "MB/s"),
            char_index(&large_line, "MB/s"),
            "MB/s shifted at width {width}:\n{small_line}\n{large_line}"
        );
        assert_eq!(
            char_index(&small_line, "T7"),
            char_index(&large_line, "T7"),
            "dest shifted at width {width}:\n{small_line}\n{large_line}"
        );
        assert_eq!(
            char_index(&small_line, "A001_C014"),
            char_index(&large_line, "A001_C014"),
            "filename shifted at width {width}:\n{small_line}\n{large_line}"
        );

        let idle_line = may4_transfer_block(
            &may4_transfer_section(&render_may4_live_screen_with_width(&small, width)),
            "T04",
        )[0]
        .clone();
        let gb = char_index(&small_line, "GB");
        let idle_qty: String = idle_line
            .chars()
            .skip(gb.saturating_sub(6))
            .take(5)
            .collect();
        assert_eq!(
            idle_qty, "   --",
            "idle size qty should share the active size slot at width {width}:\n{small_line}\n{idle_line}"
        );
    }
}

#[test]
fn may4_live_preview_keeps_oneline_truncated_filename_and_visible_dest_at_width_80() {
    let mut model = live_model();
    model.workers[0].item =
        "DCIM/100GOPRO/very_long_clip_directory_name/A001_C014_0101AB.MP4".to_string();

    let lines = render_may4_live_screen_with_width(&model, 80);
    assert!(lines.iter().all(|line| line.chars().count() == 80));

    let section = may4_transfer_section(&lines);
    let t01 = may4_transfer_block(&section, "T01");
    assert_eq!(
        t01.len(),
        1,
        "long filename stays one-line when dest and filename mins fit: {t01:?}"
    );
    let line = t01[0];
    assert!(line.contains("T01"), "worker: {line}");
    assert!(line.contains("hashing"), "phase: {line}");
    assert!(line.contains("T7"), "destination stays visible: {line}");
    assert!(
        line.contains('…') || line.contains("MP4"),
        "filename truncated or tail visible: {line}"
    );
    assert!(
        char_index(line, "T7") < char_index(line, "DCIM").min(char_index(line, "MP4")),
        "dest before filename: {line}"
    );

    let t04 = may4_transfer_block(&section, "T04");
    assert_eq!(t04.len(), 1, "idle workers stay one line: {t04:?}");
    assert!(t04[0].contains("idle"), "idle label: {}", t04[0]);
}

#[test]
fn may4_live_preview_phase_width_does_not_shift_later_columns() {
    let mut model = live_model();
    model.workers[1] = WorkerRowModel::active(
        '⠙',
        "T02",
        51,
        "A001_C015_0101AB.MP4",
        "7.9 GB",
        "64.0 MB/s",
        "Archive",
    );
    model.workers[2] = WorkerRowModel::active_with_phase(
        '⠹',
        "T03",
        TransferRowPhase::Verifying,
        12,
        "A001_C015_0101AB.MP4",
        "7.9 GB",
        "64.0 MB/s",
        "Archive",
    );

    let lines = render_may4_live_screen_with_width(&model, 100);
    let section = may4_transfer_section(&lines);
    let copying = may4_transfer_block(&section, "T02")[0];
    let verifying = may4_transfer_block(&section, "T03")[0];

    assert!(copying.contains("copying"), "copying phase: {copying}");
    assert!(
        verifying.contains("verifying"),
        "verifying phase: {verifying}"
    );
    assert_eq!(
        char_index(copying, "Archive"),
        char_index(verifying, "Archive"),
        "dest shifted:\n{copying}\n{verifying}"
    );
    assert_eq!(
        char_index(copying, "A001_C015_0101AB.MP4"),
        char_index(verifying, "A001_C015_0101AB.MP4"),
        "filename shifted:\n{copying}\n{verifying}"
    );
    assert_eq!(
        char_index(copying, "GB"),
        char_index(verifying, "GB"),
        "size shifted:\n{copying}\n{verifying}"
    );
}

#[test]
fn may4_live_preview_keeps_oneline_transfers_with_rate_at_width_140() {
    let lines = render_may4_live_screen_with_width(&live_model(), 140);
    assert!(lines.iter().all(|line| line.chars().count() == 140));

    let section = may4_transfer_section(&lines);
    let t01 = may4_transfer_block(&section, "T01");
    let t02 = may4_transfer_block(&section, "T02");
    let t03 = may4_transfer_block(&section, "T03");

    for block in [&t01, &t02, &t03] {
        assert_eq!(
            block.len(),
            1,
            "active transfers stay one-line at 140: {block:?}"
        );
    }
    assert!(
        t01[0].contains("78.4 MB/s") && t01[0].contains("T7"),
        "{}",
        t01[0]
    );
    assert!(
        char_index(t01[0], "T7") < char_index(t01[0], "A001_C014"),
        "dest before filename: {}",
        t01[0]
    );
    assert!(
        t02[0].contains("64.0 MB/s") && t02[0].contains("Archive"),
        "{}",
        t02[0]
    );
    assert!(
        char_index(t02[0], "Archive") < char_index(t02[0], "A001_C015"),
        "dest before filename: {}",
        t02[0]
    );
    assert!(
        t03[0].contains("41.8 MB/s") && t03[0].contains("T7"),
        "{}",
        t03[0]
    );
}

#[test]
fn narrow_live_progress_line_drops_eta_before_truncating_byte_counts() {
    let lines = render_live_screen_with_width(&live_model(), 80);
    let progress = lines
        .iter()
        .find(|line| line.contains("Copying  ["))
        .expect("overall progress line");
    assert!(progress.contains("copied of"));
    assert!(!progress.ends_with("ETA 8"));
    assert!(progress.contains("133.0 GB"));
}

#[test]
fn post_run_completion_line_omits_dead_eta_placeholder() {
    let rendered = render_post_run_screen(&post_run_model()).join("\n");
    assert!(rendered.contains("verified"));
    assert!(!rendered.contains("ETA --"));
}

#[test]
fn narrow_live_screen_renders_stacked_80_column_layout() {
    let lines = render_live_screen(&live_model());
    let rendered = lines.join("\n");

    assert!(
        lines
            .iter()
            .all(|line| line.chars().count() == CANONICAL_WIDTH)
    );
    assert_eq!(lines[0], exact_header("vlog-sync", "LIVE / COPY-LARGE"));
    assert!(rendered.contains("Scanned: 2,941"));
    assert!(rendered.contains("Copying  ["));
    assert!(rendered.contains("overall copying large files"));
    assert!(rendered.contains("⠋ T01"));
    assert!(rendered.contains("hashing"));
    assert!(rendered.contains("A001_C014_0101AB.MP4"));
    assert!(rendered.contains("T7"));
    assert!(!rendered.contains("┌ Run "));
    assert!(rendered.contains("Targets"));
}

#[test]
fn wide_live_screen_renders_worker_first_with_target_strip_and_core_run_box() {
    let lines = render_live_screen_with_width(&live_model(), 120);
    let rendered = lines.join("\n");

    assert!(lines.iter().all(|line| line.chars().count() == 120));
    assert!(rendered.contains("Workers"));
    assert!(rendered.contains("⠋ T01  hashing"));
    assert!(rendered.contains("78.4 MB/s"));
    assert!(rendered.contains("T7"));
    assert!(rendered.contains("Targets"));
    assert!(rendered.contains("31.0 GB / 66.5 GB"));
    assert!(rendered.contains("27.2 GB / 66.5 GB"));
    assert!(rendered.contains("┌ Run "));
    assert!(rendered.contains("│ Verified"));
    assert!(rendered.contains("141"));
    assert!(rendered.contains("│ Targets"));
    assert!(!rendered.contains("Copy W"));
    assert!(!rendered.contains("Verify W"));
}

#[test]
fn ascii_glyph_fallback_reuses_layout_without_unicode_symbols() {
    let live = render_live_screen_with_width_and_glyphs(&live_model(), 120, GlyphSet::Ascii);
    let summary = render_post_run_screen_with_glyphs(&post_run_model(), GlyphSet::Ascii);
    let rendered = format!("{}\n{}", live.join("\n"), summary.join("\n"));

    assert!(rendered.contains("+ Run "));
    assert!(rendered.contains("[###"));
    assert!(rendered.contains("| Verified"));
    assert!(!rendered.contains('┌'));
    assert!(!rendered.contains('│'));
    assert!(!rendered.contains('─'));
    assert!(!rendered.contains('█'));
    assert!(!rendered.contains('⠋'));
}

#[test]
fn narrow_live_screen_uses_stacked_fallback_without_run_box() {
    let mut model = live_model();
    model.workers[0].item = "VID_20260420_4609_121.mp4".to_string();
    model.workers[0].target = "My Passport".to_string();

    let lines = render_live_screen_with_width(&model, 80);
    let rendered = lines.join("\n");

    assert!(lines.iter().all(|line| line.chars().count() == 80));
    assert!(!rendered.contains("┌ Run "));
    assert!(rendered.contains("Targets"));
    assert!(rendered.contains("VID_20260420"));
    assert!(rendered.contains("My Passport"));
    assert!(rendered.contains("hashing"));
}

#[test]
fn live_screen_pads_worker_section_to_four_visible_slots() {
    let mut model = live_model();
    model.workers.truncate(2);

    let lines = render_live_screen(&model);

    let rendered = lines.join("\n");
    assert!(rendered.contains("T03"));
    assert!(rendered.contains("T04"));
    assert!(rendered.contains("idle"));
}

#[test]
fn live_screen_uses_requested_width_for_readable_worker_targets() {
    let mut model = live_model();
    model.workers[0].item = "VID_20260420_4609_121.mp4".to_string();
    model.workers[0].target = "My Passport".to_string();

    let lines = render_live_screen_with_width(&model, 120);

    assert!(lines.iter().all(|line| line.chars().count() == 120));
    assert!(lines.iter().any(|line| line.contains("┌ Run ")));
    assert!(lines.iter().any(|line| line.contains("│ Scanned")));
    assert!(lines.iter().any(|line| line.contains("│ Targets")));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("VID_20260420_4609_121.mp4") && line.contains("My Passport"))
    );
}

#[test]
fn live_screen_uses_independent_worker_spinner_prefixes() {
    let mut model = live_model();
    model.workers[0].spinner_frame = Some('⠹');
    model.workers[1].spinner_frame = Some('⠧');

    let lines = render_live_screen(&model);

    assert!(lines[9].starts_with("⠹ "));
    assert!(lines[10].starts_with("⠧ "));
    assert!(!lines[8].starts_with("⠹ "));
}

#[test]
fn post_run_error_screen_renders_verification_first_80_column_layout() {
    let lines = render_post_run_screen(&post_run_model());
    let rendered = lines.join("\n");

    assert!(
        lines
            .iter()
            .all(|line| line.chars().count() == CANONICAL_WIDTH)
    );
    assert_eq!(lines[0], exact_header("vlog-sync", "ATTENTION"));
    assert!(rendered.contains("Verified  ["));
    assert!(rendered.contains("Target Results"));
    assert!(rendered.contains("T7"));
    assert!(rendered.contains("verified"));
    assert!(rendered.contains("Archive"));
    assert!(rendered.contains("attention"));
    assert!(rendered.contains("Failures"));
    assert!(rendered.contains("Breakdown"));
    assert!(rendered.contains("Copied file preview"));
}

#[test]
fn post_run_summary_is_verification_first_with_attention_rows_and_failures() {
    let lines = render_post_run_screen(&post_run_model());
    let rendered = lines.join("\n");

    assert!(rendered.contains("Pathsync (vlog-sync)"));
    assert!(rendered.contains("ATTENTION"));
    assert!(rendered.contains("Verified  ["));
    let target_index = rendered.find("Target Results").unwrap();
    let breakdown_index = rendered.find("Breakdown").unwrap();
    assert!(target_index < breakdown_index);
    assert!(rendered.contains("Target"));
    assert!(rendered.contains("Planned"));
    assert!(rendered.contains("Copied"));
    assert!(rendered.contains("Verified"));
    assert!(rendered.contains("Copy Fail"));
    assert!(rendered.contains("Verify Fail"));
    assert!(rendered.contains("Result"));
    assert!(rendered.contains("T7"));
    assert!(rendered.contains("verified"));
    assert!(rendered.contains("Archive"));
    assert!(rendered.contains("attention"));
    assert!(rendered.contains("Failures"));
    assert!(rendered.contains("Target"));
    assert!(rendered.contains("Phase"));
    assert!(rendered.contains("File"));
    assert!(rendered.contains("Archive"));
    assert!(rendered.contains("copy"));
    assert!(rendered.contains("GX010194.MP4"));
    assert!(rendered.contains("permission denied"));
    assert!(rendered.contains("Copied file preview"));
    assert!(rendered.contains("showing 20 of 316 copied files"));
}

#[test]
fn final_summary_accepts_verified_attention_and_failed_outcome_language() {
    for status in ["VERIFIED", "ATTENTION", "FAILED"] {
        let mut model = post_run_model();
        model.status = status.to_string();

        let lines = render_post_run_screen(&model);

        assert_eq!(lines[0], exact_header("vlog-sync", status));
        assert!(!lines[0].contains("warning"));
    }
}

#[test]
fn rendered_post_run_errors_keep_target_specific_destination_context() {
    let mut model = post_run_model();
    model.errors = vec![ErrorRowModel::new(
        "Archive",
        "verify",
        "GX010193.MP4",
        "/Volumes/Archive/Vlog/2026/03/GX010193.MP4: permission denied",
    )];

    let lines = render_post_run_screen(&model);
    let rendered = lines.join("\n");

    assert!(rendered.contains("Archive"));
    assert!(rendered.contains("permission denied"));
}

#[test]
fn narrow_live_screen_omits_release_banner_when_not_yet_released() {
    let lines = render_live_screen(&live_model());
    let rendered = lines.join("\n");

    assert!(!rendered.contains("source released"));
}

#[test]
fn narrow_live_screen_renders_release_banner_as_a_full_width_line() {
    let mut model = live_model();
    model.release_banner = Some("source released \u{2014} safe to disconnect".to_string());

    let lines = render_live_screen(&model);

    assert!(
        lines
            .iter()
            .all(|line| line.chars().count() == CANONICAL_WIDTH)
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("source released \u{2014} safe to disconnect"))
    );
}

#[test]
fn wide_live_screen_renders_release_banner_with_staging_failures_wording() {
    let mut model = live_model();
    model.release_banner =
        Some("source released \u{2014} with staging failures, safe to disconnect".to_string());

    let lines = render_live_screen_with_width(&model, 120);
    let rendered = lines.join("\n");

    assert!(lines.iter().all(|line| line.chars().count() == 120));
    assert!(rendered.contains("source released"));
    assert!(rendered.contains("with staging failures"));
}

#[test]
fn post_run_screen_renders_release_banner_when_present() {
    let mut model = post_run_model();
    model.release_banner = Some("source released \u{2014} safe to disconnect".to_string());

    let lines = render_post_run_screen(&model);
    let rendered = lines.join("\n");

    assert!(
        lines
            .iter()
            .all(|line| line.chars().count() == CANONICAL_WIDTH)
    );
    assert!(rendered.contains("source released"));
}

#[test]
fn post_run_screen_omits_release_banner_when_absent() {
    let lines = render_post_run_screen(&post_run_model());
    let rendered = lines.join("\n");

    assert!(!rendered.contains("source released"));
}

#[test]
fn transfer_category_labels_match_mockup_taxonomy() {
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
