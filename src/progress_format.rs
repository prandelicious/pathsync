use std::path::Path;
use std::time::Duration;

use crate::format::{human_bytes, human_rate};
use crate::progress_model::{
    LiveScreenModel, PostRunScreenModel, ProgressBarModel, TargetProgressRowModel, WorkerRowModel,
    overall_message,
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

    for worker in model.workers.iter().take(VISIBLE_WORKER_ROWS) {
        lines.push(render_worker_row(worker, width));
    }
    for worker in model.workers.len()..VISIBLE_WORKER_ROWS {
        lines.push(render_worker_row(
            &WorkerRowModel::idle(worker_tag(worker)),
            width,
        ));
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
    for worker in model.workers.iter().take(VISIBLE_WORKER_ROWS) {
        left.push(render_worker_row(worker, left_width));
    }
    for worker in model.workers.len()..VISIBLE_WORKER_ROWS {
        left.push(render_worker_row(
            &WorkerRowModel::idle(worker_tag(worker)),
            left_width,
        ));
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
    render_post_run_screen_with_glyphs(model, GlyphSet::Unicode)
}

pub fn render_post_run_screen_with_glyphs(
    model: &PostRunScreenModel,
    glyphs: GlyphSet,
) -> Vec<String> {
    let mut lines = vec![
        header_line(&model.job_name, &model.status, CANONICAL_WIDTH),
        divider(CANONICAL_WIDTH),
    ];
    lines.extend(release_banner_lines(&model.release_banner, CANONICAL_WIDTH));
    lines.extend([
        progress_line(
            &model.completion_label,
            &model.completion_progress,
            Some(&format!(
                "{} verified   ETA {}",
                metric_value(&model.summary, "Bytes"),
                metric_value(&model.summary, "ETA")
            )),
            CANONICAL_WIDTH,
        ),
        blank_line(CANONICAL_WIDTH),
        pad_to_width("Target Results", CANONICAL_WIDTH),
        divider(CANONICAL_WIDTH),
        pad_to_width(
            &format!(
                "{:<12} {:>7} {:>7} {:>8} {:>9} {:>11}   {}",
                "Target", "Planned", "Copied", "Verified", "Copy Fail", "Verify Fail", "Result"
            ),
            CANONICAL_WIDTH,
        ),
    ]);

    for target in &model.target_results {
        lines.push(render_target_result_row(target));
    }

    if let Some(staging) = &model.staging {
        lines.push(blank_line(CANONICAL_WIDTH));
        lines.push(pad_to_width("Staging", CANONICAL_WIDTH));
        lines.push(divider(CANONICAL_WIDTH));
        lines.push(pad_to_width(
            &format!(
                "Staged       {:>7} files   {:>10}",
                format_count(staging.staged_files),
                staging.staged_bytes
            ),
            CANONICAL_WIDTH,
        ));
        lines.push(pad_to_width(
            &format!("Peak spool usage   {}", staging.peak_spool_bytes),
            CANONICAL_WIDTH,
        ));
        if let Some(released) = &staging.released_after {
            lines.push(pad_to_width(
                &format!("Source released    {released}"),
                CANONICAL_WIDTH,
            ));
        }
    }

    if !model.errors.is_empty() {
        lines.push(blank_line(CANONICAL_WIDTH));
        lines.push(pad_to_width("Failures", CANONICAL_WIDTH));
        lines.push(divider(CANONICAL_WIDTH));
        lines.push(pad_to_width(
            &format!("{:<10} {:<8} {:<24} {}", "Target", "Phase", "File", "Error"),
            CANONICAL_WIDTH,
        ));
        for error in &model.errors {
            lines.push(render_error_row(error));
        }
    }

    lines.push(blank_line(CANONICAL_WIDTH));
    lines.push(pad_to_width("Breakdown", CANONICAL_WIDTH));
    lines.push(divider(CANONICAL_WIDTH));
    lines.push(pad_to_width(
        &format!(
            "{:<20} {:>7} {:>12} {:>9}",
            "Bucket", "Files", "Bytes", "Share"
        ),
        CANONICAL_WIDTH,
    ));

    for category in &model.categories {
        lines.push(render_category_row(category));
    }

    if model.copied_preview_total > 0 {
        lines.push(blank_line(CANONICAL_WIDTH));
        lines.push(pad_to_width("Copied file preview", CANONICAL_WIDTH));
        lines.push(pad_to_width(
            &format!(
                "showing {} of {} copied files",
                format_count(model.copied_preview_count.min(model.copied_preview_total)),
                format_count(model.copied_preview_total),
            ),
            CANONICAL_WIDTH,
        ));
    }

    apply_glyphs(lines, glyphs)
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

fn render_target_result_row(target: &crate::progress_model::TargetResultRowModel) -> String {
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
        CANONICAL_WIDTH,
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
    let trailing = trailing.unwrap_or("");
    if trailing.is_empty() {
        return pad_to_width(&format!("{label}  {bar}"), width);
    }

    pad_to_width(&format!("{label}  {bar}   {trailing}"), width)
}

fn phase_line(label: &str, width: usize) -> String {
    pad_to_width(label, width)
}

fn render_worker_row(worker: &WorkerRowModel, width: usize) -> String {
    let bar_width = if width < 100 { 4 } else { WORKER_BAR_WIDTH };
    let bar = if worker.idle {
        worker_progress_bar_string(0, bar_width)
    } else {
        worker_progress_bar_string(worker.percent, bar_width)
    };
    let detail = if worker.time.is_empty() {
        "--".to_string()
    } else {
        worker.time.clone()
    };
    let target = if worker.target.is_empty() {
        "--"
    } else {
        &worker.target
    };
    let target_width = if width < 100 { 11 } else { 14 };
    let target = truncate_middle(target, target_width);
    let phase = worker.phase.map(|phase| phase.as_label()).unwrap_or("");
    let phase_width = if width < 100 { 8 } else { 9 };
    let size_width = if width < 100 { 6 } else { 8 };
    let rate_width = if width < 100 { 8 } else { 10 };
    let phase = truncate_middle(phase, phase_width);
    let size = truncate_middle(&worker.size, size_width);
    let detail = truncate_middle(&detail, rate_width);

    let spinner = worker.spinner_frame.unwrap_or(' ');
    let fixed_width = spinner.to_string().chars().count()
        + 1
        + worker.worker_tag.chars().count()
        + 2
        + phase_width
        + 1
        + bar.chars().count()
        + 2
        + 2
        + size_width
        + 2
        + rate_width
        + 2
        + target.chars().count();
    let max_item_width = if worker.target.is_empty() { 21 } else { 52 };
    let item_width = width.saturating_sub(fixed_width).clamp(8, max_item_width);

    pad_to_width(
        &format!(
            "{spinner} {}  {:<phase_width$} {}  {:<item_width$}  {:>size_width$}  {:>rate_width$}  {}",
            worker.worker_tag,
            phase,
            bar,
            truncate_middle(&worker.item, item_width),
            size,
            detail,
            target,
            phase_width = phase_width,
            item_width = item_width
        ),
        width,
    )
}

fn render_target_progress_row(target: &TargetProgressRowModel, width: usize) -> String {
    let bar = progress_bar_string_with_empty(target.percent, TARGET_BAR_WIDTH, ' ');
    pad_to_width(
        &format!(
            "{:<10} {}  {:<18} {:>10}   {} active",
            truncate_middle(&target.target, 10),
            bar,
            truncate_middle(&target.bytes, 18),
            target.rate,
            target.active_workers,
        ),
        width,
    )
}

fn render_category_row(category: &crate::progress_model::CategoryRowModel) -> String {
    pad_to_width(
        &format!(
            "{:<20} {:>7} {:>12} {:>9}",
            truncate_middle(&category.label, 20),
            format_count(category.files),
            category.bytes,
            category.percent,
        ),
        CANONICAL_WIDTH,
    )
}

fn render_error_row(error: &crate::progress_model::ErrorRowModel) -> String {
    let fixed_width = 10 + 1 + 8 + 1 + 24 + 1;
    let error_width = CANONICAL_WIDTH.saturating_sub(fixed_width);
    pad_to_width(
        &format!(
            "{:<10} {:<8} {:<24} {}",
            truncate_middle(&error.target, 10),
            truncate_middle(&error.phase, 8),
            truncate_middle(&error.file, 24),
            truncate_middle(&error.error, error_width),
        ),
        CANONICAL_WIDTH,
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
