//! Keeping (moving) files into the keep folder and undoing that.

use std::fs::{self, File, FileTimes, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::FileTimesExt;
use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW};
use windows::core::PCWSTR;

use crate::dates::{Times, get_times, set_times, unix_to_system};

/// Where a kept file came from, where it went, and its times before the move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptRecord {
    pub original: PathBuf,
    pub dest: PathBuf,
    pub original_times: Times,
}

/// The result of a successful keep, with a warning if dates couldn't be set.
pub struct KeepOutcome {
    pub record: KeptRecord,
    pub warning: Option<String>,
}

/// Windows `ERROR_NOT_SAME_DEVICE`: a rename across drives.
const ERROR_NOT_SAME_DEVICE: i32 = 17;

/// A path in `dir` for `file_name` that doesn't exist yet: `IMG.jpg`, then
/// `IMG (1).jpg`, `IMG (2).jpg`, ...
pub fn unique_dest(dir: &Path, file_name: &str) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let name = Path::new(file_name);
    let stem = name
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name.to_string());
    let ext = name.extension().map(|e| e.to_string_lossy().into_owned());
    (1u64..)
        .map(|n| match &ext {
            Some(ext) => dir.join(format!("{stem} ({n}).{ext}")),
            None => dir.join(format!("{stem} ({n})")),
        })
        .find(|p| !p.exists())
        .expect("ran out of collision suffixes")
}

/// Moves `src` to `dest`, falling back to copy-verify-delete across drives.
/// Never replaces an existing `dest`: that fails with `AlreadyExists`.
pub fn move_file(src: &Path, dest: &Path) -> io::Result<()> {
    match rename_no_replace(src, dest) {
        Err(e) if e.raw_os_error() == Some(ERROR_NOT_SAME_DEVICE) => copy_verify_delete(src, dest),
        other => other,
    }
}

/// Same-volume rename via `MoveFileExW` with no flags: no replacing an
/// existing destination and no implicit cross-volume copy.
fn rename_no_replace(src: &Path, dest: &Path) -> io::Result<()> {
    let src_w = to_wide(src);
    let dest_w = to_wide(dest);
    // SAFETY: both buffers are NUL-terminated UTF-16 and outlive the call.
    unsafe {
        MoveFileExW(
            PCWSTR(src_w.as_ptr()),
            PCWSTR(dest_w.as_ptr()),
            MOVE_FILE_FLAGS(0),
        )
    }
    .map_err(win32_to_io)
}

fn to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// Turns a `HRESULT_FROM_WIN32` error back into an `io::Error` carrying the
/// raw Win32 code, so `raw_os_error()` and `kind()` behave as for std calls.
fn win32_to_io(e: windows::core::Error) -> io::Error {
    let hr = e.code().0 as u32;
    if hr & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((hr & 0xFFFF) as i32)
    } else {
        io::Error::other(e)
    }
}

/// Copies `src` to a newly created `dest` (failing with `AlreadyExists`
/// rather than overwriting), carries over its created/modified times, checks
/// the sizes match, then deletes `src`. If anything fails after `dest` was
/// created, only that copy is removed; `src` is never deleted on failure.
pub fn copy_verify_delete(src: &Path, dest: &Path) -> io::Result<()> {
    let source = File::open(src)?;
    let target = OpenOptions::new().write(true).create_new(true).open(dest)?;
    let result = copy_into_then_delete_source(source, target, src);
    if result.is_err() {
        // Best effort: the original error matters more than a cleanup failure.
        let _ = fs::remove_file(dest);
    }
    result
}

/// The part of [`copy_verify_delete`] after `dest` exists. Takes the handles by
/// value so both are closed before the caller cleans up or `src` is removed.
fn copy_into_then_delete_source(mut source: File, mut target: File, src: &Path) -> io::Result<()> {
    let src_meta = source.metadata()?;
    io::copy(&mut source, &mut target)?;
    let times = FileTimes::new()
        .set_modified(src_meta.modified()?)
        .set_created(src_meta.created()?);
    target.set_times(times)?;
    target.sync_all()?;
    if target.metadata()?.len() != src_meta.len() {
        return Err(io::Error::other("copy verification failed"));
    }
    drop(source);
    drop(target);
    fs::remove_file(src)
}

/// Moves `src` into `keep_dir` under a free name and, if `taken` is known,
/// sets the kept file's created and modified times to it.
pub fn keep_file(src: &Path, keep_dir: &Path, taken: Option<i64>) -> io::Result<KeepOutcome> {
    let original_times = get_times(src)?;
    let file_name = src
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no file name"))?
        .to_string_lossy()
        .into_owned();
    let dest = unique_dest(keep_dir, &file_name);
    move_file(src, &dest)?;
    let warning = taken.and_then(|ts| {
        let time = unix_to_system(ts);
        let times = Times {
            created: time,
            modified: time,
        };
        set_times(&dest, times)
            .err()
            .map(|e| format!("Kept, but couldn't set dates: {e}"))
    });
    Ok(KeepOutcome {
        record: KeptRecord {
            original: src.to_path_buf(),
            dest,
            original_times,
        },
        warning,
    })
}

/// Moves a kept file back to where it came from and restores its times.
/// Fails with `AlreadyExists` if something now occupies the original path.
/// Returns `Ok(Some(warning))` if the file moved back but its times didn't.
pub fn unkeep_file(rec: &KeptRecord) -> io::Result<Option<String>> {
    if rec.original.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", rec.original.display()),
        ));
    }
    if let Some(parent) = rec.original.parent() {
        fs::create_dir_all(parent)?;
    }
    move_file(&rec.dest, &rec.original)?;
    Ok(set_times(&rec.original, rec.original_times)
        .err()
        .map(|e| format!("Moved back, but couldn't restore dates: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    const TAKEN: i64 = 1552399320;

    fn secs(t: SystemTime) -> i64 {
        t.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
    }

    /// A source dir with `IMG.jpg` and an empty keep dir.
    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("takeout");
        let keep = tmp.path().join("keep");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&keep).unwrap();
        let src = src_dir.join("IMG.jpg");
        fs::write(&src, b"photo bytes").unwrap();
        (tmp, src, keep)
    }

    fn dir_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn unique_dest_free() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            unique_dest(tmp.path(), "IMG.jpg"),
            tmp.path().join("IMG.jpg")
        );
    }

    #[test]
    fn unique_dest_increments() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("IMG.jpg"), b"a").unwrap();
        fs::write(tmp.path().join("IMG (1).jpg"), b"b").unwrap();
        assert_eq!(
            unique_dest(tmp.path(), "IMG.jpg"),
            tmp.path().join("IMG (2).jpg")
        );
        fs::write(tmp.path().join("README"), b"c").unwrap();
        assert_eq!(
            unique_dest(tmp.path(), "README"),
            tmp.path().join("README (1)")
        );
    }

    #[test]
    fn keep_moves_and_sets_times() {
        let (_tmp, src, keep) = setup();
        let outcome = keep_file(&src, &keep, Some(TAKEN)).unwrap();
        let dest = keep.join("IMG.jpg");
        assert!(!src.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"photo bytes");
        assert_eq!(outcome.record.dest, dest);
        let times = get_times(&dest).unwrap();
        assert_eq!(secs(times.created), TAKEN);
        assert_eq!(secs(times.modified), TAKEN);
        assert_eq!(outcome.warning, None);
        assert_eq!(outcome.record.original, src);
    }

    #[test]
    fn keep_without_date_leaves_times() {
        let (_tmp, src, keep) = setup();
        let old = unix_to_system(1_400_000_000);
        set_times(
            &src,
            Times {
                created: old,
                modified: old,
            },
        )
        .unwrap();
        let original_modified = get_times(&src).unwrap().modified;
        let outcome = keep_file(&src, &keep, None).unwrap();
        let dest_modified = get_times(&outcome.record.dest).unwrap().modified;
        assert_eq!(secs(dest_modified), secs(original_modified));
        assert_eq!(outcome.warning, None);
    }

    #[test]
    fn keep_collision_renames() {
        let (_tmp, src, keep) = setup();
        fs::write(keep.join("IMG.jpg"), b"existing").unwrap();
        let outcome = keep_file(&src, &keep, Some(TAKEN)).unwrap();
        assert_eq!(outcome.record.dest, keep.join("IMG (1).jpg"));
        assert_eq!(fs::read(keep.join("IMG (1).jpg")).unwrap(), b"photo bytes");
        assert_eq!(fs::read(keep.join("IMG.jpg")).unwrap(), b"existing");
    }

    #[test]
    fn unkeep_restores_path_and_times() {
        let (_tmp, src, keep) = setup();
        let old = unix_to_system(1_400_000_000);
        set_times(
            &src,
            Times {
                created: old,
                modified: old,
            },
        )
        .unwrap();
        let outcome = keep_file(&src, &keep, Some(TAKEN)).unwrap();
        let rec = outcome.record;
        assert_eq!(unkeep_file(&rec).unwrap(), None);
        assert!(src.exists());
        assert!(!rec.dest.exists());
        assert_eq!(fs::read(&src).unwrap(), b"photo bytes");
        let restored = get_times(&src).unwrap();
        assert_eq!(secs(restored.created), secs(rec.original_times.created));
        assert_eq!(secs(restored.modified), secs(rec.original_times.modified));
        assert_eq!(secs(restored.modified), 1_400_000_000);
    }

    #[test]
    fn unkeep_refuses_when_original_occupied() {
        let (_tmp, src, keep) = setup();
        let rec = keep_file(&src, &keep, Some(TAKEN)).unwrap().record;
        fs::write(&src, b"newcomer").unwrap();
        let err = unkeep_file(&rec).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&src).unwrap(), b"newcomer");
        assert_eq!(fs::read(&rec.dest).unwrap(), b"photo bytes");
    }

    #[test]
    fn unkeep_recreates_missing_parent() {
        let (_tmp, src, keep) = setup();
        let rec = keep_file(&src, &keep, None).unwrap().record;
        fs::remove_dir(src.parent().unwrap()).unwrap();
        assert_eq!(unkeep_file(&rec).unwrap(), None);
        assert_eq!(fs::read(&src).unwrap(), b"photo bytes");
    }

    #[test]
    fn copy_verify_delete_moves_file() {
        let (_tmp, src, keep) = setup();
        let dest = keep.join("copy.jpg");
        copy_verify_delete(&src, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"photo bytes");
        assert!(!src.exists());
    }

    #[test]
    fn move_file_refuses_existing_dest() {
        let (_tmp, src, keep) = setup();
        let dest = keep.join("IMG.jpg");
        fs::write(&dest, b"existing").unwrap();
        let err = move_file(&src, &dest).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&src).unwrap(), b"photo bytes");
        assert_eq!(fs::read(&dest).unwrap(), b"existing");
    }

    #[test]
    fn copy_verify_delete_refuses_existing_dest() {
        let (_tmp, src, keep) = setup();
        let dest = keep.join("IMG.jpg");
        fs::write(&dest, b"existing").unwrap();
        let err = copy_verify_delete(&src, &dest).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&src).unwrap(), b"photo bytes");
        assert_eq!(fs::read(&dest).unwrap(), b"existing");
    }

    #[test]
    fn copy_verify_delete_preserves_times() {
        let (_tmp, src, keep) = setup();
        let old = unix_to_system(1_400_000_000);
        set_times(
            &src,
            Times {
                created: old,
                modified: old,
            },
        )
        .unwrap();
        let dest = keep.join("copy.jpg");
        copy_verify_delete(&src, &dest).unwrap();
        let times = get_times(&dest).unwrap();
        assert_eq!(secs(times.created), 1_400_000_000);
        assert_eq!(secs(times.modified), 1_400_000_000);
    }

    #[test]
    fn copy_verify_delete_cleans_up_when_source_cannot_be_deleted() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        let (_tmp, src, keep) = setup();
        let dest = keep.join("copy.jpg");
        // Held open without FILE_SHARE_DELETE, so deleting the source fails.
        let holder = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&src)
            .unwrap();
        // Reading still works, so the failure below comes after the copy.
        assert!(File::open(&src).is_ok());
        assert!(copy_verify_delete(&src, &dest).is_err());
        drop(holder);
        assert_eq!(fs::read(&src).unwrap(), b"photo bytes");
        assert!(!dest.exists());
    }

    #[test]
    fn keep_missing_source_errors() {
        let (_tmp, src, keep) = setup();
        let missing = src.with_file_name("nope.jpg");
        assert!(keep_file(&missing, &keep, Some(TAKEN)).is_err());
        assert!(dir_names(&keep).is_empty());
        assert!(src.exists());
    }
}
