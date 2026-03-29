use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use chrono_tz::America::Los_Angeles;
use pathsync::config;
use pathsync::date;
use pathsync::error::ConfigError;
use pathsync::policy::{ComparePolicy, TimezonePolicy, TransferPolicy};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn compare_mode_defaults_to_size_mtime() {
    let parsed: config::CompareConfig = toml::Value::Table(Default::default())
        .try_into()
        .expect("compare config parses");
    assert_eq!(
        config::resolve_compare_policy(Some(&parsed)).unwrap(),
        ComparePolicy::SizeMtime
    );
}

#[test]
fn explicit_compare_modes_are_preserved() {
    let path: config::CompareConfig = toml::Value::Table({
        let mut table = toml::map::Map::new();
        table.insert("mode".to_string(), toml::Value::String("path".to_string()));
        table
    })
    .try_into()
    .unwrap();
    let path_size: config::CompareConfig = toml::Value::Table({
        let mut table = toml::map::Map::new();
        table.insert(
            "mode".to_string(),
            toml::Value::String("path_size".to_string()),
        );
        table
    })
    .try_into()
    .unwrap();
    let size_mtime: config::CompareConfig = toml::Value::Table({
        let mut table = toml::map::Map::new();
        table.insert(
            "mode".to_string(),
            toml::Value::String("size_mtime".to_string()),
        );
        table
    })
    .try_into()
    .unwrap();

    assert_eq!(
        config::resolve_compare_policy(Some(&path)).unwrap(),
        ComparePolicy::Path
    );
    assert_eq!(
        config::resolve_compare_policy(Some(&path_size)).unwrap(),
        ComparePolicy::PathSize
    );
    assert_eq!(
        config::resolve_compare_policy(Some(&size_mtime)).unwrap(),
        ComparePolicy::SizeMtime
    );
}

#[test]
fn transfer_mode_defaults_to_standard() {
    let parsed: config::TransferConfig = toml::Value::Table(Default::default()).try_into().unwrap();
    assert_eq!(
        config::resolve_transfer_policy(Some(&parsed), 4).unwrap(),
        TransferPolicy::Standard
    );
}

#[test]
fn adaptive_transfer_defaults_large_file_slots_to_parallel_budget() {
    let adaptive: config::TransferConfig = toml::Value::Table({
        let mut table = toml::map::Map::new();
        table.insert(
            "mode".to_string(),
            toml::Value::String("adaptive".to_string()),
        );
        table.insert(
            "large_file_threshold_mb".to_string(),
            toml::Value::Integer(100),
        );
        table
    })
    .try_into()
    .unwrap();

    assert_eq!(
        config::resolve_transfer_policy(Some(&adaptive), 4).unwrap(),
        TransferPolicy::Adaptive {
            large_file_threshold_bytes: 100 * 1024 * 1024,
            large_file_slots: 4,
        }
    );
}

#[test]
fn adaptive_transfer_preserves_explicit_large_file_slots() {
    let adaptive: config::TransferConfig = toml::Value::Table({
        let mut table = toml::map::Map::new();
        table.insert(
            "mode".to_string(),
            toml::Value::String("adaptive".to_string()),
        );
        table.insert(
            "large_file_threshold_mb".to_string(),
            toml::Value::Integer(100),
        );
        table.insert("large_file_slots".to_string(), toml::Value::Integer(2));
        table
    })
    .try_into()
    .unwrap();

    assert_eq!(
        config::resolve_transfer_policy(Some(&adaptive), 4).unwrap(),
        TransferPolicy::Adaptive {
            large_file_threshold_bytes: 100 * 1024 * 1024,
            large_file_slots: 2,
        }
    );
}

#[test]
fn adaptive_transfer_rejects_zero_large_file_slots() {
    let adaptive: config::TransferConfig = toml::Value::Table({
        let mut table = toml::map::Map::new();
        table.insert(
            "mode".to_string(),
            toml::Value::String("adaptive".to_string()),
        );
        table.insert("large_file_slots".to_string(), toml::Value::Integer(0));
        table
    })
    .try_into()
    .unwrap();

    assert!(matches!(
        config::resolve_transfer_policy(Some(&adaptive), 4),
        Err(ConfigError::InvalidAdaptiveLargeFileSlots)
    ));
}

#[test]
fn layout_templates_and_preset_resolution_work() {
    let flat: config::LayoutConfig = toml::Value::String("flat".to_string()).try_into().unwrap();
    let templated: config::LayoutConfig = toml::Value::Table({
        let mut table = toml::map::Map::new();
        table.insert(
            "kind".to_string(),
            toml::Value::String("template".to_string()),
        );
        table.insert(
            "value".to_string(),
            toml::Value::String("{year}/{month}/{filename}".to_string()),
        );
        table
    })
    .try_into()
    .unwrap();

    assert_eq!(config::layout_to_template(&flat).unwrap(), "{filename}");
    assert_eq!(
        config::layout_to_template(&templated).unwrap(),
        "{year}/{month}/{filename}"
    );
}

#[test]
fn image_exif_date_is_used_when_present_regardless_of_timezone_policy() {
    let root = unique_temp_dir("pathsync-config-date-exif");
    fs::create_dir_all(&root).unwrap();
    let image_path = root.join("photo.jpg");
    write_file(&image_path, &jpeg_with_exif_datetime("2024:06:08 09:10:11"));

    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_704_069_000);
    let utc = date::extract_date_parts(&image_path, modified, &TimezonePolicy::Utc).unwrap();
    let la = date::extract_date_parts(&image_path, modified, &TimezonePolicy::Named(Los_Angeles))
        .unwrap();

    assert_eq!(utc, ("2024".into(), "06".into(), "08".into()));
    assert_eq!(la, utc);
}

#[test]
fn image_without_exif_falls_back_to_configured_timezone() {
    let root = unique_temp_dir("pathsync-config-date-no-exif");
    fs::create_dir_all(&root).unwrap();
    let image_path = root.join("photo.jpg");
    write_file(&image_path, b"not-a-real-jpeg");

    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_704_069_000);
    let utc = date::extract_date_parts(&image_path, modified, &TimezonePolicy::Utc).unwrap();
    let la = date::extract_date_parts(&image_path, modified, &TimezonePolicy::Named(Los_Angeles))
        .unwrap();

    assert_eq!(utc, ("2024".into(), "01".into(), "01".into()));
    assert_eq!(la, ("2023".into(), "12".into(), "31".into()));
}

#[test]
fn image_filename_pattern_is_ignored_without_exif() {
    let root = unique_temp_dir("pathsync-config-date-pattern");
    fs::create_dir_all(&root).unwrap();
    let image_path = root.join("IMG_19990102.jpg");
    write_file(&image_path, b"jpeg-without-exif");

    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_704_069_000);
    let parts = date::extract_date_parts(&image_path, modified, &TimezonePolicy::Utc).unwrap();
    assert_eq!(parts, ("2024".into(), "01".into(), "01".into()));
}

#[test]
fn non_image_files_use_mtime_and_timezone_policy() {
    let root = unique_temp_dir("pathsync-config-date-video");
    fs::create_dir_all(&root).unwrap();
    let video_path = root.join("clip.mp4");
    write_file(&video_path, b"video-bytes");

    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_704_069_000);
    let parts = date::extract_date_parts(&video_path, modified, &TimezonePolicy::Utc).unwrap();
    assert_eq!(parts, ("2024".into(), "01".into(), "01".into()));
}

#[test]
fn resolve_job_defaults_compare_transfer_and_timezone_policy() {
    let root = unique_temp_dir("pathsync-config-date");
    let input = root.join("input");
    let output = root.join("output");
    fs::create_dir_all(&input).unwrap();
    fs::create_dir_all(&output).unwrap();

    let raw = r#"
default_job = "vlog"

[jobs.vlog]
enabled = true
source = "{input}"
target = "{output}"
extensions = ["mp4", "jpg"]
layout = "flat"
"#;
    let raw = raw
        .replace("{input}", &input.display().to_string())
        .replace("{output}", &output.display().to_string());

    let config: config::Config = toml::from_str(&raw).unwrap();
    let job = config::resolve_job(&config, None, None, false, None).unwrap();

    assert_eq!(job.name, "vlog");
    assert_eq!(job.compare_policy, ComparePolicy::SizeMtime);
    assert_eq!(job.transfer_policy, TransferPolicy::Standard);
    assert_eq!(job.template, "{filename}");
    assert_eq!(job.timezone_policy, TimezonePolicy::Local);
    assert_eq!(job.source, input);
    assert_eq!(job.target, output);
}

#[test]
fn resolve_job_prefers_job_timezone_over_global_default() {
    let root = unique_temp_dir("pathsync-config-timezone");
    let input = root.join("input");
    let output = root.join("output");
    fs::create_dir_all(&input).unwrap();
    fs::create_dir_all(&output).unwrap();

    let raw = r#"
default_job = "vlog"
timezone = "UTC"

[jobs.vlog]
enabled = true
source = "{input}"
target = "{output}"
extensions = ["jpg"]
layout = "flat"
timezone = "America/Los_Angeles"
"#;
    let raw = raw
        .replace("{input}", &input.display().to_string())
        .replace("{output}", &output.display().to_string());

    let config: config::Config = toml::from_str(&raw).unwrap();
    let job = config::resolve_job(&config, None, None, false, None).unwrap();

    assert_eq!(job.timezone_policy, TimezonePolicy::Named(Los_Angeles));
}

#[test]
fn resolve_job_rejects_invalid_timezone_values() {
    let root = unique_temp_dir("pathsync-config-timezone-invalid");
    let input = root.join("input");
    let output = root.join("output");
    fs::create_dir_all(&input).unwrap();
    fs::create_dir_all(&output).unwrap();

    let raw = r#"
default_job = "vlog"

[jobs.vlog]
enabled = true
source = "{input}"
target = "{output}"
extensions = ["jpg"]
layout = "flat"
timezone = "Mars/Olympus"
"#;
    let raw = raw
        .replace("{input}", &input.display().to_string())
        .replace("{output}", &output.display().to_string());

    let config: config::Config = toml::from_str(&raw).unwrap();
    let err = config::resolve_job(&config, None, None, false, None).unwrap_err();

    assert!(matches!(
        err,
        ConfigError::InvalidTimezone { value, .. } if value == "Mars/Olympus"
    ));
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}-{nanos}-{counter}"))
}

fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut file = fs::File::create(path).unwrap();
    file.write_all(contents).unwrap();
}

fn jpeg_with_exif_datetime(datetime: &str) -> Vec<u8> {
    let mut exif_string = datetime.as_bytes().to_vec();
    exif_string.push(0);

    let exif_ifd_offset = 26_u32;
    let datetime_offset = 44_u32;

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42_u16.to_le_bytes());
    tiff.extend_from_slice(&8_u32.to_le_bytes());

    tiff.extend_from_slice(&1_u16.to_le_bytes());
    tiff.extend_from_slice(&0x8769_u16.to_le_bytes());
    tiff.extend_from_slice(&4_u16.to_le_bytes());
    tiff.extend_from_slice(&1_u32.to_le_bytes());
    tiff.extend_from_slice(&exif_ifd_offset.to_le_bytes());
    tiff.extend_from_slice(&0_u32.to_le_bytes());

    tiff.extend_from_slice(&1_u16.to_le_bytes());
    tiff.extend_from_slice(&0x9003_u16.to_le_bytes());
    tiff.extend_from_slice(&2_u16.to_le_bytes());
    tiff.extend_from_slice(&(exif_string.len() as u32).to_le_bytes());
    tiff.extend_from_slice(&datetime_offset.to_le_bytes());
    tiff.extend_from_slice(&0_u32.to_le_bytes());
    tiff.extend_from_slice(&exif_string);

    let mut app1_payload = Vec::new();
    app1_payload.extend_from_slice(b"Exif\0\0");
    app1_payload.extend_from_slice(&tiff);

    let segment_length = (app1_payload.len() + 2) as u16;
    let mut jpeg = Vec::new();
    jpeg.extend_from_slice(&[0xFF, 0xD8]);
    jpeg.extend_from_slice(&[0xFF, 0xE1]);
    jpeg.extend_from_slice(&segment_length.to_be_bytes());
    jpeg.extend_from_slice(&app1_payload);
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    jpeg
}
