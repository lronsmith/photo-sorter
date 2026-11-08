//! The review session: which file is current and what was decided for each.
//!
//! State machine (no UI); the egui app drives it with key presses. Decisions
//! that move a file are split into begin / run / finish so the move can run
//! off the UI thread.

use std::io;
use std::path::{Path, PathBuf};

use crate::dates::resolve_taken;
use crate::fileops::{KeepOutcome, KeptRecord, keep_file, unkeep_file};
use crate::keptlist::KeptList;
use crate::media::MediaItem;
use crate::skiplist::SkipList;

/// The decision recorded for one item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Undecided,
    Kept(KeptRecord),
    Skipped,
}

/// Feedback from an action, for the UI to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    None,
    Error(String),
    Warning(String),
}

/// What starting a decision produced.
pub enum Step {
    /// The decision is complete (nothing had to move).
    Finished(Message),
    /// A file has to move first: run the job, then call [`Session::finish`].
    Pending(FileJob),
}

/// The file move behind a keep or an undo. Owns everything it needs, so it
/// can run on another thread.
pub struct FileJob {
    idx: usize,
    op: FileOp,
}

enum FileOp {
    Keep {
        src: PathBuf,
        keep_dir: PathBuf,
        taken: Option<i64>,
    },
    Unkeep(KeptRecord),
}

/// A finished [`FileJob`], to hand to [`Session::finish`].
pub struct FileJobDone {
    idx: usize,
    result: JobResult,
}

enum JobResult {
    Keep(io::Result<KeepOutcome>),
    Unkeep(io::Result<Option<String>>),
}

impl FileJob {
    /// Does the file I/O. A large cross-drive move can take minutes.
    pub fn run(self) -> FileJobDone {
        let result = match self.op {
            FileOp::Keep {
                src,
                keep_dir,
                taken,
            } => JobResult::Keep(keep_file(&src, &keep_dir, taken)),
            FileOp::Unkeep(record) => JobResult::Unkeep(unkeep_file(&record)),
        };
        FileJobDone {
            idx: self.idx,
            result,
        }
    }
}

struct Entry {
    item: MediaItem,
    status: Status,
    /// `None` until resolved; then the cached `resolve_taken` result.
    taken: Option<Option<i64>>,
}

pub struct Session {
    entries: Vec<Entry>,
    cursor: usize,
    keep_dir: PathBuf,
    skiplist: SkipList,
    keptlist: KeptList,
}

impl Session {
    pub fn new(
        items: Vec<MediaItem>,
        keep_dir: PathBuf,
        skiplist: SkipList,
        keptlist: KeptList,
    ) -> Session {
        // Kept in an earlier session (this file or a copy of it): not offered again.
        let entries: Vec<Entry> = items
            .into_iter()
            .filter(|item| !keptlist.contains(&item.dup_key()))
            .map(|item| {
                let status = if skiplist.contains(&item.rel_path) {
                    Status::Skipped
                } else {
                    Status::Undecided
                };
                Entry {
                    item,
                    status,
                    taken: None,
                }
            })
            .collect();
        let cursor = entries
            .iter()
            .position(|e| e.status != Status::Skipped)
            .unwrap_or(entries.len());
        Session {
            entries,
            cursor,
            keep_dir,
            skiplist,
            keptlist,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The current index; equal to `len()` when done.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_done(&self) -> bool {
        self.cursor >= self.entries.len()
    }

    pub fn current(&self) -> Option<&MediaItem> {
        self.entries.get(self.cursor).map(|e| &e.item)
    }

    pub fn status(&self, idx: usize) -> &Status {
        &self.entries[idx].status
    }

    /// Where the item's file is now: the keep-folder copy if kept, else the original.
    pub fn path_of(&self, idx: usize) -> &Path {
        let entry = &self.entries[idx];
        match &entry.status {
            Status::Kept(rec) => &rec.dest,
            _ => &entry.item.abs_path,
        }
    }

    pub fn item(&self, idx: usize) -> &MediaItem {
        &self.entries[idx].item
    }

    /// When the item was taken, resolved on its original path and cached.
    /// Never invalidated: the sidecar stays in the source folder.
    pub fn taken(&mut self, idx: usize) -> Option<i64> {
        let entry = &mut self.entries[idx];
        *entry
            .taken
            .get_or_insert_with(|| resolve_taken(&entry.item.abs_path, entry.item.kind))
    }

    /// Y: keep the current item (undoing a skip), then advance. Runs any file
    /// move on the calling thread; see [`Session::begin_keep`] to run it elsewhere.
    pub fn keep_current(&mut self) -> Message {
        let step = self.begin_keep();
        self.run_inline(step)
    }

    /// N: skip the current item (undoing a keep), then advance. Runs any file
    /// move on the calling thread; see [`Session::begin_skip`] to run it elsewhere.
    pub fn skip_current(&mut self) -> Message {
        let step = self.begin_skip();
        self.run_inline(step)
    }

    fn run_inline(&mut self, step: Step) -> Message {
        match step {
            Step::Finished(message) => message,
            Step::Pending(job) => self.finish(job.run()),
        }
    }

    /// Starts Y on the current item. If a file has to move, returns the job
    /// without changing the session: run it (on any thread) and pass the
    /// result to [`Session::finish`], calling nothing else that changes the
    /// session in between.
    pub fn begin_keep(&mut self) -> Step {
        if self.is_done() {
            return Step::Finished(Message::None);
        }
        let idx = self.cursor;
        if matches!(self.entries[idx].status, Status::Kept(_)) {
            self.next();
            return Step::Finished(Message::None);
        }
        // Resolve before moving: the sidecar/EXIF lookup needs the original path.
        let taken = self.taken(idx);
        Step::Pending(FileJob {
            idx,
            op: FileOp::Keep {
                src: self.entries[idx].item.abs_path.clone(),
                keep_dir: self.keep_dir.clone(),
                taken,
            },
        })
    }

    /// Starts N on the current item. A plain skip finishes at once; undoing a
    /// keep returns a job, as for [`Session::begin_keep`].
    pub fn begin_skip(&mut self) -> Step {
        if self.is_done() {
            return Step::Finished(Message::None);
        }
        let idx = self.cursor;
        let entry = &mut self.entries[idx];
        let mut warnings = Vec::new();
        match &entry.status {
            Status::Skipped => {}
            Status::Undecided => {
                entry.status = Status::Skipped;
                if let Err(e) = self.skiplist.add(&entry.item.rel_path) {
                    warnings.push(skiplist_warning(e));
                }
            }
            Status::Kept(rec) => {
                return Step::Pending(FileJob {
                    idx,
                    op: FileOp::Unkeep(rec.clone()),
                });
            }
        }
        self.next();
        Step::Finished(message_from(warnings))
    }

    /// Records the outcome of a job from [`Session::begin_keep`] or
    /// [`Session::begin_skip`]. On success the item becomes Kept / Skipped and
    /// the cursor moves past it; on failure nothing changes and an error is
    /// returned.
    pub fn finish(&mut self, done: FileJobDone) -> Message {
        let idx = done.idx;
        let entry = &mut self.entries[idx];
        let mut warnings = Vec::new();
        match done.result {
            JobResult::Keep(Err(e)) => return Message::Error(format!("Couldn't keep file: {e}")),
            JobResult::Keep(Ok(outcome)) => {
                let was_skipped = entry.status == Status::Skipped;
                entry.status = Status::Kept(outcome.record);
                if was_skipped && let Err(e) = self.skiplist.remove(&entry.item.rel_path) {
                    warnings.push(skiplist_warning(e));
                }
                if let Err(e) = self.keptlist.add(&entry.item.dup_key()) {
                    warnings.push(keptlist_warning(e));
                }
                warnings.extend(outcome.warning);
            }
            JobResult::Unkeep(Err(e)) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Message::Error(
                    "Can't undo: a file already exists at the original location".into(),
                );
            }
            JobResult::Unkeep(Err(e)) => {
                return Message::Error(format!("Couldn't undo keep: {e}"));
            }
            JobResult::Unkeep(Ok(restore_warning)) => {
                entry.status = Status::Skipped;
                if let Err(e) = self.skiplist.add(&entry.item.rel_path) {
                    warnings.push(skiplist_warning(e));
                }
                if let Err(e) = self.keptlist.remove(&entry.item.dup_key()) {
                    warnings.push(keptlist_warning(e));
                }
                warnings.extend(restore_warning);
            }
        }
        self.cursor = (idx + 1).min(self.entries.len());
        message_from(warnings)
    }

    /// Moves back one item (saturating); from Done goes to the last item.
    pub fn prev(&mut self) {
        self.cursor = self.cursor.min(self.entries.len()).saturating_sub(1);
    }

    /// Moves forward one item, up to `len()` (Done).
    pub fn next(&mut self) {
        self.cursor = (self.cursor + 1).min(self.entries.len());
    }

    /// `(kept, skipped)`.
    pub fn counts(&self) -> (usize, usize) {
        self.entries
            .iter()
            .fold((0, 0), |(kept, skipped), e| match e.status {
                Status::Kept(_) => (kept + 1, skipped),
                Status::Skipped => (kept, skipped + 1),
                Status::Undecided => (kept, skipped),
            })
    }
}

fn skiplist_warning(e: io::Error) -> String {
    format!("Couldn't update skip list: {e}")
}

fn keptlist_warning(e: io::Error) -> String {
    format!("Couldn't update kept list: {e}")
}

/// `Message::None`, or the warnings joined with `"; "`.
fn message_from(warnings: Vec<String>) -> Message {
    if warnings.is_empty() {
        Message::None
    } else {
        Message::Warning(warnings.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::MediaKind;
    use std::fs;
    use tempfile::TempDir;

    struct Fixture {
        _tmp: TempDir,
        source: PathBuf,
        keep: PathBuf,
        items: Vec<MediaItem>,
    }

    /// A source dir with `names` as real files and an empty keep dir.
    fn fixture(names: &[&str]) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let keep = tmp.path().join("keep");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&keep).unwrap();
        let items = names
            .iter()
            .map(|name| {
                let abs_path = source.join(name);
                fs::write(&abs_path, name.as_bytes()).unwrap();
                MediaItem {
                    rel_path: name.to_string(),
                    abs_path,
                    kind: MediaKind::Image,
                    size: name.len() as u64,
                }
            })
            .collect();
        Fixture {
            _tmp: tmp,
            source,
            keep,
            items,
        }
    }

    impl Fixture {
        fn session(&self) -> Session {
            Session::new(
                self.items.clone(),
                self.keep.clone(),
                SkipList::load(&self.source),
                KeptList::load(&self.source),
            )
        }

        fn keep_count(&self) -> usize {
            fs::read_dir(&self.keep).unwrap().count()
        }
    }

    #[test]
    fn empty_session_is_done() {
        let f = fixture(&[]);
        let s = f.session();
        assert_eq!(s.len(), 0);
        assert_eq!(s.cursor(), 0);
        assert!(s.is_done());
        assert!(s.current().is_none());
    }

    #[test]
    fn resumes_at_first_unskipped() {
        let f = fixture(&["a.jpg", "b.jpg", "c.jpg"]);
        let mut list = SkipList::load(&f.source);
        list.add("a.jpg").unwrap();
        list.add("b.jpg").unwrap();
        let s = f.session();
        assert_eq!(s.cursor(), 2);
        assert_eq!(s.status(0), &Status::Skipped);
        assert_eq!(s.status(1), &Status::Skipped);
        assert_eq!(s.status(2), &Status::Undecided);
        assert_eq!(s.current(), Some(&f.items[2]));
    }

    #[test]
    fn all_skipped_resumes_done() {
        let f = fixture(&["a.jpg"]);
        SkipList::load(&f.source).add("a.jpg").unwrap();
        let s = f.session();
        assert_eq!(s.cursor(), 1);
        assert!(s.is_done());
    }

    #[test]
    fn keep_moves_and_advances() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        let mut s = f.session();
        assert_eq!(s.keep_current(), Message::None);
        let dest = f.keep.join("a.jpg");
        match s.status(0) {
            Status::Kept(rec) => {
                assert_eq!(rec.original, f.items[0].abs_path);
                assert_eq!(rec.dest, dest);
            }
            other => panic!("expected Kept, got {other:?}"),
        }
        assert!(dest.exists());
        assert!(!f.items[0].abs_path.exists());
        assert_eq!(s.cursor(), 1);
        assert_eq!(s.path_of(0), dest.as_path());
        assert_eq!(s.path_of(1), f.items[1].abs_path.as_path());
        assert_eq!(s.item(0), &f.items[0]);
    }

    #[test]
    fn begin_then_finish_off_thread() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        let mut s = f.session();
        let Step::Pending(job) = s.begin_keep() else {
            panic!("keep should need a file move");
        };
        // Nothing has happened yet.
        assert!(f.items[0].abs_path.exists());
        assert_eq!(s.status(0), &Status::Undecided);
        assert_eq!(s.cursor(), 0);
        let done = std::thread::spawn(move || job.run()).join().unwrap();
        assert_eq!(s.finish(done), Message::None);
        assert!(matches!(s.status(0), Status::Kept(_)));
        assert!(!f.items[0].abs_path.exists());
        assert_eq!(s.cursor(), 1);

        // A plain skip needs no job; undoing the keep does.
        assert!(matches!(s.begin_skip(), Step::Finished(Message::None)));
        assert_eq!(s.status(1), &Status::Skipped);
        s.prev();
        s.prev();
        let Step::Pending(job) = s.begin_skip() else {
            panic!("undo should need a file move");
        };
        assert!(matches!(s.status(0), Status::Kept(_)));
        assert_eq!(s.finish(job.run()), Message::None);
        assert_eq!(s.status(0), &Status::Skipped);
        assert!(f.items[0].abs_path.exists());
        assert_eq!(s.cursor(), 1);
    }

    #[test]
    fn keep_missing_file_errors_and_stays() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        let mut s = f.session();
        fs::remove_file(&f.items[0].abs_path).unwrap();
        match s.keep_current() {
            Message::Error(msg) => assert!(msg.starts_with("Couldn't keep file: "), "{msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.status(0), &Status::Undecided);
    }

    #[test]
    fn skip_records_and_advances() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        let mut s = f.session();
        assert_eq!(s.skip_current(), Message::None);
        assert_eq!(s.status(0), &Status::Skipped);
        assert!(SkipList::load(&f.source).contains("a.jpg"));
        assert!(f.items[0].abs_path.exists());
        assert_eq!(s.cursor(), 1);
    }

    #[test]
    fn skip_list_write_failure_warns_but_skips() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        // A directory where the skip file should be makes every write fail.
        fs::create_dir(f.source.join(crate::skiplist::SKIP_FILE_NAME)).unwrap();
        let mut s = f.session();
        match s.skip_current() {
            Message::Warning(msg) => {
                assert!(msg.starts_with("Couldn't update skip list: "), "{msg}")
            }
            other => panic!("expected Warning, got {other:?}"),
        }
        assert_eq!(s.status(0), &Status::Skipped);
        assert_eq!(s.cursor(), 1);
    }

    #[test]
    fn skip_on_kept_undoes() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        let mut s = f.session();
        s.keep_current();
        s.prev();
        assert_eq!(s.skip_current(), Message::None);
        assert!(f.items[0].abs_path.exists());
        assert_eq!(f.keep_count(), 0);
        assert_eq!(s.status(0), &Status::Skipped);
        assert!(SkipList::load(&f.source).contains("a.jpg"));
        assert_eq!(s.cursor(), 1);
    }

    #[test]
    fn skip_on_kept_blocked_when_original_occupied() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        let mut s = f.session();
        s.keep_current();
        fs::write(&f.items[0].abs_path, b"new").unwrap();
        s.prev();
        assert_eq!(
            s.skip_current(),
            Message::Error("Can't undo: a file already exists at the original location".into())
        );
        assert!(matches!(s.status(0), Status::Kept(_)));
        assert_eq!(s.cursor(), 0);
        assert!(!SkipList::load(&f.source).contains("a.jpg"));
        assert_eq!(f.keep_count(), 1);
    }

    #[test]
    fn keep_on_skipped_removes_from_list() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        let mut s = f.session();
        s.skip_current();
        s.prev();
        assert_eq!(s.keep_current(), Message::None);
        assert!(matches!(s.status(0), Status::Kept(_)));
        assert!(!SkipList::load(&f.source).contains("a.jpg"));
        assert_eq!(s.cursor(), 1);
    }

    #[test]
    fn repeat_decisions_are_noops() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        let mut s = f.session();
        s.keep_current();
        s.prev();
        assert_eq!(s.keep_current(), Message::None);
        assert_eq!(s.cursor(), 1);
        assert_eq!(f.keep_count(), 1);

        assert_eq!(s.skip_current(), Message::None);
        s.prev();
        assert_eq!(s.skip_current(), Message::None);
        assert_eq!(s.status(1), &Status::Skipped);
        assert_eq!(s.cursor(), 2);
    }

    #[test]
    fn actions_when_done_are_noops() {
        let f = fixture(&["a.jpg"]);
        let mut s = f.session();
        s.next();
        assert!(s.is_done());
        assert_eq!(s.keep_current(), Message::None);
        assert_eq!(s.skip_current(), Message::None);
        assert_eq!(s.cursor(), 1);
        assert_eq!(s.status(0), &Status::Undecided);
    }

    #[test]
    fn navigation_bounds() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        let mut s = f.session();
        s.prev();
        assert_eq!(s.cursor(), 0);
        s.next();
        s.next();
        assert_eq!(s.cursor(), 2);
        assert!(s.is_done());
        s.next();
        assert_eq!(s.cursor(), 2);
        s.prev();
        assert_eq!(s.cursor(), 1);
        assert!(!s.is_done());
    }

    #[test]
    fn counts_reflect_statuses() {
        let f = fixture(&["a.jpg", "b.jpg", "c.jpg", "d.jpg"]);
        let mut s = f.session();
        assert_eq!(s.counts(), (0, 0));
        s.keep_current();
        s.skip_current();
        s.keep_current();
        assert_eq!(s.counts(), (2, 1));
        // Undo the first keep.
        s.prev();
        s.prev();
        s.prev();
        s.skip_current();
        assert_eq!(s.counts(), (1, 2));
    }

    fn scan_source(f: &Fixture) -> Vec<MediaItem> {
        crate::media::scan(&f.source, &f.keep, &mut |_| {}).unwrap()
    }

    fn session_for(f: &Fixture, items: Vec<MediaItem>) -> Session {
        Session::new(
            items,
            f.keep.clone(),
            SkipList::load(&f.source),
            KeptList::load(&f.source),
        )
    }

    /// Byte-identical copies in an album and a year folder, as Takeout has.
    fn write_copies(f: &Fixture) {
        for dir in ["Album X", "Photos from 2019"] {
            fs::create_dir_all(f.source.join(dir)).unwrap();
            fs::write(f.source.join(dir).join("IMG.jpg"), b"same bytes").unwrap();
        }
    }

    #[test]
    fn kept_copies_do_not_return_next_session() {
        let f = fixture(&[]);
        write_copies(&f);
        let items = scan_source(&f);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].rel_path, "Album X/IMG.jpg");
        let mut s = session_for(&f, items);
        assert_eq!(s.keep_current(), Message::None);
        assert!(KeptList::load(&f.source).contains("img.jpg\t10"));

        // Next session: the year-folder copy is now unique on disk, but it
        // is the photo already kept, so it must not be offered again.
        let items = scan_source(&f);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].rel_path, "Photos from 2019/IMG.jpg");
        let s = session_for(&f, items);
        assert_eq!(s.len(), 0);
        assert!(s.is_done());
        assert_eq!(s.counts(), (0, 0));
        assert!(f.source.join("Photos from 2019/IMG.jpg").exists());
    }

    #[test]
    fn kept_items_excluded_at_startup() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        KeptList::load(&f.source).add(&f.items[0].dup_key()).unwrap();
        let s = f.session();
        assert_eq!(s.len(), 1);
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.current(), Some(&f.items[1]));
    }

    #[test]
    fn undo_keep_removes_kept_key() {
        let f = fixture(&[]);
        write_copies(&f);
        let mut s = session_for(&f, scan_source(&f));
        s.keep_current();
        s.prev();
        assert_eq!(s.skip_current(), Message::None);
        assert!(!KeptList::load(&f.source).contains("img.jpg\t10"));

        // Next session: the album copy is back and remembered as skipped.
        let items = scan_source(&f);
        assert_eq!(items[0].rel_path, "Album X/IMG.jpg");
        let s = session_for(&f, items);
        assert_eq!(s.len(), 1);
        assert_eq!(s.status(0), &Status::Skipped);
    }

    #[test]
    fn kept_list_write_failure_warns_but_keeps() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        // A directory where the kept file should be makes every write fail.
        fs::create_dir(f.source.join(crate::keptlist::KEPT_FILE_NAME)).unwrap();
        let mut s = f.session();
        match s.keep_current() {
            Message::Warning(msg) => {
                assert!(msg.starts_with("Couldn't update kept list: "), "{msg}")
            }
            other => panic!("expected Warning, got {other:?}"),
        }
        assert!(matches!(s.status(0), Status::Kept(_)));
        assert_eq!(s.cursor(), 1);
        // Undo also warns but still moves the file back.
        s.prev();
        match s.skip_current() {
            Message::Warning(msg) => {
                assert!(msg.starts_with("Couldn't update kept list: "), "{msg}")
            }
            other => panic!("expected Warning, got {other:?}"),
        }
        assert_eq!(s.status(0), &Status::Skipped);
        assert!(f.items[0].abs_path.exists());
    }

    #[test]
    fn taken_is_cached_from_original_path() {
        let f = fixture(&["a.jpg", "b.jpg"]);
        let sidecar = f.source.join("a.jpg.json");
        fs::write(&sidecar, r#"{"photoTakenTime":{"timestamp":"1552399320"}}"#).unwrap();
        let mut s = f.session();
        assert_eq!(s.taken(0), Some(1552399320));
        assert_eq!(s.taken(1), None);
        // Cached: survives the sidecar disappearing.
        fs::remove_file(&sidecar).unwrap();
        assert_eq!(s.taken(0), Some(1552399320));
    }

    #[test]
    fn keep_sets_taken_date() {
        let f = fixture(&["a.jpg"]);
        fs::write(
            f.source.join("a.jpg.json"),
            r#"{"photoTakenTime":{"timestamp":"1552399320"}}"#,
        )
        .unwrap();
        let mut s = f.session();
        assert_eq!(s.keep_current(), Message::None);
        let modified = fs::metadata(s.path_of(0)).unwrap().modified().unwrap();
        assert_eq!(modified, crate::dates::unix_to_system(1552399320));
        assert_eq!(s.taken(0), Some(1552399320));
    }
}
