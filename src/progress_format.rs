use std::path::Path;
use std::time::Duration;

use crate::format::{human_bytes, human_rate};
pub use crate::progress_model::{PhaseKind, ProgressSnapshot, phase_label};
use crate::progress_model::{
    LiveScreenModel, PostRunScreenModel, ProgressBarModel, WorkerRowModel, overall_message,
};

pub const CANONICAL_WIDTH: usize = 80;
const LIVE_BAR_WIDTH: usize = 24;
const WORKER_BAR_WIDTH: usize = 22;

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
        summary_row(&model.summary[..4]),
        summary_row(&model.summary[4..8]),
        blank_line(),
        progress_line(&model.overall_label, &model.overall_progress),
        blank_line(),
        phase_line(&model.phase_label, &model.phase_progress_text),
    ];

    for worker in &model.workers {
        lines.push(render_worker_row(worker));
    }

    lines
}

pub fn render_post_run_screen(model: &PostRunScreenModel) -> Vec<String> {
    let mut lines = vec![
        header_line(&model.job_name, &model.status),
        divider(),
        summary_row(&model.summary[..4]),
        summary_row(&model.summary[4..8]),
        blank_line(),
        progress_line(&model.completion_label, &model.completion_progress),
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

fn summary_row(metrics: &[crate::progress_model::SummaryMetric]) -> String {
    let grid_labels = ["Scanned", "Planned", "Copied", "Failed"];
    if metrics.len() == 4
        && metrics
            .iter()
            .map(|metric| metric.label.as_str())
            .eq(grid_labels)
    {
        let cols: Vec<String> = metrics
            .iter()
            .map(|metric| format!("{}: {}", metric.label, metric.value))
            .collect();
        return pad_to_width(&format!(
            "{:<21}{:<20}{:<20}{:<19}",
            cols[0], cols[1], cols[2], cols[3]
        ));
    }

    let content = metrics
        .iter()
        .enumerate()
        .fold(String::new(), |mut acc, (index, metric)| {
            let separator = if index == 0 { "" } else { "  " };
            let full = format!("{separator}{}: {}", metric.label, metric.value);
            if acc.chars().count() + full.chars().count() <= CANONICAL_WIDTH {
                acc.push_str(&full);
                return acc;
            }

            let fallback = format!("{separator}{}: ", metric.label);
            if acc.chars().count() + fallback.chars().count() <= CANONICAL_WIDTH {
                acc.push_str(&fallback);
            }
            acc
        });
    pad_to_width(&content)
}

fn progress_line(label: &str, model: &ProgressBarModel) -> String {
    let percent = model.percent.min(100);
    let bar = progress_bar_string(percent, LIVE_BAR_WIDTH);
    pad_to_width(&format!("{label:<19}  {bar}  {percent:>2}%"))
}

fn phase_line(label: &str, progress: &str) -> String {
    let gap = CANONICAL_WIDTH.saturating_sub(label.chars().count() + progress.chars().count());
    format!("{label}{}{progress}", " ".repeat(gap))
}

fn render_worker_row(worker: &WorkerRowModel) -> String {
    let bar = if worker.idle {
        "░".repeat(WORKER_BAR_WIDTH)
    } else {
        rounded_progress_bar_string(worker.percent, WORKER_BAR_WIDTH)
    };
    let item_width = if worker.idle { 28 } else { 30 };

    pad_to_width(&format!(
        "{}  {}  {:<item_width$}  {:>8}  {:>8}",
        worker.worker_tag,
        bar,
        truncate_middle(&worker.item, item_width),
        worker.size,
        worker.time,
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

fn progress_bar_string(percent: usize, width: usize) -> String {
    let filled = (percent.min(100) * width) / 100;
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

fn rounded_progress_bar_string(percent: usize, width: usize) -> String {
    let filled = ((percent.min(100) * width) + 50) / 100;
    format!("{}{}", "█".repeat(filled.min(width)), "░".repeat(width.saturating_sub(filled.min(width))))
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
