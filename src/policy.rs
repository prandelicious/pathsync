use chrono_tz::Tz;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparePolicy {
    Path,
    PathSize,
    SizeMtime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferPolicy {
    Standard,
    Adaptive {
        large_file_threshold_bytes: u64,
        large_file_slots: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimezonePolicy {
    Local,
    Utc,
    Named(Tz),
}

impl TimezonePolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "local" => Some(Self::Local),
            "UTC" => Some(Self::Utc),
            _ => value.parse::<Tz>().ok().map(Self::Named),
        }
    }

    pub fn display_name(&self) -> String {
        match self {
            Self::Local => "local".to_string(),
            Self::Utc => "UTC".to_string(),
            Self::Named(tz) => tz.name().to_string(),
        }
    }
}

pub fn normalize_extensions(extensions: &[String]) -> Vec<String> {
    extensions
        .iter()
        .filter_map(|ext| {
            let trimmed = ext.trim().trim_start_matches('.').to_ascii_lowercase();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect()
}
