use std::time::Duration;

pub fn human_bytes(size: u64) -> String {
    human_size(size as f64, &["B", "KB", "MB", "GB", "TB"])
}

pub fn human_rate(bytes: u64, elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return "0 B/s".to_string();
    }

    human_rate_f64(bytes as f64 / seconds)
}

pub fn human_rate_f64(rate: f64) -> String {
    human_size(rate.max(0.0), &["B/s", "KB/s", "MB/s", "GB/s", "TB/s"])
}

pub fn format_duration(duration: Duration) -> String {
    let mut seconds = duration.as_secs();
    if duration.subsec_nanos() > 0 {
        seconds += 1;
    }

    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m{}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{}m", seconds / 3_600, (seconds % 3_600) / 60)
    }
}

fn human_size(mut value: f64, units: &[&str]) -> String {
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{value:.0} {}", units[unit])
    } else {
        format!("{value:.1} {}", units[unit])
    }
}
