use filetime::{FileTime, set_file_mtime};
use pathsync::plan::{
    FileContext, PlanError, PlanJob, build_plan, path_to_token, relative_source_dir, render_layout,
    should_skip_existing,
};
use pathsync::policy::ComparePolicy;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        path.push(format!(
            "pathsync-plan-{unique}-{}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("failed to create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("failed to create parent directories");
    }
    let mut file = File::create(path).expect("failed to create file");
    file.write_all(contents).expect("failed to write file");
}

fn sample_context(filename: &str) -> FileContext {
    FileContext {
        year: "2026".to_string(),
        month: "03".to_string(),
        day: "29".to_string(),
        ext: "jpg".to_string(),
        stem: filename.trim_end_matches(".jpg").to_string(),
        filename: filename.to_string(),
        source_rel_dir: String::new(),
    }
}

#[test]
fn render_layout_rejects_absolute_and_escape_paths() {
    let context = sample_context("photo.jpg");

    let absolute = render_layout("/tmp/{filename}", &context).unwrap_err();
    assert!(matches!(
        absolute,
        PlanError::InvalidTemplate {
            template: _,
            reason: _
        }
    ));

    let unresolved = render_layout("{filename}/{missing}", &context).unwrap_err();
    assert!(matches!(
        unresolved,
        PlanError::InvalidTemplate {
            template: _,
            reason: _
        }
    ));

    let escape = render_layout("../{filename}", &context).unwrap_err();
    assert!(matches!(
        escape,
        PlanError::InvalidTemplate {
            template: _,
            reason: _
        }
    ));
}

#[test]
fn render_layout_accepts_relative_templates() {
    let context = sample_context("photo.jpg");
    let rendered = render_layout("{year}/{month}/{filename}", &context).unwrap();
    assert_eq!(rendered, PathBuf::from("2026/03/photo.jpg"));
}

#[test]
fn build_plan_reports_destination_collisions() {
    let temp = TempDir::new();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();

    write_file(&source.join("one/photo.jpg"), b"1111");
    write_file(&source.join("two/photo.jpg"), b"2222");

    let job = PlanJob {
        source: source.clone(),
        target: target.clone(),
        extensions: vec!["jpg".to_string()],
        compare_policy: ComparePolicy::PathSize,
        template: "{filename}".to_string(),
    };

    let err = build_plan(&job, false, |path, _metadata| {
        Ok(sample_context(
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("utf-8 filename"),
        ))
    })
    .unwrap_err();

    match err {
        PlanError::Collision {
            destination,
            sources,
        } => {
            assert_eq!(destination, target.join("photo.jpg"));
            assert_eq!(sources.len(), 2);
            assert!(sources.iter().any(|path| path.ends_with("one/photo.jpg")));
            assert!(sources.iter().any(|path| path.ends_with("two/photo.jpg")));
        }
        other => panic!("expected collision error, got {other:?}"),
    }
}

#[test]
fn build_plan_returns_planning_stats() {
    let temp = TempDir::new();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::create_dir_all(&target).unwrap();

    write_file(&source.join("nested/a.jpg"), b"aaaa");
    write_file(&source.join("nested/b.jpg"), b"bbbbbb");
    write_file(&target.join("nested/b.jpg"), b"bbbbbb");

    let job = PlanJob {
        source: source.clone(),
        target: target.clone(),
        extensions: vec!["jpg".to_string()],
        compare_policy: ComparePolicy::PathSize,
        template: "{source_rel_dir}/{filename}".to_string(),
    };

    let build = build_plan(&job, false, |path, _metadata| {
        Ok(sample_context(
            path.file_name()
                .and_then(|name| name.to_str())
                .expect("utf-8 filename"),
        ))
    })
    .unwrap();

    assert_eq!(build.plans.len(), 1);
    assert_eq!(build.stats.scanned_files, 2);
    assert_eq!(build.stats.planned_files, 1);
    assert_eq!(build.stats.planned_bytes, 4);
    assert_eq!(build.stats.skipped_existing_files, 1);
    assert_eq!(build.stats.skipped_existing_bytes, 6);
}

#[test]
fn source_path_helpers_extract_relative_tokens() {
    let temp = TempDir::new();
    let source = temp.path().join("source");
    let file = source.join("nested/one/photo.jpg");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    write_file(&file, b"abcd");

    let relative = relative_source_dir(&source, &file).unwrap();
    assert_eq!(relative, "nested/one");
    assert_eq!(path_to_token(Path::new("nested/one")), "nested/one");
}

#[test]
fn should_skip_existing_supports_size_mtime() {
    let temp = TempDir::new();
    let source = temp.path().join("source.jpg");
    let target = temp.path().join("target.jpg");

    write_file(&source, b"abcd");
    write_file(&target, b"wxyz");

    let mtime = FileTime::from_unix_time(1_700_000_000, 123_456_789);
    set_file_mtime(&source, mtime).unwrap();
    set_file_mtime(&target, mtime).unwrap();

    let source_meta = fs::metadata(&source).unwrap();
    let should_skip =
        should_skip_existing(ComparePolicy::SizeMtime, &source_meta, &target).unwrap();
    assert!(should_skip);

    let later = FileTime::from_unix_time(1_700_000_001, 0);
    set_file_mtime(&target, later).unwrap();
    let should_skip =
        should_skip_existing(ComparePolicy::SizeMtime, &source_meta, &target).unwrap();
    assert!(!should_skip);
}

#[test]
fn should_skip_existing_path_policy_treats_existing_destination_as_match() {
    let temp = TempDir::new();
    let source = temp.path().join("source.jpg");
    let target = temp.path().join("target.jpg");

    write_file(&source, b"abcd");
    write_file(&target, b"wxyz");

    let source_meta = fs::metadata(&source).unwrap();
    let should_skip = should_skip_existing(ComparePolicy::Path, &source_meta, &target).unwrap();
    assert!(should_skip);
}
