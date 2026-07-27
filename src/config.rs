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
    pub staging: Option<StagingConfig>,
    pub jobs: BTreeMap<String, JobConfig>,
}

#[derive(Debug, Deserialize)]
pub struct JobConfig {
    pub enabled: Option<bool>,
    pub source: PathBuf,
    pub target: Option<PathBuf>,
    pub targets: Option<Vec<PathBuf>>,
    pub extensions: Vec<String>,
    pub compare: Option<CompareConfig>,
    pub transfer: Option<TransferConfig>,
    pub parallel: Option<usize>,
    pub timezone: Option<String>,
    pub staging: Option<StagingConfig>,
    pub layout: LayoutConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StagingConfig {
    pub dir: Option<PathBuf>,
    pub max_gb: Option<u64>,
    pub min_free_gb: Option<u64>,
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
    pub max_large_per_target: Option<usize>,
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
pub struct ResolvedStaging {
    pub dir: PathBuf,
    pub max_bytes: Option<u64>,
    pub min_free_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedJob {
    pub name: String,
    pub source: PathBuf,
    pub targets: Vec<PathBuf>,
    pub extensions: Vec<String>,
    pub compare_policy: ComparePolicy,
    pub transfer_policy: TransferPolicy,
    pub timezone_policy: TimezonePolicy,
    pub parallel: usize,
    pub template: String,
    pub staging: Option<ResolvedStaging>,
}

impl ResolvedJob {
    pub fn primary_target(&self) -> &Path {
        self.targets
            .first()
            .expect("resolved jobs always contain at least one target")
    }
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
    let targets = resolve_targets(&job_name, job)?;

    let template = layout_to_template(&job.layout)?;
    let extensions = normalize_extensions(cli_extensions.unwrap_or(&job.extensions));
    if extensions.is_empty() {
        return Err(ConfigError::NoValidExtensions { name: job_name });
    }
    let compare_policy = resolve_compare_policy(job.compare.as_ref())?;
    let transfer_policy = resolve_transfer_policy(job.transfer.as_ref(), parallel)?;
    let timezone_policy =
        resolve_timezone_policy(config.timezone.as_deref(), job.timezone.as_deref())?;
    let staging = resolve_staging(
        job.staging.as_ref(),
        config.staging.as_ref(),
        &job.source,
        &targets,
    )?;

    Ok(ResolvedJob {
        name: job_name.clone(),
        source: job.source.clone(),
        targets,
        extensions,
        compare_policy,
        transfer_policy,
        timezone_policy,
        parallel,
        template,
        staging,
    })
}

fn resolve_targets(job_name: &str, job: &JobConfig) -> Result<Vec<PathBuf>, ConfigError> {
    let targets = match (&job.target, &job.targets) {
        (Some(_), Some(_)) => {
            return Err(ConfigError::ConflictingTargetSettings {
                name: job_name.to_string(),
            });
        }
        (None, None) => {
            return Err(ConfigError::MissingTargetSetting {
                name: job_name.to_string(),
            });
        }
        (Some(target), None) => vec![target.clone()],
        (None, Some(targets)) if targets.is_empty() => {
            return Err(ConfigError::EmptyTargets {
                name: job_name.to_string(),
            });
        }
        (None, Some(targets)) => targets.clone(),
    };

    for target in &targets {
        if !target.is_dir() {
            return Err(ConfigError::TargetFolderNotFound {
                path: target.clone(),
            });
        }
    }

    Ok(targets)
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
            let max_large_per_target = transfer
                .max_large_per_target
                .unwrap_or_else(|| 2.min(parallel));
            if large_file_slots == 0
                || large_file_slots > parallel
                || max_large_per_target == 0
                || max_large_per_target > parallel
            {
                return Err(ConfigError::InvalidAdaptiveLargeFileSlots);
            }
            Ok(TransferPolicy::Adaptive {
                large_file_threshold_bytes: threshold_mb.saturating_mul(1024 * 1024),
                large_file_slots,
                max_large_per_target,
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

pub fn resolve_staging(
    job_staging: Option<&StagingConfig>,
    global_staging: Option<&StagingConfig>,
    source: &Path,
    targets: &[PathBuf],
) -> Result<Option<ResolvedStaging>, ConfigError> {
    // If neither job nor global staging is present, return None (backward-compatible path)
    if job_staging.is_none() && global_staging.is_none() {
        return Ok(None);
    }

    // Resolve dir: job overrides global, default to state directory if absent
    let dir = job_staging
        .and_then(|s| s.dir.as_ref())
        .or_else(|| global_staging.and_then(|s| s.dir.as_ref()))
        .cloned()
        .unwrap_or_else(default_staging_dir);

    // Ensure staging directory exists
    fs::create_dir_all(&dir).map_err(|e| ConfigError::StagingDirCreationFailed {
        path: dir.clone(),
        source: e,
    })?;

    // Validate that dir is not inside, equal to, or contains the source
    if paths_conflict(&dir, source) {
        return Err(ConfigError::StagingDirConflictsWithSource {
            message: format!(
                "staging directory {} conflicts with source directory {}",
                dir.display(),
                source.display()
            ),
        });
    }

    // Validate that dir is not inside, equal to, or contains any target
    for target in targets {
        if paths_conflict(&dir, target) {
            return Err(ConfigError::StagingDirConflictsWithTarget {
                message: format!(
                    "staging directory {} conflicts with target directory {}",
                    dir.display(),
                    target.display()
                ),
            });
        }
    }

    // Resolve max_gb: job overrides global
    let max_gb = job_staging
        .and_then(|s| s.max_gb)
        .or_else(|| global_staging.and_then(|s| s.max_gb));

    // Reject max_gb == 0
    if max_gb == Some(0) {
        return Err(ConfigError::StagingMaxGbZero);
    }

    // Convert max_gb to bytes using saturating multiply (matching the pattern from transfer_policy)
    let max_bytes = max_gb.map(|gb| gb.saturating_mul(1024 * 1024 * 1024));

    // Resolve min_free_gb: job overrides global, default to 5
    let min_free_gb = job_staging
        .and_then(|s| s.min_free_gb)
        .or_else(|| global_staging.and_then(|s| s.min_free_gb))
        .unwrap_or(5);

    // Convert min_free_gb to bytes
    let min_free_bytes = min_free_gb.saturating_mul(1024 * 1024 * 1024);

    Ok(Some(ResolvedStaging {
        dir,
        max_bytes,
        min_free_bytes,
    }))
}

fn default_staging_dir() -> PathBuf {
    // Try to use XDG_STATE_HOME or dirs::state_dir() for Unix-like systems
    #[cfg(target_os = "macos")]
    {
        // macOS: use ~/Library/Application Support/pathsync/spool
        dirs::home_dir()
            .map(|home| home.join("Library/Application Support/pathsync/spool"))
            .unwrap_or_else(|| PathBuf::from(".pathsync/spool"))
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Linux and other Unix-like systems: try XDG_STATE_HOME first, then fall back to ~/.local/state
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
            .map(|base| base.join("pathsync/spool"))
            .unwrap_or_else(|| PathBuf::from(".pathsync/spool"))
    }
}

fn paths_conflict(dir: &Path, other: &Path) -> bool {
    // Normalize both paths to handle . and .. components
    let dir_normalized = normalize_path(dir);
    let other_normalized = normalize_path(other);

    // Check if dir equals other
    if dir_normalized == other_normalized {
        return true;
    }

    // Check if dir is inside other (other is a prefix of dir)
    if dir_normalized.starts_with(&other_normalized) {
        return true;
    }

    // Check if other is inside dir (dir is a prefix of other)
    if other_normalized.starts_with(&dir_normalized) {
        return true;
    }

    false
}

fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {
                // Skip current directory references
            }
            _ => {
                normalized.push(component);
            }
        }
    }
    normalized
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
