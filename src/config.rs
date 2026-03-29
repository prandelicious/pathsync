use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::policy::{
    ComparePolicy, TimezonePolicy, TransferPolicy,
    normalize_extensions as normalize_policy_extensions,
};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub default_job: Option<String>,
    pub parallel: Option<usize>,
    pub timezone: Option<String>,
    pub jobs: BTreeMap<String, JobConfig>,
}

#[derive(Debug, Deserialize)]
pub struct JobConfig {
    pub enabled: Option<bool>,
    pub source: PathBuf,
    pub target: PathBuf,
    pub extensions: Vec<String>,
    pub compare: Option<CompareConfig>,
    pub transfer: Option<TransferConfig>,
    pub parallel: Option<usize>,
    pub timezone: Option<String>,
    pub layout: LayoutConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompareConfig {
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransferConfig {
    pub mode: Option<String>,
    pub large_file_threshold_mb: Option<u64>,
    pub large_file_slots: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum LayoutConfig {
    Preset(String),
    Detailed(LayoutDetailed),
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct LayoutDetailed {
    pub kind: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedJob {
    pub name: String,
    pub source: PathBuf,
    pub target: PathBuf,
    pub extensions: Vec<String>,
    pub compare_policy: ComparePolicy,
    pub transfer_policy: TransferPolicy,
    pub timezone_policy: TimezonePolicy,
    pub parallel: usize,
    pub template: String,
}

#[allow(dead_code)]
pub fn default_config_path() -> PathBuf {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("pathsync").join("config.toml")
}

#[allow(dead_code)]
pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    let content = fs::read_to_string(path).map_err(|source| ConfigError::ReadConfig {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&content).map_err(|source| ConfigError::ParseConfig {
        path: path.to_path_buf(),
        source,
    })
}

pub fn resolve_job(
    config: &Config,
    requested_job: Option<&str>,
    cli_parallel: Option<usize>,
    allow_disabled: bool,
    cli_extensions: Option<&[String]>,
) -> Result<ResolvedJob, ConfigError> {
    let job_name = match requested_job {
        Some(name) => {
            let job = config
                .jobs
                .get(name)
                .ok_or_else(|| ConfigError::JobNotFound {
                    name: name.to_string(),
                })?;
            if !allow_disabled && !job.enabled.unwrap_or(true) {
                return Err(ConfigError::DisabledJob {
                    name: name.to_string(),
                });
            }
            name.to_string()
        }
        None => {
            if let Some(default_job) = &config.default_job {
                if let Some(job) = config.jobs.get(default_job) {
                    if job.enabled.unwrap_or(true) {
                        default_job.clone()
                    } else {
                        config
                            .jobs
                            .iter()
                            .find(|(_, job)| job.enabled.unwrap_or(true))
                            .map(|(name, _)| name.clone())
                            .ok_or(ConfigError::NoEnabledJobs)?
                    }
                } else {
                    return Err(ConfigError::DefaultJobNotFound {
                        name: default_job.clone(),
                    });
                }
            } else {
                config
                    .jobs
                    .iter()
                    .find(|(_, job)| job.enabled.unwrap_or(true))
                    .map(|(name, _)| name.clone())
                    .ok_or(ConfigError::NoEnabledJobs)?
            }
        }
    };

    let job = config
        .jobs
        .get(&job_name)
        .ok_or_else(|| ConfigError::JobNotFound {
            name: job_name.clone(),
        })?;

    let parallel = cli_parallel
        .or(job.parallel)
        .or(config.parallel)
        .unwrap_or(4);

    if parallel == 0 {
        return Err(ConfigError::InvalidParallel);
    }

    if !job.source.is_dir() {
        return Err(ConfigError::SourceFolderNotFound {
            path: job.source.clone(),
        });
    }
    if !job.target.is_dir() {
        return Err(ConfigError::TargetFolderNotFound {
            path: job.target.clone(),
        });
    }

    let template = layout_to_template(&job.layout)?;
    let extensions = normalize_extensions(cli_extensions.unwrap_or(&job.extensions));
    if extensions.is_empty() {
        return Err(ConfigError::NoValidExtensions { name: job_name });
    }
    let compare_policy = resolve_compare_policy(job.compare.as_ref())?;
    let transfer_policy = resolve_transfer_policy(job.transfer.as_ref(), parallel)?;
    let timezone_policy =
        resolve_timezone_policy(config.timezone.as_deref(), job.timezone.as_deref())?;

    Ok(ResolvedJob {
        name: job_name.clone(),
        source: job.source.clone(),
        target: job.target.clone(),
        extensions,
        compare_policy,
        transfer_policy,
        timezone_policy,
        parallel,
        template,
    })
}

pub fn normalize_extensions_public(extensions: &[String]) -> Vec<String> {
    normalize_policy_extensions(extensions)
}

pub fn normalize_extensions(extensions: &[String]) -> Vec<String> {
    normalize_extensions_public(extensions)
}

pub fn resolve_compare_policy(
    compare: Option<&CompareConfig>,
) -> Result<ComparePolicy, ConfigError> {
    let Some(compare) = compare else {
        return Ok(ComparePolicy::SizeMtime);
    };

    match compare.mode.as_deref().unwrap_or("size_mtime") {
        "path" => Ok(ComparePolicy::Path),
        "path_size" => Ok(ComparePolicy::PathSize),
        "size_mtime" => Ok(ComparePolicy::SizeMtime),
        other => Err(ConfigError::UnsupportedCompareMode {
            mode: other.to_string(),
        }),
    }
}

pub fn resolve_transfer_policy(
    transfer: Option<&TransferConfig>,
    parallel: usize,
) -> Result<TransferPolicy, ConfigError> {
    let Some(transfer) = transfer else {
        return Ok(TransferPolicy::Standard);
    };

    match transfer.mode.as_deref().unwrap_or("standard") {
        "standard" => Ok(TransferPolicy::Standard),
        "adaptive" => {
            let threshold_mb = transfer.large_file_threshold_mb.unwrap_or(100);
            let large_file_slots = transfer.large_file_slots.unwrap_or(parallel);
            if large_file_slots == 0 || large_file_slots > parallel {
                return Err(ConfigError::InvalidAdaptiveLargeFileSlots);
            }
            Ok(TransferPolicy::Adaptive {
                large_file_threshold_bytes: threshold_mb.saturating_mul(1024 * 1024),
                large_file_slots,
            })
        }
        other => Err(ConfigError::UnsupportedTransferMode {
            mode: other.to_string(),
        }),
    }
}

pub fn resolve_timezone_policy(
    global_timezone: Option<&str>,
    job_timezone: Option<&str>,
) -> Result<TimezonePolicy, ConfigError> {
    match job_timezone.or(global_timezone).unwrap_or("local") {
        "local" => Ok(TimezonePolicy::Local),
        "UTC" => Ok(TimezonePolicy::Utc),
        other => TimezonePolicy::parse(other).ok_or_else(|| ConfigError::InvalidTimezone {
            value: other.to_string(),
        }),
    }
}

pub fn layout_to_template(layout: &LayoutConfig) -> Result<String, ConfigError> {
    match layout {
        LayoutConfig::Preset(name) => preset_to_template(name),
        LayoutConfig::Detailed(detail) => match detail.kind.as_str() {
            "flat" => preset_to_template("flat"),
            "year_month" => preset_to_template("year_month"),
            "template" => detail
                .value
                .clone()
                .ok_or(ConfigError::MissingTemplateValue),
            other => Err(ConfigError::UnsupportedLayoutKind {
                kind: other.to_string(),
            }),
        },
    }
}

pub fn preset_to_template(name: &str) -> Result<String, ConfigError> {
    match name {
        "flat" => Ok("{filename}".to_string()),
        "year_month" => Ok("{year}/{month}/{filename}".to_string()),
        other => Err(ConfigError::UnsupportedLayoutPreset {
            preset: other.to_string(),
        }),
    }
}
