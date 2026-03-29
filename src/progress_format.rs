use std::path::Path;
use std::time::Duration;

use crate::format::{human_bytes, human_rate};
pub use crate::progress_model::{PhaseKind, ProgressSnapshot, phase_label};

use crate::progress_model::overall_message;

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
    format!("[W{worker:02}]")
}

pub fn overall_line(snapshot: &ProgressSnapshot) -> String {
    overall_message(snapshot)
}

pub fn plain_progress_line(snapshot: &ProgressSnapshot) -> String {
    overall_line(snapshot)
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
