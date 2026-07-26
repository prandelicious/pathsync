use std::path::PathBuf;

use thiserror::Error;

use crate::plan::PlanError;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config: {path}")]
    ReadConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config: {path}")]
    ParseConfig {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("job `{name}` not found in config")]
    JobNotFound { name: String },
    #[error("job `{name}` is disabled")]
    DisabledJob { name: String },
    #[error("default job `{name}` not found in config")]
    DefaultJobNotFound { name: String },
    #[error("no enabled jobs found in config")]
    NoEnabledJobs,
    #[error("parallel must be at least 1")]
    InvalidParallel,
    #[error("source folder not found: {path}")]
    SourceFolderNotFound { path: PathBuf },
    #[error("target folder not found: {path}")]
    TargetFolderNotFound { path: PathBuf },
    #[error("job `{name}` cannot set both `target` and `targets`")]
    ConflictingTargetSettings { name: String },
    #[error("job `{name}` must set either `target` or `targets`")]
    MissingTargetSetting { name: String },
    #[error("job `{name}` must include at least one target path")]
    EmptyTargets { name: String },
    #[error("job `{name}` has no valid extensions")]
    NoValidExtensions { name: String },
    #[error("unsupported compare mode: {mode}")]
    UnsupportedCompareMode { mode: String },
    #[error("unsupported transfer mode: {mode}")]
    UnsupportedTransferMode { mode: String },
    #[error("adaptive large_file_slots must be between 1 and the resolved parallel limit")]
    InvalidAdaptiveLargeFileSlots,
    #[error("template layout requires `value`")]
    MissingTemplateValue,
    #[error("unsupported layout kind: {kind}")]
    UnsupportedLayoutKind { kind: String },
    #[error("unsupported layout preset: {preset}")]
    UnsupportedLayoutPreset { preset: String },
    #[error("invalid timezone: {value}")]
    InvalidTimezone { value: String },
    #[error("staging max_gb must be greater than 0")]
    StagingMaxGbZero,
    #[error("{message}")]
    StagingDirConflictsWithSource { message: String },
    #[error("{message}")]
    StagingDirConflictsWithTarget { message: String },
    #[error("staging directory creation failed: {path}")]
    StagingDirCreationFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Error)]
pub enum DateError {
    #[error("failed to resolve date components for timezone policy `{policy}`")]
    ResolveTimezone { policy: String },
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CopyFailureClassification {
    #[error("local")]
    Local,
    #[error("systemic")]
    Systemic,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CopyOperation {
    #[error("create_dir")]
    CreateDir,
    #[error("open_source")]
    OpenSource,
    #[error("create_temp")]
    CreateTemp,
    #[error("read")]
    Read,
    #[error("write")]
    Write,
    #[error("flush")]
    Flush,
    #[error("set_permissions")]
    SetPermissions,
    #[error("set_mtime")]
    SetMtime,
    #[error("rename")]
    Rename,
    #[error("hash_source")]
    HashSource,
    #[error("source_changed")]
    SourceChanged,
    #[error("verify")]
    Verify,
    #[error("cleanup_temp")]
    CleanupTemp,
    #[error("worker_panic")]
    WorkerPanic,
    #[error("ui_panic")]
    UiPanic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFailure {
    pub source: PathBuf,
    pub dest: Option<PathBuf>,
    pub operation: CopyOperation,
    pub kind: std::io::ErrorKind,
    pub raw_os_error: Option<i32>,
    pub classification: CopyFailureClassification,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum CopyError {
    #[error("copy failed for {failures_len} item(s)")]
    RunFailed {
        failures: Vec<CopyFailure>,
        failures_len: usize,
        systemic_detected: bool,
    },
    #[error("{message}")]
    Internal { message: String },
    #[error("progress UI thread panicked")]
    UiThreadPanicked,
}

#[derive(Debug, Error)]
pub enum PathsyncError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Date(#[from] DateError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Copy(#[from] CopyError),
    #[error("invalid UTF-8 file name: {path}")]
    InvalidSourceFileName { path: PathBuf },
    #[error("invalid UTF-8 file stem: {path}")]
    InvalidSourceFileStem { path: PathBuf },
    #[error("missing file extension: {path}")]
    MissingFileExtension { path: PathBuf },
    #[error("failed to read modified time: {path}")]
    ReadModifiedTime {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
