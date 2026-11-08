//! Locating Google Takeout JSON sidecars and reading their taken-time.

use std::path::{Path, PathBuf};

/// Takeout caps sidecar file names at this many chars, including `.json`.
const MAX_SIDECAR_CHARS: usize = 51;
/// A capped name keeps this many chars before the `.json` it re-appends.
const TRUNCATED_STEM_CHARS: usize = MAX_SIDECAR_CHARS - ".json".len();

/// Ordered candidate sidecar JSON file names for a media file name.
pub fn sidecar_candidates(file_name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (stem, ext) = split_ext(file_name);
    push_candidates(&mut out, file_name, stem, ext);
    if let Some(original) = stem.strip_suffix("-edited") {
        let original_name = join_ext(original, ext);
        push_candidates(&mut out, &original_name, original, ext);
    }
    out
}

/// The first candidate sidecar that exists next to `media`.
pub fn find_sidecar(media: &Path) -> Option<PathBuf> {
    let name = media.file_name()?.to_str()?;
    let dir = media.parent().unwrap_or(Path::new(""));
    sidecar_candidates(name)
        .into_iter()
        .map(|c| dir.join(c))
        .find(|p| p.is_file())
}

/// `photoTakenTime.timestamp` from a sidecar, as UTC seconds.
pub fn read_taken_time(json: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(json).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    match &value["photoTakenTime"]["timestamp"] {
        serde_json::Value::String(s) if is_digits(s) => s.parse().ok(),
        serde_json::Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// Steps 1–4 of the candidate order for one name, skipping duplicates.
fn push_candidates(out: &mut Vec<String>, name: &str, stem: &str, ext: Option<&str>) {
    let plain = format!("{name}.json");
    let supplemental = format!("{name}.supplemental-metadata.json");
    for full in [&plain, &supplemental] {
        push_unique(out, full.clone());
    }
    for full in [&plain, &supplemental] {
        if full.chars().count() > MAX_SIDECAR_CHARS {
            let before_json = &full[..full.len() - ".json".len()];
            let kept: String = before_json.chars().take(TRUNCATED_STEM_CHARS).collect();
            push_unique(out, format!("{kept}.json"));
        }
    }
    if let Some((base, n)) = split_dup_number(stem) {
        let base_name = join_ext(base, ext);
        push_unique(out, format!("{base_name}({n}).json"));
        push_unique(out, format!("{base_name}.supplemental-metadata({n}).json"));
    }
}

fn push_unique(out: &mut Vec<String>, candidate: String) {
    if !out.contains(&candidate) {
        out.push(candidate);
    }
}

/// Splits `"<stem>.<ext>"` at the final dot; a name with no dot has no ext.
fn split_ext(name: &str) -> (&str, Option<&str>) {
    match name.rsplit_once('.') {
        Some((stem, ext)) => (stem, Some(ext)),
        None => (name, None),
    }
}

fn join_ext(stem: &str, ext: Option<&str>) -> String {
    match ext {
        Some(ext) => format!("{stem}.{ext}"),
        None => stem.to_string(),
    }
}

/// Matches `^(.*)\((\d+)\)$`, returning `(base, digits)`.
fn split_dup_number(stem: &str) -> Option<(&str, &str)> {
    let inner = stem.strip_suffix(')')?;
    let open = inner.rfind('(')?;
    let digits = &inner[open + 1..];
    is_digits(digits).then(|| (&inner[..open], digits))
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn contains(c: &[String], s: &str) -> bool {
        c.iter().any(|x| x == s)
    }

    #[test]
    fn basic_candidates() {
        let c = sidecar_candidates("IMG_1234.jpg");
        assert!(c.len() >= 2, "{c:?}");
        assert_eq!(
            &c[..2],
            &["IMG_1234.jpg.json", "IMG_1234.jpg.supplemental-metadata.json"]
        );
    }

    #[test]
    fn truncated_supplemental() {
        let name = "abcdefghijklmnopqrstuvwxyz0123456789ABCD.jpg";
        assert_eq!(name.chars().count(), 44);
        let c = sidecar_candidates(name);
        assert!(
            contains(&c, "abcdefghijklmnopqrstuvwxyz0123456789ABCD.jpg.s.json"),
            "{c:?}"
        );
    }

    #[test]
    fn truncated_plain() {
        let name = "a".repeat(46) + ".jpg";
        let c = sidecar_candidates(&name);
        assert!(contains(&c, &("a".repeat(46) + ".json")), "{c:?}");
    }

    #[test]
    fn truncated_multibyte_name() {
        let name = "é".repeat(46) + ".jpg";
        let c = sidecar_candidates(&name);
        assert!(contains(&c, &("é".repeat(46) + ".json")), "{c:?}");
    }

    #[test]
    fn numbered_duplicate() {
        let c = sidecar_candidates("IMG_1234(1).jpg");
        assert!(contains(&c, "IMG_1234.jpg(1).json"), "{c:?}");
        assert!(
            contains(&c, "IMG_1234.jpg.supplemental-metadata(1).json"),
            "{c:?}"
        );
    }

    #[test]
    fn edited_uses_original() {
        let c = sidecar_candidates("IMG_1234-edited.jpg");
        let edited = c.iter().position(|x| x == "IMG_1234-edited.jpg.json");
        let original = c.iter().position(|x| x == "IMG_1234.jpg.json");
        assert!(edited.is_some() && original.is_some(), "{c:?}");
        assert!(edited < original, "{c:?}");
    }

    #[test]
    fn candidates_have_no_duplicates() {
        // A short name never needs truncation, so the truncation step must not
        // re-add the untruncated names; and an edited name must not repeat them.
        for name in ["IMG_1234-edited.jpg", "IMG_1234(1).jpg", "x"] {
            let c = sidecar_candidates(name);
            let mut seen = std::collections::HashSet::new();
            assert!(c.iter().all(|x| seen.insert(x)), "{c:?}");
            assert!(!c.is_empty(), "{name}");
        }
    }

    #[test]
    fn find_sidecar_prefers_first_existing() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("x.jpg");
        fs::write(&media, b"").unwrap();
        let plain = dir.path().join("x.jpg.json");
        let supp = dir.path().join("x.jpg.supplemental-metadata.json");

        assert_eq!(find_sidecar(&media), None);

        fs::write(&supp, b"{}").unwrap();
        assert_eq!(find_sidecar(&media), Some(supp.clone()));

        fs::write(&plain, b"{}").unwrap();
        assert_eq!(find_sidecar(&media), Some(plain));
    }

    fn write_json(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.jpg.json");
        fs::write(&p, contents).unwrap();
        (dir, p)
    }

    #[test]
    fn read_taken_time_string() {
        let (_d, p) =
            write_json(r#"{"photoTakenTime":{"timestamp":"1552399320","formatted":"x"}}"#);
        assert_eq!(read_taken_time(&p), Some(1552399320));
    }

    #[test]
    fn read_taken_time_number() {
        let (_d, p) = write_json(r#"{"photoTakenTime":{"timestamp":1552399320}}"#);
        assert_eq!(read_taken_time(&p), Some(1552399320));
    }

    #[test]
    fn read_taken_time_missing_or_bad() {
        let (_d, p) = write_json("{}");
        assert_eq!(read_taken_time(&p), None);
        let (_d2, p2) = write_json("not json");
        assert_eq!(read_taken_time(&p2), None);
        let (_d3, p3) = write_json(r#"{"photoTakenTime":{"timestamp":"12ab"}}"#);
        assert_eq!(read_taken_time(&p3), None);
        let (_d4, p4) = write_json(r#"{"photoTakenTime":{"timestamp":1.5}}"#);
        assert_eq!(read_taken_time(&p4), None);
        let missing = tempfile::tempdir().unwrap().path().join("nope.json");
        assert_eq!(read_taken_time(&missing), None);
    }
}
