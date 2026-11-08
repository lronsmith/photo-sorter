use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "dng",
];
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "3gp", "webm", "wmv", "mts",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaItem {
    pub rel_path: String,
    pub abs_path: PathBuf,
    pub kind: MediaKind,
    pub size: u64,
}

/// Classifies a path by its (case-insensitive) extension.
pub fn media_kind(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        Some(MediaKind::Image)
    } else if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

/// Recursively collects media files under `source`, skipping anything inside
/// `exclude`. Results are sorted case-insensitively by `rel_path`, and files
/// sharing a (case-insensitive) file name and size are collapsed to the first.
/// `progress` receives the running count of media files found.
pub fn scan(
    source: &Path,
    exclude: &Path,
    progress: &mut dyn FnMut(usize),
) -> std::io::Result<Vec<MediaItem>> {
    let exclude = fs::canonicalize(exclude).ok();
    let mut items = Vec::new();
    let mut pending = vec![source.to_path_buf()];

    while let Some(dir) = pending.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if dir == source => return Err(err),
            // Unreadable subdirectories are skipped rather than failing the scan.
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                if is_excluded(&path, exclude.as_deref()) {
                    continue;
                }
                pending.push(path);
            } else if file_type.is_file() {
                let Some(kind) = media_kind(&path) else {
                    continue;
                };
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                items.push(MediaItem {
                    rel_path: rel_path(source, &path),
                    abs_path: path,
                    kind,
                    size: metadata.len(),
                });
                if items.len() % 100 == 0 {
                    progress(items.len());
                }
            }
        }
    }
    progress(items.len());

    items.sort_by_cached_key(|item| (item.rel_path.to_lowercase(), item.rel_path.clone()));
    let mut seen = HashSet::new();
    items.retain(|item| {
        let name = item
            .abs_path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        seen.insert((name, item.size))
    });
    Ok(items)
}

fn is_excluded(dir: &Path, exclude: Option<&Path>) -> bool {
    let Some(exclude) = exclude else {
        return false;
    };
    fs::canonicalize(dir).is_ok_and(|canonical| canonical.starts_with(exclude))
}

fn rel_path(source: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(source).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, vec![0u8; bytes]).unwrap();
    }

    fn rel_paths(items: &[MediaItem]) -> Vec<&str> {
        items.iter().map(|i| i.rel_path.as_str()).collect()
    }

    fn scan_all(source: &Path, exclude: &Path) -> Vec<MediaItem> {
        scan(source, exclude, &mut |_| {}).unwrap()
    }

    #[test]
    fn media_kind_matches_extensions() {
        assert_eq!(media_kind(Path::new("IMG.JPG")), Some(MediaKind::Image));
        assert_eq!(media_kind(Path::new("a.heic")), Some(MediaKind::Image));
        assert_eq!(media_kind(Path::new("v.MOV")), Some(MediaKind::Video));
        assert_eq!(media_kind(Path::new("x.json")), None);
        assert_eq!(media_kind(Path::new("noext")), None);
    }

    #[test]
    fn scan_is_recursive_and_ignores_non_media() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path();
        write_file(&src.join("a/1.jpg"), 1);
        write_file(&src.join("a/b/2.mp4"), 2);
        write_file(&src.join("a/1.jpg.json"), 3);
        write_file(&src.join("notes.txt"), 4);
        let items = scan_all(src, &src.join("keep"));
        assert_eq!(rel_paths(&items), ["a/1.jpg", "a/b/2.mp4"]);
        assert_eq!(items[0].kind, MediaKind::Image);
        assert_eq!(items[0].size, 1);
        assert_eq!(items[0].abs_path, src.join("a").join("1.jpg"));
        assert_eq!(items[1].kind, MediaKind::Video);
        assert_eq!(items[1].size, 2);
    }

    #[test]
    fn scan_sorts_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path();
        write_file(&src.join("B/x.jpg"), 1);
        write_file(&src.join("a/y.jpg"), 2);
        write_file(&src.join("a/X.jpg"), 3);
        let items = scan_all(src, &src.join("keep"));
        assert_eq!(rel_paths(&items), ["a/X.jpg", "a/y.jpg", "B/x.jpg"]);
    }

    #[test]
    fn scan_collapses_same_name_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path();
        write_file(&src.join("Photos from 2019/IMG_1.jpg"), 10);
        write_file(&src.join("Album/IMG_1.jpg"), 10);
        let items = scan_all(src, &src.join("keep"));
        assert_eq!(rel_paths(&items), ["Album/IMG_1.jpg"]);
    }

    #[test]
    fn scan_keeps_same_name_different_size() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path();
        write_file(&src.join("Photos from 2019/IMG_1.jpg"), 10);
        write_file(&src.join("Album/IMG_1.jpg"), 11);
        let items = scan_all(src, &src.join("keep"));
        assert_eq!(
            rel_paths(&items),
            ["Album/IMG_1.jpg", "Photos from 2019/IMG_1.jpg"]
        );
    }

    #[test]
    fn scan_excludes_keep_folder() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path();
        write_file(&src.join("keep/k.jpg"), 1);
        write_file(&src.join("s.jpg"), 2);
        let items = scan_all(src, &src.join("keep"));
        assert_eq!(rel_paths(&items), ["s.jpg"]);
    }
}
