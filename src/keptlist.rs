//! Persistent list of kept photos, stored as `<source>/.photo-sorter-kept.txt`.
//!
//! UTF-8, one duplicate key ([`crate::media::MediaItem::dup_key`]: lowercased
//! file name, a tab, the size in bytes) per line. A kept file has left the
//! source folder, but Takeout often holds other copies of it (an album copy and
//! a year-folder copy); recording the key means those copies don't come back
//! as undecided in a later session.

use std::path::Path;

use crate::lineset::LineSet;

pub const KEPT_FILE_NAME: &str = ".photo-sorter-kept.txt";

pub struct KeptList {
    lines: LineSet,
}

impl KeptList {
    /// Loads the kept list for `source`. A missing or unreadable file yields an empty list.
    pub fn load(source: &Path) -> KeptList {
        KeptList {
            lines: LineSet::load(source.join(KEPT_FILE_NAME)),
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.lines.contains(key)
    }

    /// Appends `key` to the file. No-op if already present.
    /// In-memory state is updated even if the write fails.
    pub fn add(&mut self, key: &str) -> std::io::Result<()> {
        self.lines.add(key)
    }

    /// Rewrites the file without `key`. No-op if absent.
    /// In-memory state is updated even if the write fails.
    pub fn remove(&mut self, key: &str) -> std::io::Result<()> {
        self.lines.remove(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn load_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let list = KeptList::load(dir.path());
        assert!(!list.contains("img.jpg\t10"));
        assert!(!dir.path().join(KEPT_FILE_NAME).exists());
    }

    #[test]
    fn add_and_remove_persist() {
        let dir = tempfile::tempdir().unwrap();
        let mut list = KeptList::load(dir.path());
        list.add("img.jpg\t10").unwrap();
        list.add("café.jpg\t7").unwrap();
        assert!(KeptList::load(dir.path()).contains("img.jpg\t10"));
        assert_eq!(
            fs::read_to_string(dir.path().join(KEPT_FILE_NAME)).unwrap(),
            "img.jpg\t10\ncafé.jpg\t7\n"
        );
        list.remove("img.jpg\t10").unwrap();
        let reloaded = KeptList::load(dir.path());
        assert!(!reloaded.contains("img.jpg\t10"));
        assert!(reloaded.contains("café.jpg\t7"));
    }

    #[test]
    fn write_failure_still_updates_memory() {
        let dir = tempfile::tempdir().unwrap();
        let mut list = KeptList::load(&dir.path().join("no-such-folder"));
        assert!(list.add("a.jpg\t1").is_err());
        assert!(list.contains("a.jpg\t1"));
        assert!(list.remove("a.jpg\t1").is_err());
        assert!(!list.contains("a.jpg\t1"));
    }
}
