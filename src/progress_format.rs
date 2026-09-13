use std::path::Path;
use std::time::Duration;

use crate::format::{human_bytes, human_rate};
use crate::progress_model::{
    LiveScreenModel, PostRunScreenModel, ProgressBarModel, TargetProgressRowModel, WorkerRowModel,
    overall_message, visible_worker_rows,
};
pub use crate::progress_model::{PhaseKind, ProgressSnapshot, phase_label};

pub const CANONICAL_WIDTH: usize = 80;
const LIVE_BAR_WIDTH: usize = 30;
const WORKER_BAR_WIDTH: usize = 18;
const TARGET_BAR_WIDTH: usize = 30;
const VISIBLE_WORKER_ROWS: usize = 4;
const VISIBLE_TARGET_ROWS: usize = 3;
const SUMMARY_RIGHT_COLUMN: usize = 27;
const WIDE_LAYOUT_MIN_WIDTH: usize = 110;
const WIDE_STATS_BOX_WIDTH: usize = 28;
const WIDE_LAYOUT_GUTTER: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphSet {
    Unicode,
    Ascii,
}

pub fn worker_label(display_name: &str, source: &Path, root: &Path, max_chars: usize) -> String {
    let relative = source
        .strip_prefix(root)
        .ok()
        .and_then(|path| path.to_str())
        .filter(|path| !path.is_empty())
        .unwrap_or(display_name);

    let candidate = if relative == display_name {
        display_name.to_string()
    } else {
        relative.to_string()
    };

    truncate_middle(&candidate, max_chars)
}

pub fn worker_prefix(worker: usize) -> String {
    worker_tag(worker)
}

pub fn worker_tag(worker: usize) -> String {
    format!("T{:02}", worker + 1)
}

pub fn overall_line(snapshot: &ProgressSnapshot) -> String {
    overall_message(snapshot)
}

pub fn plain_progress_line(snapshot: &ProgressSnapshot) -> String {
    overall_line(snapshot)
}

pub fn live_progress_line(snapshot: &ProgressSnapshot) -> String {
    format!("Total copy progress: {}", overall_message(snapshot))
}

pub fn post_run_progress_line(snapshot: &ProgressSnapshot) -> String {
    format!("Copy completion: {}", overall_message(snapshot))
}

pub fn worker_line(label: &str, bytes: u64, elapsed: Duration) -> String {
    if bytes == 0 && elapsed.is_zero() {
        return label.to_string();
    }

    format!(
        "{label} | {} | rate {}",
        human_bytes(bytes),
        human_rate(bytes, elapsed)
    )
}

pub fn worker_row(
    worker: usize,
    progress: &str,
    current_item: &str,
    size: Option<&str>,
    rate: Option<&str>,
    time: Option<&str>,
) -> String {
    let worker = worker_tag(worker);
    let item = truncate_middle(current_item, 30);
    let size = size.unwrap_or("--");
    let rate = rate.unwrap_or("--");
    let time = time.unwrap_or("--");

    format!("{worker:<4}  {progress:<18}  {item:<30}  {size:>8}  {rate:>8}  {time:>6}")
}

pub fn render_live_screen(model: &LiveScreenModel) -> Vec<String> {
    render_live_screen_with_width(model, CANONICAL_WIDTH)
}

pub fn render_live_screen_with_width(model: &LiveScreenModel, width: usize) -> Vec<String> {
    render_live_screen_with_width_and_glyphs(model, width, GlyphSet::Unicode)
}

pub fn render_live_screen_with_width_and_glyphs(
    model: &LiveScreenModel,
    width: usize,
    glyphs: GlyphSet,
) -> Vec<String> {
    let width = width.max(CANONICAL_WIDTH);
    let lines = if width >= WIDE_LAYOUT_MIN_WIDTH {
        render_live_screen_wide(model, width)
    } else {
        render_live_screen_narrow(model, width)
    };

    apply_glyphs(lines, glyphs)
}

fn render_live_screen_narrow(model: &LiveScreenModel, width: usize) -> Vec<String> {
    let mut lines = vec![
        header_line(&model.job_name, &model.status, width),
        divider(width),
    ];
    lines.extend(release_banner_lines(&model.release_banner, width));
    lines.extend([
        pad_to_width(&live_counts_row(&model.summary), width),
        pad_to_width(&live_bytes_rate_row(&model.summary), width),
        pad_to_width(&live_elapsed_eta_row(&model.summary), width),
        blank_line(width),
    ]);
    lines.push(progress_line(
        &model.overall_label,
        &model.overall_progress,
        Some(&model.overall_progress_text),
        width,
    ));
    lines.push(blank_line(width));
    lines.push(phase_line(&model.phase_label, width));

    for worker in visible_worker_rows(&model.workers, VISIBLE_WORKER_ROWS) {
        lines.push(render_worker_row(&worker, width));
    }
    append_target_progress_lines(&mut lines, model, width);
    lines.push(divider(width));

    lines
}

pub fn render_may4_live_screen_with_width(model: &LiveScreenModel, width: usize) -> Vec<String> {
    render_may4_live_screen_with_width_and_glyphs(model, width, GlyphSet::Unicode)
}

pub fn render_may4_live_screen_with_width_and_glyphs(
    model: &LiveScreenModel,
    width: usize,
    glyphs: GlyphSet,
) -> Vec<String> {
    let width = width.max(CANONICAL_WIDTH);
    apply_glyphs(render_may4_live_screen(model, width), glyphs)
}

fn render_may4_live_screen(model: &LiveScreenModel, width: usize) -> Vec<String> {
    let mut lines = vec![
        header_line(&model.job_name, &model.status, width),
        divider(width),
    ];
    lines.extend(release_banner_lines(&model.release_banner, width));
    lines.push(pad_to_width(&may4_primary_metrics_row(model, width), width));
    lines.push(pad_to_width(
        &may4_secondary_metrics_row(model, width),
        width,
    ));
    lines.push(blank_line(width));
    lines.push(pad_to_width(&may4_phase_title(&model.phase_label), width));
    let bar = progress_bar_string(
        model.overall_progress.percent,
        model.overall_progress.width.max(LIVE_BAR_WIDTH),
    );
    lines.push(pad_to_width(
        &format!(
            "{}   {}",
            bar,
            may4_progress_bytes_text(metric_value(&model.summary, "Bytes"))
        ),
        width,
    ));
    lines.push(blank_line(width));
    lines.push(pad_to_width("Active transfers", width));
    for worker in visible_worker_rows(&model.workers, VISIBLE_WORKER_ROWS) {
        lines.extend(render_may4_transfer_row(&worker, width));
    }
    if !model.target_progress.is_empty() {
        lines.push(blank_line(width));
        lines.push(pad_to_width("Targets", width));
        for target in model.target_progress.iter().take(VISIBLE_TARGET_ROWS) {
            lines.push(render_may4_target_progress_row(target, width));
        }
        if model.target_progress.len() > VISIBLE_TARGET_ROWS {
            lines.push(pad_to_width(
                &format!(
                    "... {} more targets",
                    model.target_progress.len() - VISIBLE_TARGET_ROWS
                ),
                width,
            ));
        }
    }
    lines.push(divider(width));
    lines
}

fn render_live_screen_wide(model: &LiveScreenModel, width: usize) -> Vec<String> {
    let left_width = width.saturating_sub(WIDE_STATS_BOX_WIDTH + WIDE_LAYOUT_GUTTER);
    let mut lines = vec![
        header_line(&model.job_name, &model.status, width),
        divider(width),
    ];
    lines.extend(release_banner_lines(&model.release_banner, width));
    lines.push(blank_line(width));

    let mut left = vec![
        progress_line(
            &model.overall_label,
            &model.overall_progress,
            Some(&model.overall_progress_text),
            left_width,
        ),
        phase_line(&model.phase_label, left_width),
        blank_line(left_width),
        pad_to_width("Workers", left_width),
    ];
    for worker in visible_worker_rows(&model.workers, VISIBLE_WORKER_ROWS) {
        left.push(render_worker_row(&worker, left_width));
    }
    if !model.target_progress.is_empty() {
        left.push(blank_line(left_width));
        left.push(pad_to_width("Targets", left_width));
        for target in model.target_progress.iter().take(VISIBLE_TARGET_ROWS) {
            left.push(render_target_progress_row(target, left_width));
        }
        if model.target_progress.len() > VISIBLE_TARGET_ROWS {
            left.push(pad_to_width(
                &format!(
                    "... {} more targets",
                    model.target_progress.len() - VISIBLE_TARGET_ROWS
                ),
                left_width,
            ));
        }
    }

    let right = live_stats_box(&model.summary);
    let row_count = left.len().max(right.len());
    for row in 0..row_count {
        let left_line = left.get(row).map(String::as_str).unwrap_or("");
        let right_line = right.get(row).map(String::as_str).unwrap_or("");
        lines.push(format!(
            "{}{}{}",
            pad_to_width(left_line, left_width),
            " ".repeat(WIDE_LAYOUT_GUTTER),
            pad_to_width(right_line, WIDE_STATS_BOX_WIDTH)
        ));
    }

    lines.push(divider(width));
    lines
}

/// Renders the staged-mode "source released" milestone (R2) as zero or one
/// full-width line. Zero lines when `banner` is `None` -- i.e. always, for
/// direct (non-staged) runs -- so existing layouts are byte-for-byte
/// unchanged unless a run actually observed `WorkerEvent::SourceReleased`.
fn release_banner_lines(banner: &Option<String>, width: usize) -> Vec<String> {
    banner
        .as_deref()
        .map(|text| vec![pad_to_width(text, width)])
        .unwrap_or_default()
}

fn live_stats_box(metrics: &[crate::progress_model::SummaryMetric]) -> Vec<String> {
    let inner_width = WIDE_STATS_BOX_WIDTH - 2;
    let title = " Run ";
    let rule_len = inner_width.saturating_sub(title.chars().count());
    let mut lines = vec![format!("┌{title}{}┐", "─".repeat(rule_len))];
    for label in [
        "Scanned", "Planned", "Copied", "Verified", "Failed", "Bytes", "Rate", "Elapsed", "ETA",
        "Targets",
    ] {
        lines.push(stats_box_row(
            label,
            metric_value(metrics, label),
            inner_width,
        ));
    }
    lines.push(format!("└{}┘", "─".repeat(inner_width)));
    lines
}

fn stats_box_row(label: &str, value: &str, inner_width: usize) -> String {
    let label_width = 9;
    let content_width = inner_width.saturating_sub(2);
    let value_width = content_width.saturating_sub(label_width);
    let value = truncate_middle(value, value_width);
    let content = format!("{label:<label_width$}{value:<value_width$}");
    format!("│ {} │", truncate_right(&content, content_width))
}

pub fn render_post_run_screen(model: &PostRunScreenModel) -> Vec<String> {
    render_post_run_screen_with_width(model, CANONICAL_WIDTH)
}

pub fn render_post_run_screen_with_width(model: &PostRunScreenModel, width: usize) -> Vec<String> {
    render_post_run_screen_with_width_and_glyphs(model, width, GlyphSet::Unicode)
}

pub fn render_post_run_screen_with_glyphs(
    model: &PostRunScreenModel,
    glyphs: GlyphSet,
) -> Vec<String> {
    render_post_run_screen_with_width_and_glyphs(model, CANONICAL_WIDTH, glyphs)
}

pub fn render_post_run_screen_with_width_and_glyphs(
    model: &PostRunScreenModel,
    width: usize,
    glyphs: GlyphSet,
) -> Vec<String> {
    let width = width.max(CANONICAL_WIDTH);
    let mut lines = vec![
        header_line(&model.job_name, &model.status, width),
        divider(width),
    ];
    lines.extend(release_banner_lines(&model.release_banner, width));
    lines.extend([
        progress_line(
            &model.completion_label,
            &model.completion_progress,
            Some(&post_run_progress_trailing(
                metric_value(&model.summary, "Bytes"),
                metric_value(&model.summary, "ETA"),
            )),
            width,
        ),
        blank_line(width),
        pad_to_width("Target Results", width),
        divider(width),
        pad_to_width(
            &format!(
                "{:<12} {:>7} {:>7} {:>8} {:>9} {:>11}   {}",
                "Target", "Planned", "Copied", "Verified", "Copy Fail", "Verify Fail", "Result"
            ),
            width,
        ),
    ]);

    for target in &model.target_results {
        lines.push(render_target_result_row(target, width));
    }

    if let Some(staging) = &model.staging {
        lines.push(blank_line(width));
        lines.push(pad_to_width("Staging", width));
        lines.push(divider(width));
        lines.push(pad_to_width(
            &format!(
                "Staged       {:>7} files   {:>10}",
                format_count(staging.staged_files),
                staging.staged_bytes
            ),
            width,
        ));
        lines.push(pad_to_width(
            &format!("Peak spool usage   {}", staging.peak_spool_bytes),
            width,
        ));
        if let Some(released) = &staging.released_after {
            lines.push(pad_to_width(
                &format!("Source released    {released}"),
                width,
            ));
        }
    }

    if !model.errors.is_empty() {
        lines.push(blank_line(width));
        lines.push(pad_to_width("Failures", width));
        lines.push(divider(width));
        lines.push(pad_to_width(
            &format!("{:<10} {:<8} {:<24} {}", "Target", "Phase", "File", "Error"),
            width,
        ));
        for error in &model.errors {
            lines.push(render_error_row(error, width));
        }
    }

    lines.push(blank_line(width));
    lines.push(pad_to_width("Breakdown", width));
    lines.push(divider(width));
    lines.push(pad_to_width(
        &format!(
            "{:<20} {:>7} {:>12} {:>9}",
            "Bucket", "Files", "Bytes", "Share"
        ),
        width,
    ));

    for category in &model.categories {
        lines.push(render_category_row(category, width));
    }

    if model.copied_preview_total > 0 {
        lines.push(blank_line(width));
        lines.push(pad_to_width("Copied file preview", width));
        lines.push(divider(width));
        lines.push(pad_to_width(
            &format!("{:<3} {:<44} {:>10}", "#", "File", "Size"),
            width,
        ));
        for (index, file) in model.copied_preview.iter().enumerate() {
            lines.push(pad_to_width(
                &format!(
                    "{:<3} {:<44} {:>10}",
                    index + 1,
                    truncate_middle(&file.file, 44),
                    file.size
                ),
                width,
            ));
        }
        if model.copied_preview_total > model.copied_preview.len() {
            lines.push(blank_line(width));
            lines.push(pad_to_width(
                &format!(
                    "showing {} of {} copied files",
                    format_count(model.copied_preview_count.min(model.copied_preview_total)),
                    format_count(model.copied_preview_total),
                ),
                width,
            ));
        }
    }

    apply_glyphs(lines, glyphs)
}

fn append_target_progress_lines(lines: &mut Vec<String>, model: &LiveScreenModel, width: usize) {
    if model.target_progress.is_empty() {
        return;
    }

    lines.push(blank_line(width));
    lines.push(pad_to_width("Targets", width));
    for target in model.target_progress.iter().take(VISIBLE_TARGET_ROWS) {
        lines.push(render_target_progress_row(target, width));
    }
    if model.target_progress.len() > VISIBLE_TARGET_ROWS {
        lines.push(pad_to_width(
            &format!(
                "... {} more targets",
                model.target_progress.len() - VISIBLE_TARGET_ROWS
            ),
            width,
        ));
    }
}

fn apply_glyphs(lines: Vec<String>, glyphs: GlyphSet) -> Vec<String> {
    match glyphs {
        GlyphSet::Unicode => lines,
        GlyphSet::Ascii => lines.into_iter().map(ascii_line).collect(),
    }
}

fn ascii_line(line: String) -> String {
    line.chars()
        .map(|ch| match ch {
            '─' => '-',
            '┌' | '┐' | '└' | '┘' => '+',
            '│' => '|',
            '█' => '#',
            '…' => '~',
            '⠋' | '⠙' | '⠹' | '⠸' | '⠼' | '⠴' | '⠦' | '⠧' | '⠇' | '⠏' => '*',
            other => other,
        })
        .collect()
}

fn render_target_result_row(
    target: &crate::progress_model::TargetResultRowModel,
    width: usize,
) -> String {
    let result = if target.copy_failed == 0
        && target.verify_failed == 0
        && target.planned == target.copied
        && target.planned == target.verified
    {
        "verified"
    } else {
        "attention"
    };
    pad_to_width(
        &format!(
            "{:<12} {:>7} {:>7} {:>8} {:>9} {:>11}   {}",
            truncate_middle(&target.target, 12),
            format_count(target.planned),
            format_count(target.copied),
            format_count(target.verified),
            format_count(target.copy_failed),
            format_count(target.verify_failed),
            result,
        ),
        width,
    )
}

fn header_line(job_name: &str, status: &str, width: usize) -> String {
    let left = format!("Pathsync ({job_name})");
    let gap = width.saturating_sub(left.chars().count() + status.chars().count());
    format!("{left}{}{status}", " ".repeat(gap))
}

fn divider(width: usize) -> String {
    "─".repeat(width)
}

fn blank_line(width: usize) -> String {
    " ".repeat(width)
}

fn progress_line(
    label: &str,
    model: &ProgressBarModel,
    trailing: Option<&str>,
    width: usize,
) -> String {
    let bar = progress_bar_string(model.percent, model.width.max(LIVE_BAR_WIDTH));
    let prefix = format!("{label}  {bar}");
    let trailing = trailing.unwrap_or("");
    if trailing.is_empty() {
        return pad_to_width(&prefix, width);
    }

    let fitted = fit_progress_trailing(&prefix, trailing, width);
    pad_to_width(&format!("{prefix}   {fitted}"), width)
}

fn post_run_progress_trailing(bytes: &str, eta: &str) -> String {
    if eta == "--" {
        format!("{bytes} verified")
    } else {
        format!("{bytes} verified   ETA {eta}")
    }
}

fn fit_progress_trailing(prefix: &str, trailing: &str, width: usize) -> String {
    let max_trailing = width.saturating_sub(prefix.chars().count() + 3);
    if trailing.chars().count() <= max_trailing {
        return trailing.to_string();
    }

    if let Some(pos) = trailing.find("   ETA ") {
        let without_eta = trailing[..pos].trim_end();
        if without_eta.chars().count() <= max_trailing {
            return without_eta.to_string();
        }
        let compact = without_eta.replace(" copied of ", " / ");
        if compact.chars().count() <= max_trailing {
            return compact;
        }
        return truncate_right(&compact, max_trailing);
    }

    truncate_right(trailing, max_trailing)
}

fn phase_line(label: &str, width: usize) -> String {
    pad_to_width(label, width)
}

fn render_worker_row(worker: &WorkerRowModel, width: usize) -> String {
    let show_rate = width >= 100;
    let show_bar = width >= 90;
    let bar_width = if width < 110 { 4 } else { WORKER_BAR_WIDTH };
    let bar = if !show_bar {
        String::new()
    } else if worker.idle {
        worker_progress_bar_string(0, bar_width)
    } else {
        worker_progress_bar_string(worker.percent, bar_width)
    };
    let rate = if show_rate && !worker.time.is_empty() {
        worker.time.clone()
    } else {
        String::new()
    };
    let target = if worker.target.is_empty() {
        "--".to_string()
    } else {
        worker.target.clone()
    };
    let target_width = if width < 100 { 11 } else { 14 };
    let target = truncate_middle(&target, target_width);
    let phase = worker.phase.map(|phase| phase.as_label()).unwrap_or("");
    let phase_width = if width < 100 { 8 } else { 9 };
    let size_width = if width < 100 { 6 } else { 8 };
    let rate_width = if show_rate { 10 } else { 0 };
    let phase = truncate_middle(phase, phase_width);
    let size = truncate_middle(&worker.size, size_width);
    let rate = truncate_right(&rate, rate_width);

    let spinner = worker.spinner_frame.unwrap_or(' ');
    let bar_segment = if show_bar {
        format!("{}  ", bar)
    } else {
        String::new()
    };
    let rate_segment = if show_rate {
        format!("  {:>rate_width$}", rate, rate_width = rate_width)
    } else {
        String::new()
    };
    let fixed_width = spinner.to_string().chars().count()
        + 1
        + worker.worker_tag.chars().count()
        + 2
        + phase_width
        + 1
        + bar.chars().count()
        + if show_bar { 2 } else { 0 }
        + 2
        + size_width
        + if show_rate { 2 + rate_width } else { 0 }
        + 2
        + target.chars().count();
    let max_item_width = if worker.target.is_empty() { 21 } else { 52 };
    let item_width = width.saturating_sub(fixed_width).clamp(8, max_item_width);

    pad_to_width(
        &format!(
            "{spinner} {}  {:<phase_width$} {bar_segment}{:<item_width$}  {:>size_width$}{rate_segment}  {}",
            worker.worker_tag,
            phase,
            truncate_middle(&worker.item, item_width),
            size,
            target,
            phase_width = phase_width,
            item_width = item_width
        ),
        width,
    )
}

fn render_target_progress_row(target: &TargetProgressRowModel, width: usize) -> String {
    let bar_width = if width < 90 { 16 } else { TARGET_BAR_WIDTH };
    let bar = progress_bar_string_with_empty(target.percent, bar_width, ' ');
    let label = truncate_middle(&target.target, 10);
    let bytes = truncate_middle(&target.bytes, if width < 90 { 14 } else { 18 });

    let line = if width < 85 {
        format!("{:<10} {}  {}", label, bar, bytes)
    } else if width < 100 {
        format!("{:<10} {}  {:<14} {:>10}", label, bar, bytes, target.rate)
    } else {
        format!(
            "{:<10} {}  {:<18} {:>10}   {} active",
            label, bar, bytes, target.rate, target.active_workers
        )
    };

    pad_to_width(&line, width)
}

fn render_category_row(category: &crate::progress_model::CategoryRowModel, width: usize) -> String {
    pad_to_width(
        &format!(
            "{:<20} {:>7} {:>12} {:>9}",
            truncate_middle(&category.label, 20),
            format_count(category.files),
            category.bytes,
            category.percent,
        ),
        width,
    )
}

fn render_error_row(error: &crate::progress_model::ErrorRowModel, width: usize) -> String {
    let fixed_width = 10 + 1 + 8 + 1 + 24 + 1;
    let error_width = width.saturating_sub(fixed_width);
    pad_to_width(
        &format!(
            "{:<10} {:<8} {:<24} {}",
            truncate_middle(&error.target, 10),
            truncate_middle(&error.phase, 8),
            truncate_middle(&error.file, 24),
            truncate_middle(&error.error, error_width),
        ),
        width,
    )
}

fn progress_bar_string(percent: usize, width: usize) -> String {
    progress_bar_string_with_empty(percent, width, '-')
}

fn worker_progress_bar_string(percent: usize, width: usize) -> String {
    progress_bar_string_with_empty(percent, width, ' ')
}

fn progress_bar_string_with_empty(percent: usize, width: usize, empty: char) -> String {
    let filled = filled_cells(percent, width);
    format!(
        "[{}{}]",
        "█".repeat(filled.min(width)),
        empty
            .to_string()
            .repeat(width.saturating_sub(filled.min(width)))
    )
}

fn filled_cells(percent: usize, width: usize) -> usize {
    let percent = percent.min(100);
    if percent >= 100 {
        return width;
    }

    let rounded = ((percent * width) + 50) / 100;
    rounded.clamp(0, width.saturating_sub(1))
}

fn may4_primary_metrics_row(model: &LiveScreenModel, width: usize) -> String {
    let bytes = compact_bytes_range(metric_value(&model.summary, "Bytes"));
    let percent = model.overall_progress.percent;
    let rate = metric_value(&model.summary, "Rate");
    let eta = metric_value(&model.summary, "ETA");
    let copied = metric_value(&model.summary, "Copied");
    let verified = metric_value(&model.summary, "Verified");
    let failed = metric_value(&model.summary, "Failed");

    let variants = vec![
        format!(
            "{}   {}%   {}   ETA {}   copied {}   verified {}   failed {}",
            bytes, percent, rate, eta, copied, verified, failed
        ),
        format!(
            "{}   {}%   {}   ETA {}   copied {}   verified {}",
            bytes, percent, rate, eta, copied, verified
        ),
        format!(
            "{}   {}%   {}   copied {}   verified {}",
            bytes, percent, rate, copied, verified
        ),
        format!("{}   {}%   {}   copied {}", bytes, percent, rate, copied),
        format!("{}   {}%", bytes, percent),
    ];
    fit_first_variant(&variants, width)
}

fn may4_secondary_metrics_row(model: &LiveScreenModel, width: usize) -> String {
    let scanned = metric_value(&model.summary, "Scanned");
    let planned = metric_value(&model.summary, "Planned");
    let failed = metric_value(&model.summary, "Failed");
    let elapsed = metric_value(&model.summary, "Elapsed");
    let targets = metric_value(&model.summary, "Targets");

    let variants = vec![
        format!(
            "scanned {}     planned {}         failed {}     elapsed {}   targets {}",
            scanned, planned, failed, elapsed, targets
        ),
        format!(
            "scanned {}   planned {}   failed {}   elapsed {}   targets {}",
            scanned, planned, failed, elapsed, targets
        ),
        format!(
            "scanned {}   planned {}   elapsed {}   targets {}",
            scanned, planned, elapsed, targets
        ),
        format!(
            "scanned {}   planned {}   targets {}",
            scanned, planned, targets
        ),
    ];
    fit_first_variant(&variants, width)
}

fn may4_phase_title(phase_label: &str) -> String {
    let title = phase_label.strip_prefix("overall ").unwrap_or(phase_label);
    let mut chars = title.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn may4_progress_bytes_text(bytes_metric: &str) -> String {
    bytes_metric.replace(" / ", " of ")
}

fn compact_bytes_range(bytes_metric: &str) -> String {
    let parts: Vec<&str> = bytes_metric.split(" / ").collect();
    if parts.len() != 2 {
        return bytes_metric.to_string();
    }

    let left = parts[0].split_whitespace().next().unwrap_or(parts[0]);
    format!("{} / {}", left, parts[1])
}

fn fit_first_variant(variants: &[String], width: usize) -> String {
    for variant in variants {
        if variant.chars().count() <= width {
            return variant.clone();
        }
    }
    truncate_right(variants.last().map(String::as_str).unwrap_or(""), width)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferFieldId {
    Worker,
    Phase,
    Filename,
    Size,
    Rate,
    Destination,
}

impl TransferFieldId {
    fn index(self) -> usize {
        match self {
            Self::Worker => 0,
            Self::Phase => 1,
            Self::Filename => 2,
            Self::Size => 3,
            Self::Rate => 4,
            Self::Destination => 5,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FieldSpec {
    id: TransferFieldId,
    min: usize,
    preferred: usize,
    max: usize,
    /// lower = keep first
    priority: u8,
    required_for_oneline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferRowMode {
    OneLine,
    Stacked,
}

#[derive(Debug, Clone, Copy)]
struct FieldAllocation {
    mode: TransferRowMode,
    widths: [usize; FIELD_COUNT],
}

impl FieldAllocation {
    fn width(self, id: TransferFieldId) -> usize {
        self.widths[id.index()]
    }
}

const FIELD_COUNT: usize = 6;
const FILENAME_MIN: usize = 12;
const FILENAME_MAX: usize = 72;
const PHASE_WIDTH: usize = 9;
const SIZE_PREFERRED: usize = 8;
const SIZE_MAX: usize = 12;
const RATE_PREFERRED: usize = 10;
const RATE_MAX: usize = 12;
const DESTINATION_MAX: usize = 24;
const ONE_LINE_SEPARATORS_WITHOUT_RATE: usize = 10;
const ONE_LINE_RATE_SEPARATOR: usize = 2;
const STACK_LINE1_SEPARATORS: usize = 6;
const STACK_INDENT: usize = 9;
const STACK_LINE2_GAPS: usize = 7;

fn transfer_field_specs(worker: &WorkerRowModel) -> [FieldSpec; FIELD_COUNT] {
    let worker_len = worker.worker_tag.chars().count().max(1);
    let filename_len = worker.item.chars().count();
    let filename_min = filename_len
        .min(FILENAME_MAX)
        .max(filename_len.min(FILENAME_MIN));
    let size_len = worker.size.chars().count().max(1);
    let rate_len = worker.time.chars().count().max(1);
    let dest_len = if worker.target.is_empty() {
        2
    } else {
        worker.target.chars().count().max(1)
    }
    .min(DESTINATION_MAX);

    [
        FieldSpec {
            id: TransferFieldId::Worker,
            min: worker_len,
            preferred: worker_len,
            max: worker_len,
            priority: 0,
            required_for_oneline: true,
        },
        FieldSpec {
            id: TransferFieldId::Phase,
            min: PHASE_WIDTH,
            preferred: PHASE_WIDTH,
            max: PHASE_WIDTH,
            priority: 1,
            required_for_oneline: true,
        },
        FieldSpec {
            id: TransferFieldId::Filename,
            min: filename_min,
            preferred: filename_len.min(FILENAME_MAX).max(filename_min),
            max: FILENAME_MAX,
            priority: 4,
            required_for_oneline: true,
        },
        FieldSpec {
            id: TransferFieldId::Size,
            min: size_len,
            preferred: size_len.max(SIZE_PREFERRED),
            max: SIZE_MAX,
            priority: 3,
            required_for_oneline: true,
        },
        FieldSpec {
            id: TransferFieldId::Rate,
            min: rate_len,
            preferred: rate_len.max(RATE_PREFERRED),
            max: RATE_MAX,
            priority: 5,
            required_for_oneline: false,
        },
        FieldSpec {
            id: TransferFieldId::Destination,
            min: dest_len,
            preferred: dest_len,
            max: DESTINATION_MAX,
            priority: 2,
            required_for_oneline: true,
        },
    ]
}

fn spec_for(specs: &[FieldSpec; FIELD_COUNT], id: TransferFieldId) -> FieldSpec {
    specs[id.index()]
}

fn grow_width(current: usize, remaining: usize, target: usize) -> (usize, usize) {
    let add = target.saturating_sub(current).min(remaining);
    (current + add, remaining - add)
}

fn allocate(specs: &[FieldSpec; FIELD_COUNT], width: usize) -> FieldAllocation {
    let required_min: usize = specs
        .iter()
        .filter(|spec| spec.required_for_oneline)
        .map(|spec| spec.min)
        .sum();
    let one_line_min = required_min + ONE_LINE_SEPARATORS_WITHOUT_RATE;
    if one_line_min > width {
        return allocate_stacked(specs, width);
    }

    let mut widths = [0usize; FIELD_COUNT];
    for spec in specs {
        if spec.required_for_oneline {
            widths[spec.id.index()] = spec.min;
        }
    }

    let mut remaining = width - one_line_min;
    let filename = spec_for(specs, TransferFieldId::Filename);
    let destination = spec_for(specs, TransferFieldId::Destination);
    let size = spec_for(specs, TransferFieldId::Size);
    let phase = spec_for(specs, TransferFieldId::Phase);

    (widths[filename.id.index()], remaining) =
        grow_width(widths[filename.id.index()], remaining, filename.preferred);
    (widths[destination.id.index()], remaining) = grow_width(
        widths[destination.id.index()],
        remaining,
        destination.preferred,
    );

    let mut optional: Vec<FieldSpec> = specs
        .iter()
        .copied()
        .filter(|spec| !spec.required_for_oneline)
        .collect();
    optional.sort_by_key(|spec| spec.priority);
    for spec in optional {
        let needed = spec.min + ONE_LINE_RATE_SEPARATOR;
        if remaining >= needed {
            remaining -= needed;
            widths[spec.id.index()] = spec.min;
            (widths[spec.id.index()], remaining) =
                grow_width(widths[spec.id.index()], remaining, spec.preferred);
        }
    }

    (widths[filename.id.index()], remaining) =
        grow_width(widths[filename.id.index()], remaining, filename.max);
    (widths[destination.id.index()], remaining) =
        grow_width(widths[destination.id.index()], remaining, destination.max);

    let rate_idx = TransferFieldId::Rate.index();
    if widths[rate_idx] > 0 {
        let rate = spec_for(specs, TransferFieldId::Rate);
        (widths[rate_idx], remaining) = grow_width(widths[rate_idx], remaining, rate.max);
    }

    (widths[phase.id.index()], remaining) =
        grow_width(widths[phase.id.index()], remaining, phase.preferred);
    (widths[size.id.index()], remaining) = grow_width(widths[size.id.index()], remaining, size.max);

    debug_assert_eq!(
        remaining,
        width.saturating_sub(one_line_occupancy(&widths)),
        "one-line allocation must reserve separators before padding"
    );
    FieldAllocation {
        mode: TransferRowMode::OneLine,
        widths,
    }
}

fn one_line_occupancy(widths: &[usize; FIELD_COUNT]) -> usize {
    let rate = widths[TransferFieldId::Rate.index()];
    let rate_part = if rate > 0 {
        ONE_LINE_RATE_SEPARATOR + rate
    } else {
        0
    };
    ONE_LINE_SEPARATORS_WITHOUT_RATE
        + widths[TransferFieldId::Worker.index()]
        + widths[TransferFieldId::Phase.index()]
        + widths[TransferFieldId::Filename.index()]
        + widths[TransferFieldId::Size.index()]
        + widths[TransferFieldId::Destination.index()]
        + rate_part
}

fn allocate_stacked(specs: &[FieldSpec; FIELD_COUNT], width: usize) -> FieldAllocation {
    let worker = spec_for(specs, TransferFieldId::Worker);
    let phase = spec_for(specs, TransferFieldId::Phase);
    let filename = spec_for(specs, TransferFieldId::Filename);
    let size = spec_for(specs, TransferFieldId::Size);
    let rate = spec_for(specs, TransferFieldId::Rate);
    let destination = spec_for(specs, TransferFieldId::Destination);

    let mut widths = [0usize; FIELD_COUNT];
    widths[worker.id.index()] = worker.min;
    widths[phase.id.index()] = phase.min.min(phase.max);
    let filename_budget = width.saturating_sub(
        STACK_LINE1_SEPARATORS + widths[worker.id.index()] + widths[phase.id.index()],
    );
    widths[filename.id.index()] = filename_budget
        .min(filename.max)
        .max(1.min(filename_budget));

    widths[size.id.index()] = size.preferred.min(size.max);
    widths[rate.id.index()] = rate.preferred.min(rate.max);
    let dest_budget = width.saturating_sub(
        STACK_INDENT + STACK_LINE2_GAPS + widths[size.id.index()] + widths[rate.id.index()],
    );
    widths[destination.id.index()] = dest_budget.min(destination.max).max(1.min(dest_budget));

    FieldAllocation {
        mode: TransferRowMode::Stacked,
        widths,
    }
}

fn transfer_destination(worker: &WorkerRowModel) -> &str {
    if worker.target.is_empty() {
        "--"
    } else {
        &worker.target
    }
}

fn render_may4_transfer_row(worker: &WorkerRowModel, width: usize) -> Vec<String> {
    if worker.idle {
        return vec![pad_to_width(
            &format!(
                "  {}           idle                          --       --          --",
                worker.worker_tag
            ),
            width,
        )];
    }

    let specs = transfer_field_specs(worker);
    let alloc = allocate(&specs, width);
    match alloc.mode {
        TransferRowMode::OneLine => vec![render_may4_transfer_oneline(worker, alloc, width)],
        TransferRowMode::Stacked => render_may4_transfer_stacked(worker, alloc, width),
    }
}

fn render_may4_transfer_oneline(
    worker: &WorkerRowModel,
    alloc: FieldAllocation,
    width: usize,
) -> String {
    let spinner = worker.spinner_frame.unwrap_or(' ');
    let phase = worker
        .phase
        .map(|phase| phase.as_label())
        .unwrap_or("copying");
    let worker_width = alloc.width(TransferFieldId::Worker);
    let phase_width = alloc.width(TransferFieldId::Phase);
    let filename_width = alloc.width(TransferFieldId::Filename);
    let size_width = alloc.width(TransferFieldId::Size);
    let rate_width = alloc.width(TransferFieldId::Rate);
    let dest_width = alloc.width(TransferFieldId::Destination);
    let rate_segment = if rate_width > 0 {
        format!(
            "  {:>rate_width$}",
            truncate_right(&worker.time, rate_width),
            rate_width = rate_width
        )
    } else {
        String::new()
    };

    pad_to_width(
        &format!(
            "{spinner} {:<worker_width$}  {:<phase_width$}  {:<filename_width$}  {:>size_width$}{rate_segment}  {:<dest_width$}",
            worker.worker_tag,
            truncate_middle(phase, phase_width),
            truncate_middle(&worker.item, filename_width),
            truncate_middle(&worker.size, size_width),
            truncate_middle(transfer_destination(worker), dest_width),
            worker_width = worker_width,
            phase_width = phase_width,
            filename_width = filename_width,
            dest_width = dest_width
        ),
        width,
    )
}

fn render_may4_transfer_stacked(
    worker: &WorkerRowModel,
    alloc: FieldAllocation,
    width: usize,
) -> Vec<String> {
    let spinner = worker.spinner_frame.unwrap_or(' ');
    let phase = worker
        .phase
        .map(|phase| phase.as_label())
        .unwrap_or("copying");
    let worker_width = alloc.width(TransferFieldId::Worker);
    let phase_width = alloc.width(TransferFieldId::Phase);
    let filename_width = alloc.width(TransferFieldId::Filename);
    let size_width = alloc.width(TransferFieldId::Size);
    let rate_width = alloc.width(TransferFieldId::Rate);
    let dest_width = alloc.width(TransferFieldId::Destination);

    let line1 = format!(
        "{spinner} {:<worker_width$}  {:<phase_width$}  {:<filename_width$}",
        worker.worker_tag,
        truncate_middle(phase, phase_width),
        truncate_middle(&worker.item, filename_width),
        worker_width = worker_width,
        phase_width = phase_width,
        filename_width = filename_width
    );
    let line2 = format!(
        "{}{:>size_width$}   {:>rate_width$}  → {}",
        " ".repeat(STACK_INDENT),
        truncate_middle(&worker.size, size_width),
        truncate_right(&worker.time, rate_width),
        truncate_middle(transfer_destination(worker), dest_width),
        size_width = size_width,
        rate_width = rate_width
    );

    vec![pad_to_width(&line1, width), pad_to_width(&line2, width)]
}

fn render_may4_target_progress_row(target: &TargetProgressRowModel, width: usize) -> String {
    let bar_width = if width < 90 { 16 } else { TARGET_BAR_WIDTH };
    let bar = progress_bar_string_with_empty(target.percent, bar_width, '-');
    let label = truncate_middle(&target.target, if width < 100 { 8 } else { 10 });
    let bytes = compact_bytes_range(&target.bytes);

    let line = if width < 85 {
        format!("{:<10} {}  {}", label, bar, bytes)
    } else if width < 110 {
        format!("{:<10} {}  {:<16} {:>10}", label, bar, bytes, target.rate)
    } else {
        format!(
            "{:<10} {}  {:<16} {:>10}   {} active",
            label, bar, bytes, target.rate, target.active_workers
        )
    };

    pad_to_width(&line, width)
}

fn live_counts_row(metrics: &[crate::progress_model::SummaryMetric]) -> String {
    summary_two_column_row(
        &metric_pair(metrics, "Scanned"),
        &format!(
            "{}, {}, {}",
            metric_pair(metrics, "Planned"),
            metric_pair(metrics, "Copied"),
            metric_pair(metrics, "Failed")
        ),
    )
}

fn live_bytes_rate_row(metrics: &[crate::progress_model::SummaryMetric]) -> String {
    summary_two_column_row(
        &metric_pair(metrics, "Bytes"),
        &metric_pair(metrics, "Rate"),
    )
}

fn live_elapsed_eta_row(metrics: &[crate::progress_model::SummaryMetric]) -> String {
    summary_two_column_row(
        &metric_pair(metrics, "Elapsed"),
        &metric_pair(metrics, "ETA"),
    )
}

fn summary_two_column_row(left: &str, right: &str) -> String {
    let left_width = SUMMARY_RIGHT_COLUMN.max(left.chars().count() + 1);
    pad_to_width(
        &format!("{left:<left_width$}{right}", left_width = left_width),
        CANONICAL_WIDTH,
    )
}

fn metric_pair(metrics: &[crate::progress_model::SummaryMetric], label: &str) -> String {
    format!("{label}: {}", metric_value(metrics, label))
}

fn metric_value<'a>(metrics: &'a [crate::progress_model::SummaryMetric], label: &str) -> &'a str {
    metrics
        .iter()
        .find(|metric| metric.label == label)
        .map(|metric| metric.value.as_str())
        .unwrap_or("--")
}

fn pad_to_width(value: &str, width: usize) -> String {
    let value_width = value.chars().count();
    if value_width >= width {
        value.chars().take(width).collect()
    } else {
        format!("{value}{}", " ".repeat(width - value_width))
    }
}

fn truncate_middle(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max_chars {
        return value.to_string();
    }

    if max_chars <= 3 {
        return "…".to_string();
    }

    let head_len = (max_chars - 1) / 2;
    let tail_len = max_chars - head_len - 1;
    let head: String = chars[..head_len].iter().collect();
    let tail: String = chars[chars.len() - tail_len..].iter().collect();
    format!("{head}…{tail}")
}

fn truncate_right(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max_chars {
        value.to_string()
    } else {
        chars[..max_chars].iter().collect()
    }
}

fn format_count(value: usize) -> String {
    let digits = value.to_string();
    let mut result = String::new();
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}
