//! Persistent list of skipped files, stored as `<source>/.photo-sorter-skipped.txt`.
//!
//! UTF-8, one relative path (with `/` separators) per line. Lines for files
//! that no longer exist are preserved untouched.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SKIP_FILE_NAME: &str = ".photo-sorter-skipped.txt";

pub struct SkipList {
    /// Full path of the skip file.
    path: PathBuf,
    /// Non-empty lines in file order (including stale entries), used when rewriting.
    lines: Vec<String>,
    /// Fast membership lookup over `lines`.
    set: HashSet<String>,
    /// The file on disk is non-empty and does not end with `\n`, so the next
    /// append must start with a newline to avoid merging two entries.
    needs_newline: bool,
}

impl SkipList {
    /// Loads the skip list for `source`. A missing or unreadable file yields an empty list.
    pub fn load(source: &Path) -> SkipList {
        let path = source.join(SKIP_FILE_NAME);
        let content = fs::read(&path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        let needs_newline = !content.is_empty() && !content.ends_with('\n');

        let mut lines = Vec::new();
        let mut set = HashSet::new();
        for line in content.lines() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            lines.push(line.to_string());
            set.insert(line.to_string());
        }

        SkipList {
            path,
            lines,
            set,
            needs_newline,
        }
    }

    pub fn contains(&self, rel_path: &str) -> bool {
        self.set.contains(rel_path)
    }

    /// Appends `rel_path` to the file. No-op if already present.
    /// In-memory state is updated even if the write fails.
    pub fn add(&mut self, rel_path: &str) -> std::io::Result<()> {
        if !self.set.insert(rel_path.to_string()) {
            return Ok(());
        }
        self.lines.push(rel_path.to_string());

        let mut record = String::new();
        if self.needs_newline {
            record.push('\n');
        }
        record.push_str(rel_path);
        record.push('\n');

        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        file.write_all(record.as_bytes())?;
        self.needs_newline = false;
        Ok(())
    }

    /// Rewrites the file without `rel_path`. No-op if absent.
    /// In-memory state is updated even if the write fails.
    pub fn remove(&mut self, rel_path: &str) -> std::io::Result<()> {
        if !self.set.remove(rel_path) {
            return Ok(());
        }
        self.lines.retain(|line| line != rel_path);

        let mut content = String::new();
        for line in &self.lines {
            content.push_str(line);
            content.push('\n');
        }
        fs::write(&self.path, content)?;
        self.needs_newline = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_in(dir: &Path) -> PathBuf {
        dir.join(SKIP_FILE_NAME)
    }

    #[test]
    fn load_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let list = SkipList::load(dir.path());
        assert!(!list.contains("a/1.jpg"));
        assert!(!list.contains(""));
        assert!(!file_in(dir.path()).exists());
    }

    #[test]
    fn add_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut list = SkipList::load(dir.path());
        list.add("a/1.jpg").unwrap();
        assert!(list.contains("a/1.jpg"));
        let reloaded = SkipList::load(dir.path());
        assert!(reloaded.contains("a/1.jpg"));
        assert_eq!(fs::read_to_string(file_in(dir.path())).unwrap(), "a/1.jpg\n");
    }

    #[test]
    fn add_twice_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let mut list = SkipList::load(dir.path());
        list.add("a/1.jpg").unwrap();
        list.add("a/1.jpg").unwrap();
        assert_eq!(fs::read_to_string(file_in(dir.path())).unwrap(), "a/1.jpg\n");
        // Also a no-op after a reload.
        let mut reloaded = SkipList::load(dir.path());
        reloaded.add("a/1.jpg").unwrap();
        assert_eq!(fs::read_to_string(file_in(dir.path())).unwrap(), "a/1.jpg\n");
    }

    #[test]
    fn remove_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut list = SkipList::load(dir.path());
        list.add("a").unwrap();
        list.add("b").unwrap();
        list.remove("a").unwrap();
        assert!(!list.contains("a"));
        assert!(list.contains("b"));
        let reloaded = SkipList::load(dir.path());
        assert!(!reloaded.contains("a"));
        assert!(reloaded.contains("b"));
        assert_eq!(fs::read_to_string(file_in(dir.path())).unwrap(), "b\n");
    }

    #[test]
    fn remove_absent_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let mut list = SkipList::load(dir.path());
        list.remove("nope.jpg").unwrap();
        assert!(!file_in(dir.path()).exists());
    }

    #[test]
    fn stale_lines_preserved() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(file_in(dir.path()), "gone.jpg\nx.jpg\n").unwrap();
        let mut list = SkipList::load(dir.path());
        assert!(list.contains("gone.jpg"));
        list.add("y.jpg").unwrap();
        assert_eq!(
            fs::read_to_string(file_in(dir.path())).unwrap(),
            "gone.jpg\nx.jpg\ny.jpg\n"
        );
        list.remove("x.jpg").unwrap();
        assert_eq!(
            fs::read_to_string(file_in(dir.path())).unwrap(),
            "gone.jpg\ny.jpg\n"
        );
    }

    #[test]
    fn crlf_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(file_in(dir.path()), "x.jpg\r\n").unwrap();
        let list = SkipList::load(dir.path());
        assert!(list.contains("x.jpg"));
        assert!(!list.contains("x.jpg\r"));
    }

    #[test]
    fn empty_lines_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(file_in(dir.path()), "\n\r\nx.jpg\n\n").unwrap();
        let list = SkipList::load(dir.path());
        assert!(list.contains("x.jpg"));
        assert!(!list.contains(""));
    }

    #[test]
    fn missing_trailing_newline_does_not_merge_lines() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(file_in(dir.path()), "x.jpg").unwrap();
        let mut list = SkipList::load(dir.path());
        list.add("y.jpg").unwrap();
        assert_eq!(
            fs::read_to_string(file_in(dir.path())).unwrap(),
            "x.jpg\ny.jpg\n"
        );
        let reloaded = SkipList::load(dir.path());
        assert!(reloaded.contains("x.jpg"));
        assert!(reloaded.contains("y.jpg"));
    }

    #[test]
    fn unicode_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let name = "Fotos/Café 写真 🎉.jpg";
        let mut list = SkipList::load(dir.path());
        list.add(name).unwrap();
        let reloaded = SkipList::load(dir.path());
        assert!(reloaded.contains(name));
        assert_eq!(
            fs::read_to_string(file_in(dir.path())).unwrap(),
            format!("{name}\n")
        );
    }

    #[test]
    fn write_failure_still_updates_memory() {
        let dir = tempfile::tempdir().unwrap();
        // Source folder that does not exist: every write fails.
        let missing = dir.path().join("no-such-folder");
        let mut list = SkipList::load(&missing);
        assert!(list.add("a.jpg").is_err());
        assert!(list.contains("a.jpg"));
        assert!(list.remove("a.jpg").is_err());
        assert!(!list.contains("a.jpg"));
    }
}
