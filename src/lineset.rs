//! A persisted set of strings: a UTF-8 text file with one entry per line.
//!
//! Shared by the skip list and the kept list. Lines for entries that no longer
//! matter (stale paths, removed files) are preserved untouched; CRLF line
//! endings and blank lines are tolerated.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub struct LineSet {
    /// Full path of the backing file.
    path: PathBuf,
    /// Non-empty lines in file order (including stale entries), used when rewriting.
    lines: Vec<String>,
    /// Fast membership lookup over `lines`.
    set: HashSet<String>,
    /// The file on disk is non-empty and does not end with `\n`, so the next
    /// append must start with a newline to avoid merging two entries.
    needs_newline: bool,
}

impl LineSet {
    /// Loads the set stored at `path`. A missing or unreadable file yields an empty set.
    pub fn load(path: PathBuf) -> LineSet {
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

        LineSet {
            path,
            lines,
            set,
            needs_newline,
        }
    }

    pub fn contains(&self, entry: &str) -> bool {
        self.set.contains(entry)
    }

    /// Appends `entry` to the file. No-op if already present.
    /// In-memory state is updated even if the write fails.
    pub fn add(&mut self, entry: &str) -> std::io::Result<()> {
        if !self.set.insert(entry.to_string()) {
            return Ok(());
        }
        self.lines.push(entry.to_string());

        let mut record = String::new();
        if self.needs_newline {
            record.push('\n');
        }
        record.push_str(entry);
        record.push('\n');

        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        file.write_all(record.as_bytes())?;
        self.needs_newline = false;
        Ok(())
    }

    /// Rewrites the file without `entry`. No-op if absent.
    /// In-memory state is updated even if the write fails.
    pub fn remove(&mut self, entry: &str) -> std::io::Result<()> {
        if !self.set.remove(entry) {
            return Ok(());
        }
        self.lines.retain(|line| line != entry);

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
