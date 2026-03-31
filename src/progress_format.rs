use std::path::Path;
use std::time::Duration;

use crate::format::{human_bytes, human_rate};
use crate::progress_model::{
    LiveScreenModel, PostRunScreenModel, ProgressBarModel, WorkerRowModel, overall_message,
};
pub use crate::progress_model::{PhaseKind, ProgressSnapshot, phase_label};

pub const CANONICAL_WIDTH: usize = 80;
const LIVE_BAR_WIDTH: usize = 30;
const WORKER_BAR_WIDTH: usize = 18;
const VISIBLE_WORKER_ROWS: usize = 4;
const SUMMARY_RIGHT_COLUMN: usize = 27;

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
    format!("W{:02}", worker + 1)
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
    let mut lines = vec![
        header_line(&model.job_name, &model.status),
        divider(),
        live_counts_row(&model.summary),
        live_bytes_rate_row(&model.summary),
        live_elapsed_eta_row(&model.summary),
        blank_line(),
        progress_line(
            &model.overall_label,
            &model.overall_progress,
            Some(&model.overall_progress_text),
        ),
        blank_line(),
        phase_line(&model.phase_label),
    ];

    for worker in model.workers.iter().take(VISIBLE_WORKER_ROWS) {
        lines.push(render_worker_row(worker));
    }
    for worker in model.workers.len()..VISIBLE_WORKER_ROWS {
        lines.push(render_worker_row(&WorkerRowModel::idle(worker_tag(worker))));
    }
    lines.push(divider());

    lines
}

pub fn render_post_run_screen(model: &PostRunScreenModel) -> Vec<String> {
    let mut lines = vec![
        header_line(&model.job_name, &model.status),
        divider(),
        post_run_counts_row(&model.summary),
        post_run_bytes_rate_row(&model.summary),
        post_run_elapsed_skip_row(&model.summary),
        blank_line(),
        progress_line(&model.completion_label, &model.completion_progress, None),
        blank_line(),
        pad_to_width("By Category"),
        divider(),
    ];

    for category in &model.categories {
        lines.push(render_category_row(category));
    }

    lines.push(blank_line());
    lines.push(pad_to_width("Errors"));
    lines.push(divider());
    for error in &model.errors {
        lines.push(render_error_row(&error.label, &error.detail));
    }

    lines
}

fn header_line(job_name: &str, status: &str) -> String {
    let left = format!("Pathsync ({job_name})");
    let gap = CANONICAL_WIDTH.saturating_sub(left.chars().count() + status.chars().count());
    format!("{left}{}{status}", " ".repeat(gap))
}

fn divider() -> String {
    "─".repeat(CANONICAL_WIDTH)
}

fn blank_line() -> String {
    " ".repeat(CANONICAL_WIDTH)
}

fn progress_line(label: &str, model: &ProgressBarModel, trailing: Option<&str>) -> String {
    let percent = model.percent.min(100);
    let bar = rounded_progress_bar_string(percent, model.width.max(LIVE_BAR_WIDTH));
    let trailing = trailing.unwrap_or("");
    if trailing.is_empty() {
        return pad_to_width(&format!("{label:<19}  {bar}  {percent:>2}%"));
    }

    pad_to_width(&format!("{label:<19}  {bar}  {percent:>2}%  {trailing}"))
}

fn phase_line(label: &str) -> String {
    pad_to_width(label)
}

fn render_worker_row(worker: &WorkerRowModel) -> String {
    let bar = if worker.idle {
        "▒".repeat(WORKER_BAR_WIDTH)
    } else {
        rounded_progress_bar_string(worker.percent, WORKER_BAR_WIDTH)
    };
    let item_width = 24;
    let detail = if worker.time.is_empty() {
        "--".to_string()
    } else {
        worker.time.clone()
    };

    let spinner = worker.spinner_frame.unwrap_or(' ');

    pad_to_width(&format!(
        "{spinner} {}  {}  {:<item_width$}  {:>8}  {:>10}",
        worker.worker_tag,
        bar,
        truncate_middle(&worker.item, item_width),
        worker.size,
        detail,
        item_width = item_width
    ))
}

fn render_category_row(category: &crate::progress_model::CategoryRowModel) -> String {
    let file_label = if category.files == 1 { "file" } else { "files" };
    pad_to_width(&format!(
        "{:<20}{:>7} {:<5}{:>11}{:>9}{:>11}",
        truncate_middle(&category.label, 20),
        format_count(category.files),
        file_label,
        category.bytes,
        category.percent,
        category.time
    ))
}

fn render_error_row(label: &str, detail: &str) -> String {
    pad_to_width(&format!("{}  {}", truncate_middle(label, 24), detail))
}

fn rounded_progress_bar_string(percent: usize, width: usize) -> String {
    let filled = filled_cells(percent, width);
    format!(
        "{}{}",
        "█".repeat(filled.min(width)),
        "▒".repeat(width.saturating_sub(filled.min(width)))
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

fn post_run_counts_row(metrics: &[crate::progress_model::SummaryMetric]) -> String {
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

fn post_run_bytes_rate_row(metrics: &[crate::progress_model::SummaryMetric]) -> String {
    summary_two_column_row(
        &metric_pair(metrics, "Bytes transferred"),
        &metric_pair(metrics, "Avg rate"),
    )
}

fn post_run_elapsed_skip_row(metrics: &[crate::progress_model::SummaryMetric]) -> String {
    summary_two_column_row(
        &metric_pair(metrics, "Elapsed"),
        &metric_pair(metrics, "Skip rate"),
    )
}

fn summary_two_column_row(left: &str, right: &str) -> String {
    let left_width = SUMMARY_RIGHT_COLUMN.max(left.chars().count() + 1);
    pad_to_width(&format!(
        "{left:<left_width$}{right}",
        left_width = left_width
    ))
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

fn pad_to_width(value: &str) -> String {
    let width = value.chars().count();
    if width >= CANONICAL_WIDTH {
        value.chars().take(CANONICAL_WIDTH).collect()
    } else {
        format!("{value}{}", " ".repeat(CANONICAL_WIDTH - width))
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
