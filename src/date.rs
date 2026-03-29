use chrono::{DateTime, Datelike, Local, Utc};
use exif::{In, Reader, Tag, Value};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::SystemTime;

use crate::error::DateError;
use crate::policy::TimezonePolicy;

pub fn extract_date_parts(
    path: &Path,
    modified: SystemTime,
    timezone_policy: &TimezonePolicy,
) -> Result<(String, String, String), DateError> {
    if is_exif_image(path)
        && let Some(parts) = extract_exif_date_parts(path)?
    {
        return Ok(parts);
    }

    extract_mtime_date_parts(modified, timezone_policy)
}

fn is_exif_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase()),
        Some(ext) if matches!(ext.as_str(), "jpg" | "jpeg" | "tif" | "tiff")
    )
}

fn extract_exif_date_parts(path: &Path) -> Result<Option<(String, String, String)>, DateError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };
    let mut reader = BufReader::new(file);
    let exif = match Reader::new().read_from_container(&mut reader) {
        Ok(exif) => exif,
        Err(_) => return Ok(None),
    };

    let field = exif
        .get_field(Tag::DateTimeOriginal, In::PRIMARY)
        .or_else(|| exif.get_field(Tag::DateTime, In::PRIMARY));
    let Some(field) = field else {
        return Ok(None);
    };

    let Value::Ascii(values) = &field.value else {
        return Ok(None);
    };
    let Some(raw) = values.first() else {
        return Ok(None);
    };
    let Ok(value) = std::str::from_utf8(raw) else {
        return Ok(None);
    };

    let trimmed = value.trim_end_matches('\0');
    let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y:%m:%d %H:%M:%S") else {
        return Ok(None);
    };

    Ok(Some((
        format!("{:04}", parsed.year()),
        format!("{:02}", parsed.month()),
        format!("{:02}", parsed.day()),
    )))
}

fn extract_mtime_date_parts(
    modified: SystemTime,
    timezone_policy: &TimezonePolicy,
) -> Result<(String, String, String), DateError> {
    let utc_datetime: DateTime<Utc> = DateTime::from(modified);

    match timezone_policy {
        TimezonePolicy::Local => {
            let datetime = utc_datetime.with_timezone(&Local);
            Ok(parts_from_datetime(
                datetime.year(),
                datetime.month(),
                datetime.day(),
            ))
        }
        TimezonePolicy::Utc => Ok(parts_from_datetime(
            utc_datetime.year(),
            utc_datetime.month(),
            utc_datetime.day(),
        )),
        TimezonePolicy::Named(tz) => {
            let datetime = utc_datetime.with_timezone(tz);
            Ok(parts_from_datetime(
                datetime.year(),
                datetime.month(),
                datetime.day(),
            ))
        }
    }
}

fn parts_from_datetime(year: i32, month: u32, day: u32) -> (String, String, String) {
    (
        format!("{year:04}"),
        format!("{month:02}"),
        format!("{day:02}"),
    )
}
