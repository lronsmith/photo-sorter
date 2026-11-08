//! Resolving when a media file was taken, formatting it, and file times.

use std::fs::{File, FileTimes, OpenOptions};
use std::io::BufReader;
#[cfg(windows)]
use std::os::windows::fs::FileTimesExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{NaiveDateTime, TimeZone};

use crate::media::MediaKind;
use crate::sidecar::{find_sidecar, read_taken_time};

const EXIF_DATETIME_FORMAT: &str = "%Y:%m:%d %H:%M:%S";
const DISPLAY_FORMAT: &str = "%-d %b %Y %H:%M";

/// A file's creation and last-modified times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Times {
    pub created: SystemTime,
    pub modified: SystemTime,
}

/// Parses an EXIF `"YYYY:MM:DD HH:MM:SS"`, ignoring trailing NULs/whitespace.
pub fn parse_exif_datetime(s: &str) -> Option<NaiveDateTime> {
    let trimmed = s.trim_end_matches(|c: char| c == '\0' || c.is_whitespace());
    NaiveDateTime::parse_from_str(trimmed, EXIF_DATETIME_FORMAT).ok()
}

/// EXIF `DateTimeOriginal`, read as local time, in UTC seconds.
pub fn exif_taken(path: &Path) -> Option<i64> {
    let file = File::open(path).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut BufReader::new(file))
        .ok()?;
    let field = exif.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)?;
    let exif::Value::Ascii(values) = &field.value else {
        return None;
    };
    let text = std::str::from_utf8(values.first()?).ok()?;
    let naive = parse_exif_datetime(text)?;
    let local = chrono::Local.from_local_datetime(&naive).earliest()?;
    Some(local.timestamp())
}

/// When `path` was taken: its Takeout sidecar first, then EXIF for images.
pub fn resolve_taken(path: &Path, kind: MediaKind) -> Option<i64> {
    if let Some(ts) = find_sidecar(path).and_then(|json| read_taken_time(&json)) {
        return Some(ts);
    }
    match kind {
        MediaKind::Image => exif_taken(path),
        MediaKind::Video => None,
    }
}

/// `"Taken 12 Mar 2019 14:02"` in `tz`, or `"Taken: unknown"`.
pub fn format_taken_in<Tz: TimeZone>(ts: Option<i64>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match ts.and_then(|ts| tz.timestamp_opt(ts, 0).single()) {
        Some(dt) => format!("Taken {}", dt.format(DISPLAY_FORMAT)),
        None => "Taken: unknown".to_string(),
    }
}

/// [`format_taken_in`] using the local time zone.
pub fn format_taken(ts: Option<i64>) -> String {
    format_taken_in(ts, &chrono::Local)
}

/// Reads a file's creation and last-modified times.
pub fn get_times(path: &Path) -> std::io::Result<Times> {
    let meta = std::fs::metadata(path)?;
    Ok(Times {
        created: meta.created()?,
        modified: meta.modified()?,
    })
}

/// Sets a file's creation and last-modified times.
pub fn set_times(path: &Path, times: Times) -> std::io::Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    let file_times = FileTimes::new().set_modified(times.modified);
    #[cfg(windows)]
    let file_times = file_times.set_created(times.created);
    file.set_times(file_times)
}

/// UTC seconds to a `SystemTime`; pre-1970 values are supported. Values the
/// platform cannot represent fall back to the Unix epoch rather than panic.
pub fn unix_to_system(ts: i64) -> SystemTime {
    let offset = Duration::from_secs(ts.unsigned_abs());
    let time = if ts >= 0 {
        UNIX_EPOCH.checked_add(offset)
    } else {
        UNIX_EPOCH.checked_sub(offset)
    };
    time.unwrap_or(UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn parses_exif_datetime() {
        let expected = NaiveDate::from_ymd_opt(2019, 3, 12)
            .unwrap()
            .and_hms_opt(14, 2, 0)
            .unwrap();
        assert_eq!(parse_exif_datetime("2019:03:12 14:02:00"), Some(expected));
        assert_eq!(parse_exif_datetime("2019:03:12 14:02:00\0"), Some(expected));
        assert_eq!(parse_exif_datetime("0000:00:00 00:00:00"), None);
        assert_eq!(parse_exif_datetime("garbage"), None);
    }

    #[test]
    fn formats_known_time() {
        assert_eq!(
            format_taken_in(Some(1552399320), &chrono::Utc),
            "Taken 12 Mar 2019 14:02"
        );
    }

    #[test]
    fn formats_unknown() {
        assert_eq!(format_taken(None), "Taken: unknown");
    }

    #[test]
    fn resolve_prefers_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("v.mp4");
        std::fs::write(&video, b"not really a video").unwrap();
        std::fs::write(
            dir.path().join("v.mp4.json"),
            r#"{"photoTakenTime":{"timestamp":"1552399320"}}"#,
        )
        .unwrap();
        assert_eq!(resolve_taken(&video, MediaKind::Video), Some(1552399320));
    }

    #[test]
    fn resolve_none_without_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("p.jpg");
        std::fs::write(&image, b"definitely not a jpeg").unwrap();
        assert_eq!(resolve_taken(&image, MediaKind::Image), None);
    }

    #[test]
    fn unix_to_system_handles_pre_1970() {
        assert_eq!(
            unix_to_system(-86_400),
            UNIX_EPOCH - Duration::from_secs(86_400)
        );
        assert_eq!(unix_to_system(i64::MIN), UNIX_EPOCH);
    }

    #[test]
    fn set_and_get_times_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.bin");
        std::fs::write(&file, b"x").unwrap();
        let t = unix_to_system(1552399320);
        let times = Times {
            created: t,
            modified: t,
        };
        set_times(&file, times).unwrap();
        let got = get_times(&file).unwrap();
        let secs = |s: SystemTime| s.duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs(got.created), 1552399320);
        assert_eq!(secs(got.modified), 1552399320);
    }
}
