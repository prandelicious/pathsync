use filetime::FileTime;
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::hash::Hash;
use std::io::{self, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

use crate::policy::{ComparePolicy, normalize_extensions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanJob {
    pub source: PathBuf,
    pub targets: Vec<PathBuf>,
    pub extensions: Vec<String>,
    pub compare_policy: ComparePolicy,
    pub template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileContext {
    pub year: String,
    pub month: String,
    pub day: String,
    pub ext: String,
    pub stem: String,
    pub filename: String,
    pub source_rel_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferPlan {
    pub source: PathBuf,
    pub dest: PathBuf,
    pub size: u64,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanningStats {
    pub scanned_files: usize,
    pub planned_files: usize,
    pub planned_bytes: u64,
    pub skipped_existing_files: usize,
    pub skipped_existing_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanBuild {
    pub plans: Vec<TransferPlan>,
    pub stats: PlanningStats,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    Io {
        context: String,
        path: Option<PathBuf>,
        message: String,
    },
    InvalidTemplate {
        template: String,
        reason: String,
    },
    Collision {
        destination: PathBuf,
        sources: Vec<PathBuf>,
    },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::Io {
                context,
                path,
                message,
            } => {
                if let Some(path) = path {
                    write!(f, "{context}: {}: {message}", path.display())
                } else {
                    write!(f, "{context}: {message}")
                }
            }
            PlanError::InvalidTemplate { template, reason } => {
                write!(f, "invalid layout template `{template}`: {reason}")
            }
            PlanError::Collision {
                destination,
                sources,
            } => {
                let joined = sources
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "destination collision at {} for {} source(s): {}",
                    destination.display(),
                    sources.len(),
                    joined
                )
            }
        }
    }
}

impl Error for PlanError {}

pub type Result<T> = std::result::Result<T, PlanError>;

/// A unique identifier for a file based on its device and inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FileId {
    device: u64,
    inode: u64,
}

impl From<&fs::Metadata> for FileId {
    fn from(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            FileId {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            // On non-Unix, use file path as fallback (may have collisions)
            let path = "";
            let mut h = std::collections::hash_map::DefaultHasher::new();
            path.hash(&mut h);
            FileId {
                device: 0,
                inode: h.finish(),
            }
        }
    }
}

pub fn build_plan<F>(job: &PlanJob, force: bool, mut context_for: F) -> Result<PlanBuild>
where
    F: FnMut(&Path, &fs::Metadata) -> Result<FileContext>,
{
    let extensions = normalize_extensions(&job.extensions);
    let mut candidates = BTreeMap::<PathBuf, Vec<TransferPlan>>::new();
    let mut seen_files: HashSet<FileId> = HashSet::new();
    let mut stats = PlanningStats::default();

    for entry in WalkDir::new(&job.source).follow_links(false) {
        let entry = entry.map_err(|err| PlanError::Io {
            context: "failed while scanning source tree".to_string(),
            path: err.path().map(|path| path.to_path_buf()),
            message: err.to_string(),
        })?;

        if !entry.file_type().is_file() {
            continue;
        }

        let source = entry.path().to_path_buf();
        let ext = match extension_of(&source) {
            Some(ext) => ext,
            None => continue,
        };

        if !extensions.iter().any(|allowed| allowed == &ext) {
            continue;
        }

        stats.scanned_files += 1;

        let metadata = fs::metadata(&source).map_err(|err| PlanError::Io {
            context: "failed to stat source file".to_string(),
            path: Some(source.clone()),
            message: err.to_string(),
        })?;

        // Deduplicate by inode/device to handle symlinked directories
        // where the same physical file may be discovered via multiple paths
        let file_id = FileId::from(&metadata);
        if !seen_files.insert(file_id) {
            continue; // Skip this file, already processed via another path
        }
        let file_name = file_name_string(&source)?;
        let mut ctx = context_for(&source, &metadata)?;
        if ctx.source_rel_dir.is_empty() {
            ctx.source_rel_dir = relative_source_dir(&job.source, &source)?;
        }
        let rel_dest = render_layout(&job.template, &ctx)?;
        for target in &job.targets {
            let dest = target.join(&rel_dest);
            let plan = TransferPlan {
                source: source.clone(),
                dest: dest.clone(),
                size: metadata.len(),
                display_name: file_name.clone(),
            };

            candidates.entry(dest).or_default().push(plan);
        }
    }

    resolve_collisions(&mut candidates)?;

    let mut plans = Vec::new();
    for plan in candidates.into_values().flatten() {
        let source_metadata = fs::metadata(&plan.source).map_err(|err| PlanError::Io {
            context: "failed to stat source file".to_string(),
            path: Some(plan.source.clone()),
            message: err.to_string(),
        })?;
        if !force && should_skip_existing(job.compare_policy, &source_metadata, &plan.dest)? {
            stats.skipped_existing_files += 1;
            stats.skipped_existing_bytes += plan.size;
            continue;
        }
        stats.planned_files += 1;
        stats.planned_bytes += plan.size;
        plans.push(plan);
    }

    plans.sort_by(|a, b| a.dest.cmp(&b.dest));
    Ok(PlanBuild { plans, stats })
}

fn resolve_collisions(candidates: &mut BTreeMap<PathBuf, Vec<TransferPlan>>) -> Result<()> {
    let collisions: Vec<(PathBuf, Vec<TransferPlan>)> = candidates
        .iter()
        .filter(|(_, plans)| plans.len() > 1)
        .map(|(dest, plans)| (dest.clone(), plans.clone()))
        .collect();

    for (destination, plans) in collisions {
        let Some(plan) = dedupe_identical_collision_sources(&plans)? else {
            let mut sources: Vec<PathBuf> = plans.into_iter().map(|plan| plan.source).collect();
            sources.sort();
            sources.dedup();
            return Err(PlanError::Collision {
                destination,
                sources,
            });
        };

        candidates.insert(destination, vec![plan]);
    }

    Ok(())
}

fn dedupe_identical_collision_sources(plans: &[TransferPlan]) -> Result<Option<TransferPlan>> {
    if plans.len() < 2 {
        return Ok(plans.first().cloned());
    }

    let mut sorted = plans.to_vec();
    sorted.sort_by(|a, b| a.source.cmp(&b.source));

    let first = &sorted[0];
    if sorted.iter().any(|plan| plan.size != first.size) {
        return Ok(None);
    }

    for plan in sorted.iter().skip(1) {
        if !files_match(&first.source, &plan.source)? {
            return Ok(None);
        }
    }

    Ok(Some(first.clone()))
}

fn files_match(left: &Path, right: &Path) -> Result<bool> {
    const BUFFER_SIZE: usize = 64 * 1024;

    let left_file = fs::File::open(left).map_err(|err| PlanError::Io {
        context: "failed to open source file for collision comparison".to_string(),
        path: Some(left.to_path_buf()),
        message: err.to_string(),
    })?;
    let right_file = fs::File::open(right).map_err(|err| PlanError::Io {
        context: "failed to open source file for collision comparison".to_string(),
        path: Some(right.to_path_buf()),
        message: err.to_string(),
    })?;

    let mut left_reader = BufReader::new(left_file);
    let mut right_reader = BufReader::new(right_file);
    let mut left_buf = [0u8; BUFFER_SIZE];
    let mut right_buf = [0u8; BUFFER_SIZE];

    loop {
        let left_read = left_reader
            .read(&mut left_buf)
            .map_err(|err| PlanError::Io {
                context: "failed to read source file for collision comparison".to_string(),
                path: Some(left.to_path_buf()),
                message: err.to_string(),
            })?;
        let right_read = right_reader
            .read(&mut right_buf)
            .map_err(|err| PlanError::Io {
                context: "failed to read source file for collision comparison".to_string(),
                path: Some(right.to_path_buf()),
                message: err.to_string(),
            })?;

        if left_read != right_read {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
        if left_buf[..left_read] != right_buf[..right_read] {
            return Ok(false);
        }
    }
}

pub fn should_skip_existing(
    compare_policy: ComparePolicy,
    source_metadata: &fs::Metadata,
    dest: &Path,
) -> Result<bool> {
    let dest_metadata = match fs::metadata(dest) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(PlanError::Io {
                context: "failed to stat destination file".to_string(),
                path: Some(dest.to_path_buf()),
                message: err.to_string(),
            });
        }
    };

    let same = match compare_policy {
        ComparePolicy::Path => true,
        ComparePolicy::PathSize => source_metadata.len() == dest_metadata.len(),
        ComparePolicy::SizeMtime => {
            source_metadata.len() == dest_metadata.len()
                && file_mtime_seconds(source_metadata)? == file_mtime_seconds(&dest_metadata)?
        }
    };

    Ok(same)
}

pub fn render_layout(template: &str, ctx: &FileContext) -> Result<PathBuf> {
    let mut rendered = template.to_string();
    let replacements = [
        ("{year}", ctx.year.as_str()),
        ("{month}", ctx.month.as_str()),
        ("{day}", ctx.day.as_str()),
        ("{ext}", ctx.ext.as_str()),
        ("{stem}", ctx.stem.as_str()),
        ("{filename}", ctx.filename.as_str()),
        ("{source_rel_dir}", ctx.source_rel_dir.as_str()),
    ];

    for (token, value) in replacements {
        rendered = rendered.replace(token, value);
    }

    if rendered.contains('{') || rendered.contains('}') {
        return Err(PlanError::InvalidTemplate {
            template: template.to_string(),
            reason: format!("unresolved token in `{rendered}`"),
        });
    }

    let path = PathBuf::from(rendered);
    validate_rendered_path(template, &path)
}

pub fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
}

pub fn relative_source_dir(source_root: &Path, source_file: &Path) -> Result<String> {
    let relative = source_file
        .strip_prefix(source_root)
        .map_err(|_| PlanError::Io {
            context: "failed to compute source-relative path".to_string(),
            path: Some(source_file.to_path_buf()),
            message: format!(
                "{} is not under {}",
                source_file.display(),
                source_root.display()
            ),
        })?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    Ok(path_to_token(parent))
}

pub fn path_to_token(path: &Path) -> String {
    let mut parts = Vec::new();
    for component in path.components() {
        if let Component::Normal(value) = component {
            parts.push(value.to_string_lossy().to_string());
        }
    }
    parts.join("/")
}

fn file_name_string(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.to_string())
        .ok_or_else(|| PlanError::Io {
            context: "invalid UTF-8 file name".to_string(),
            path: Some(path.to_path_buf()),
            message: "file name was not valid UTF-8".to_string(),
        })
}

fn validate_rendered_path(template: &str, path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Err(PlanError::InvalidTemplate {
            template: template.to_string(),
            reason: "rendered path must be relative".to_string(),
        });
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => continue,
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => {
                return Err(PlanError::InvalidTemplate {
                    template: template.to_string(),
                    reason: "rendered path contains `..`".to_string(),
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(PlanError::InvalidTemplate {
                    template: template.to_string(),
                    reason: "rendered path must be relative".to_string(),
                });
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err(PlanError::InvalidTemplate {
            template: template.to_string(),
            reason: "rendered path cannot be empty".to_string(),
        });
    }

    Ok(normalized)
}

fn file_mtime_seconds(metadata: &fs::Metadata) -> Result<i64> {
    Ok(FileTime::from_last_modification_time(metadata).unix_seconds())
}
