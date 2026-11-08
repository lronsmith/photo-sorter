# Photo Sorter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A single standalone `photo-sorter.exe` for Windows that walks a Google Takeout folder and lets the user keep (move + fix dates) or skip each photo/video with the keyboard.

**Architecture:** A Rust crate with a library (`photo_sorter`) of small, pure, unit-tested modules (scan, sidecar lookup, dates, file ops, skip list, review session state machine, preview loading) and a thin `eframe`/`egui` UI on top. Windows-specific calls (shell thumbnails, open-in-default-app, file creation time) are isolated in `shell.rs` and `dates.rs`.

**Tech Stack:** Rust stable (latest via `rustup update`), eframe/egui, rfd, image, kamadak-exif, serde/serde_json, chrono, windows crate; tempfile (dev).

**Spec:** `docs/superpowers/specs/2026-11-08-photo-sorter-design.md` — read it before starting any task.

## Global Constraints

- Rust ≥ 1.75 (use the latest stable installed via `rustup update`); target `x86_64-pc-windows-msvc`.
- Output is one file: `target/release/photo-sorter.exe`. No installer, no side DLLs beyond Windows system DLLs.
- `.cargo/config.toml`: `[target.x86_64-pc-windows-msvc] rustflags = ["-C", "target-feature=+crt-static"]`.
- `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` at the top of `src/main.rs`.
- Image extensions (case-insensitive): `jpg jpeg png gif webp bmp tif tiff heic heif dng`. Video: `mp4 mov m4v avi mkv 3gp webm wmv mts`.
- Skip list file: `<source>/.photo-sorter-skipped.txt`, UTF-8, one relative path per line with `/` separators.
- Legend text (exact): `Y / →  Keep    N / ←  Skip    ↑  Previous    ↓  Next    Space  Open in default app`
- Unavailable-preview text: `Preview unavailable — press Space to open in your photo viewer` (images) / `Preview unavailable — press Space to open in your video player` (videos).
- Date display: `Taken 12 Mar 2019 14:02` (local time, format `%-d %b %Y %H:%M`) or `Taken: unknown`.
- Done text: `All done — {kept} kept, {skipped} skipped`.
- Takeout sidecar filenames are capped at 51 characters including `.json`.
- The app never deletes a file except the source of a cross-drive move after its copy's size has been verified.
- Tests live in `#[cfg(test)] mod tests` inside each module; run with `cargo test`.

## Review Focus

1. **Non-ASCII filenames** (`Café.jpg`, `写真.jpg`, emoji) — sidecar truncation must cut on char boundaries, not bytes; skip list must round-trip them. Tests: Task 3 `truncated_multibyte_name`, Task 6 `unicode_round_trip`.
2. **Holding a key down** — key auto-repeat must not keep/skip a run of files. Y/N/←/→ act only on the initial press; ↑/↓ may repeat. Test: Task 9 `repeat_ignored_for_decisions`.
3. **File vanished or locked after scan** (user deleted/opened it elsewhere) — keep returns an error message, the item stays Undecided, no panic. Test: Task 7 `keep_missing_file_errors_and_stays`.
4. **Huge images** (50 MP panoramas) — must be downscaled to ≤ 2560 px on the long edge before becoming a texture, or egui will panic/stall. Test: Task 8 `downscales_large_image`.
5. **Keep folder on a different drive** (USB stick) — rename fails with OS error 17; copy+verify+delete path must work and leave the source intact on failure. Test: Task 5 `copy_verify_delete_moves_file`.

---

### Task 1: Project scaffold and build configuration

**Files:**
- Create: `Cargo.toml`, `.cargo/config.toml`, `.gitignore`, `src/main.rs`, `src/lib.rs`

**Interfaces:**
- Produces: crate `photo-sorter` (lib name `photo_sorter`, bin name `photo-sorter`). `src/lib.rs` declares `pub mod` for each module as later tasks create them (start empty).

- [ ] **Step 1:** Confirm toolchain: `rustc --version` must print ≥ 1.75 (run `rustup update stable` if not).
- [ ] **Step 2:** `cargo init --name photo-sorter` in the repo root (keep existing `docs/`). Add deps with `cargo add eframe rfd image kamadak-exif serde --features serde/derive serde_json chrono` and `cargo add windows --features Win32_Foundation,Win32_UI_Shell,Win32_UI_Shell_Common,Win32_System_Com,Win32_Graphics_Gdi,Win32_UI_WindowsAndMessaging`, `cargo add --dev tempfile`. Latest versions are fine.
- [ ] **Step 3:** Write `.cargo/config.toml` (crt-static, see Global Constraints), `.gitignore` containing `/target`, and `src/main.rs` with the `windows_subsystem` attribute and a `fn main()` that opens an empty `eframe` window titled `Photo Sorter` (1200×800 initial size).
- [ ] **Step 4:** Verify: `cargo build` and `cargo test` succeed; `cargo build --release` produces `target/release/photo-sorter.exe`.
- [ ] **Step 5:** Commit: `chore: scaffold photo-sorter crate`.

---

### Task 2: Media scanning (`media.rs`)

**Files:**
- Create: `src/media.rs`; Modify: `src/lib.rs` (`pub mod media;`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub enum MediaKind { Image, Video }
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub struct MediaItem { pub rel_path: String, pub abs_path: PathBuf, pub kind: MediaKind, pub size: u64 }
  pub fn media_kind(path: &Path) -> Option<MediaKind>;
  pub fn scan(source: &Path, exclude: &Path, progress: &mut dyn FnMut(usize)) -> std::io::Result<Vec<MediaItem>>;
  ```
  `rel_path` uses `/` separators, relative to `source`. `progress` is called with the running count of media files found (at least every 100 files and once at the end).

- [ ] **Step 1: Write failing tests** (use `tempfile::tempdir()`; helper writes N bytes to a path, creating parent dirs):
  - `media_kind_matches_extensions`: `IMG.JPG`→Image, `a.heic`→Image, `v.MOV`→Video, `x.json`→None, `noext`→None.
  - `scan_is_recursive_and_ignores_non_media`: files `a/1.jpg`, `a/b/2.mp4`, `a/1.jpg.json`, `notes.txt` → rel_paths `["a/1.jpg", "a/b/2.mp4"]`.
  - `scan_sorts_case_insensitively`: `B/x.jpg`, `a/y.jpg`, `a/X.jpg` → `["a/X.jpg", "a/y.jpg", "B/x.jpg"]`.
  - `scan_collapses_same_name_and_size`: `Photos from 2019/IMG_1.jpg` (10 bytes) and `Album/IMG_1.jpg` (10 bytes) → one item, `Album/IMG_1.jpg` (first in sort order).
  - `scan_keeps_same_name_different_size`: same names with 10 and 11 bytes → two items.
  - `scan_excludes_keep_folder`: source contains `keep/k.jpg` and `s.jpg`, `exclude = source/keep` → `["s.jpg"]`.
- [ ] **Step 2:** `cargo test media` → FAIL (not defined).
- [ ] **Step 3:** Implement. Use `std::fs::read_dir` recursively (no extra crate). Sort by `rel_path.to_lowercase()`. Duplicate key: `(file_name.to_lowercase(), size)`, keep first after sorting. Compare `exclude` by canonicalized path prefix (skip if `exclude` doesn't exist). Unreadable subdirectories are skipped, not fatal; only an unreadable `source` returns `Err`.
- [ ] **Step 4:** `cargo test media` → PASS.
- [ ] **Step 5:** Commit: `feat: recursive media scan with duplicate collapsing`.

---

### Task 3: Takeout sidecar lookup (`sidecar.rs`)

**Files:**
- Create: `src/sidecar.rs`; Modify: `src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn sidecar_candidates(file_name: &str) -> Vec<String>; // ordered candidate JSON file names
  pub fn find_sidecar(media: &Path) -> Option<PathBuf>;     // first candidate that exists in media's dir
  pub fn read_taken_time(json: &Path) -> Option<i64>;       // photoTakenTime.timestamp, UTC seconds
  ```

Candidate order for `name = "<stem>.<ext>"` (spec "Sidecar lookup"):
1. `<name>.json`
2. `<name>.supplemental-metadata.json`
3. For each of the two above whose total length exceeds 51 chars: take everything before the final `.json`, truncate to 46 **chars**, append `.json`.
4. If stem matches `^(.*)\((\d+)\)$` → base `B`, number `N`: `B.<ext>(N).json`, `B.<ext>.supplemental-metadata(N).json`.
5. If stem ends with `-edited`: strip it and append the candidates from steps 1–4 for the original name.

Duplicates removed, order preserved.

- [ ] **Step 1: Write failing tests:**
  - `basic_candidates`: `sidecar_candidates("IMG_1234.jpg")` starts with `["IMG_1234.jpg.json", "IMG_1234.jpg.supplemental-metadata.json"]`.
  - `truncated_supplemental`: for `"abcdefghijklmnopqrstuvwxyz0123456789ABCD.jpg"` (44 chars) candidates contain `"abcdefghijklmnopqrstuvwxyz0123456789ABCD.jpg.s.json"`.
  - `truncated_plain`: for `"a".repeat(46) + ".jpg"` candidates contain `"a".repeat(46) + ".json"`.
  - `truncated_multibyte_name`: for `"é".repeat(46) + ".jpg"` candidates contain `"é".repeat(46) + ".json"` (no panic, char-based).
  - `numbered_duplicate`: `"IMG_1234(1).jpg"` → contains `"IMG_1234.jpg(1).json"` and `"IMG_1234.jpg.supplemental-metadata(1).json"`.
  - `edited_uses_original`: `"IMG_1234-edited.jpg"` → contains `"IMG_1234.jpg.json"`, and `"IMG_1234-edited.jpg.json"` comes before it.
  - `find_sidecar_prefers_first_existing`: dir with both `x.jpg.json` and `x.jpg.supplemental-metadata.json` → returns `x.jpg.json`; with only the latter → returns the latter; with none → `None`.
  - `read_taken_time_string`: `{"photoTakenTime":{"timestamp":"1552399320","formatted":"x"}}` → `Some(1552399320)`.
  - `read_taken_time_number`: `{"photoTakenTime":{"timestamp":1552399320}}` → `Some(1552399320)`.
  - `read_taken_time_missing_or_bad`: `{}` → `None`; `not json` → `None`; missing file → `None`.
- [ ] **Step 2:** `cargo test sidecar` → FAIL.
- [ ] **Step 3:** Implement with `serde_json::Value` (no regex crate; parse the `(N)` suffix by hand).
- [ ] **Step 4:** `cargo test sidecar` → PASS.
- [ ] **Step 5:** Commit: `feat: Takeout sidecar lookup and timestamp parsing`.

---

### Task 4: Date resolution, formatting and file times (`dates.rs`)

**Files:**
- Create: `src/dates.rs`; Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `sidecar::find_sidecar`, `sidecar::read_taken_time`, `media::MediaKind`.
- Produces:
  ```rust
  pub fn parse_exif_datetime(s: &str) -> Option<chrono::NaiveDateTime>; // "YYYY:MM:DD HH:MM:SS", tolerate trailing NUL/whitespace
  pub fn exif_taken(path: &Path) -> Option<i64>;           // DateTimeOriginal as local time → UTC seconds
  pub fn resolve_taken(path: &Path, kind: MediaKind) -> Option<i64>; // sidecar first, then EXIF for images
  pub fn format_taken_in<Tz: chrono::TimeZone>(ts: Option<i64>, tz: &Tz) -> String where Tz::Offset: std::fmt::Display;
  pub fn format_taken(ts: Option<i64>) -> String;          // format_taken_in(ts, &chrono::Local)
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub struct Times { pub created: SystemTime, pub modified: SystemTime }
  pub fn get_times(path: &Path) -> std::io::Result<Times>;
  pub fn set_times(path: &Path, times: Times) -> std::io::Result<()>;
  pub fn unix_to_system(ts: i64) -> SystemTime;
  ```

- [ ] **Step 1: Write failing tests:**
  - `parses_exif_datetime`: `"2019:03:12 14:02:00"` → 2019-03-12 14:02:00; `"2019:03:12 14:02:00\0"` same; `"0000:00:00 00:00:00"` → None; `"garbage"` → None.
  - `formats_known_time`: `format_taken_in(Some(1552399320), &chrono::Utc)` == `"Taken 12 Mar 2019 14:02"`.
  - `formats_unknown`: `format_taken(None)` == `"Taken: unknown"`.
  - `resolve_prefers_sidecar`: temp dir with `v.mp4` (any bytes) and `v.mp4.json` containing timestamp `"1552399320"` → `resolve_taken(.., Video) == Some(1552399320)`.
  - `resolve_none_without_metadata`: `p.jpg` with non-JPEG bytes and no sidecar → `None` (no panic).
  - `set_and_get_times_round_trip`: set created = `unix_to_system(1552399320)`, modified = same, on a temp file; `get_times` returns values whose `duration_since(UNIX_EPOCH).as_secs()` equal 1552399320.
- [ ] **Step 2:** `cargo test dates` → FAIL.
- [ ] **Step 3:** Implement. EXIF via `exif::Reader::new().read_from_container(&mut BufReader)`, field `Tag::DateTimeOriginal` (`In::PRIMARY`), any read error → `None`. Local→UTC via `chrono::Local.from_local_datetime(..).earliest()`. `set_times`: `OpenOptions::new().write(true).open(path)?` then `file.set_times(std::fs::FileTimes::new().set_modified(m).set_created(c))` with `std::os::windows::fs::FileTimesExt`. `get_times` from `metadata().created()/modified()`.
- [ ] **Step 4:** `cargo test dates` → PASS.
- [ ] **Step 5:** Commit: `feat: date-taken resolution and file time setting`.

---

### Task 5: File operations (`fileops.rs`)

**Files:**
- Create: `src/fileops.rs`; Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `dates::{Times, get_times, set_times, unix_to_system}`.
- Produces:
  ```rust
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub struct KeptRecord { pub original: PathBuf, pub dest: PathBuf, pub original_times: Times }
  pub struct KeepOutcome { pub record: KeptRecord, pub warning: Option<String> }
  pub fn unique_dest(dir: &Path, file_name: &str) -> PathBuf;
  pub fn move_file(src: &Path, dest: &Path) -> std::io::Result<()>;
  pub fn copy_verify_delete(src: &Path, dest: &Path) -> std::io::Result<()>;
  pub fn keep_file(src: &Path, keep_dir: &Path, taken: Option<i64>) -> std::io::Result<KeepOutcome>;
  pub fn unkeep_file(rec: &KeptRecord) -> std::io::Result<Option<String>>; // Ok(Some(warning)) if times couldn't be restored
  ```

- [ ] **Step 1: Write failing tests:**
  - `unique_dest_free`: empty dir → `dir/IMG.jpg`.
  - `unique_dest_increments`: dir has `IMG.jpg` and `IMG (1).jpg` → `dir/IMG (2).jpg`; no extension: `README` taken → `README (1)`.
  - `keep_moves_and_sets_times`: `keep_file(src, keep, Some(1552399320))` → src gone, `keep/<name>` exists with same bytes, created and modified secs == 1552399320, `warning == None`, `record.original == src`.
  - `keep_without_date_leaves_times`: `taken = None` → modified time of dest equals original modified (secs).
  - `keep_collision_renames`: keep dir already has `IMG.jpg` → dest is `IMG (1).jpg`, existing file untouched.
  - `unkeep_restores_path_and_times`: keep then unkeep → file back at original path, `get_times` equals `record.original_times` (secs), dest gone.
  - `unkeep_refuses_when_original_occupied`: after keep, create a new file at original path → `unkeep_file` returns `Err` with kind `AlreadyExists`; both files still exist.
  - `copy_verify_delete_moves_file`: → dest has same bytes, src gone.
  - `keep_missing_source_errors`: nonexistent src → `Err`, keep dir unchanged.
- [ ] **Step 2:** `cargo test fileops` → FAIL.
- [ ] **Step 3:** Implement. `move_file`: `fs::rename`; if it fails with `raw_os_error() == Some(17)` (ERROR_NOT_SAME_DEVICE) call `copy_verify_delete`. `copy_verify_delete`: `fs::copy`, compare `metadata().len()`; on mismatch remove dest and return `Err(io::Error::other("copy verification failed"))`; else remove src. `keep_file`: capture `get_times(src)` first, pick `unique_dest`, `move_file`, then if `taken` is Some, `set_times` — a failure there becomes `warning = Some(format!("Kept, but couldn't set dates: {e}"))`. `unkeep_file`: if `original` exists → `AlreadyExists`; create parent dirs if missing; `move_file(dest, original)`; restore times (failure → warning).
- [ ] **Step 4:** `cargo test fileops` → PASS.
- [ ] **Step 5:** Commit: `feat: keep/unkeep file operations with collision naming`.

---

### Task 6: Skip list (`skiplist.rs`)

**Files:**
- Create: `src/skiplist.rs`; Modify: `src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub const SKIP_FILE_NAME: &str = ".photo-sorter-skipped.txt";
  pub struct SkipList { /* path, ordered lines, HashSet */ }
  impl SkipList {
      pub fn load(source: &Path) -> SkipList;                    // missing/unreadable file → empty
      pub fn contains(&self, rel_path: &str) -> bool;
      pub fn add(&mut self, rel_path: &str) -> std::io::Result<()>;    // append line; no-op if present
      pub fn remove(&mut self, rel_path: &str) -> std::io::Result<()>; // rewrite file without it; no-op if absent
  }
  ```
  In-memory state is updated even if the write fails (the caller turns the `Err` into a warning).

- [ ] **Step 1: Write failing tests:**
  - `load_missing_is_empty`.
  - `add_persists`: add `a/1.jpg`, reload → contains it; file content == `"a/1.jpg\n"`.
  - `add_twice_one_line`.
  - `remove_persists`: add `a`, `b`, remove `a`, reload → contains `b` only.
  - `stale_lines_preserved`: pre-write file `"gone.jpg\nx.jpg\n"`, load, add `y.jpg` → file has all three lines.
  - `crlf_tolerated`: pre-write `"x.jpg\r\n"` → `contains("x.jpg")`.
  - `unicode_round_trip`: add `"Fotos/Café 写真 🎉.jpg"`, reload → contains it.
- [ ] **Step 2:** `cargo test skiplist` → FAIL.
- [ ] **Step 3:** Implement (trim `\r`, skip empty lines; append with `OpenOptions::append(true).create(true)`).
- [ ] **Step 4:** `cargo test skiplist` → PASS.
- [ ] **Step 5:** Commit: `feat: persistent skip list`.

---

### Task 7: Review session state machine (`session.rs`)

**Files:**
- Create: `src/session.rs`; Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `media::MediaItem`, `fileops::{KeptRecord, keep_file, unkeep_file}`, `skiplist::SkipList`, `dates::resolve_taken`.
- Produces:
  ```rust
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub enum Status { Undecided, Kept(KeptRecord), Skipped }
  #[derive(Clone, Debug, PartialEq, Eq)]
  pub enum Message { None, Error(String), Warning(String) }
  pub struct Session { /* entries: Vec<(MediaItem, Status, Option<Option<i64>>)>, cursor, keep_dir, skiplist */ }
  impl Session {
      pub fn new(items: Vec<MediaItem>, keep_dir: PathBuf, skiplist: SkipList) -> Session;
      pub fn len(&self) -> usize;
      pub fn cursor(&self) -> usize;              // == len() means Done
      pub fn is_done(&self) -> bool;
      pub fn current(&self) -> Option<&MediaItem>;
      pub fn status(&self, idx: usize) -> &Status;
      pub fn path_of(&self, idx: usize) -> &Path; // dest if Kept, else abs_path
      pub fn item(&self, idx: usize) -> &MediaItem;
      pub fn taken(&mut self, idx: usize) -> Option<i64>; // resolve_taken on the ORIGINAL abs_path, cached; never invalidated (the sidecar stays in the source dir)
      pub fn keep_current(&mut self) -> Message;
      pub fn skip_current(&mut self) -> Message;
      pub fn prev(&mut self);                     // saturating; from Done goes to len()-1
      pub fn next(&mut self);                     // up to len() (Done)
      pub fn counts(&self) -> (usize, usize);     // (kept, skipped)
  }
  ```

Rules (spec "Controls" / "Changing a decision"):
- `new`: items on the skip list get `Skipped`; cursor = first non-Skipped index (or `len()` if none).
- `keep_current`: Undecided/Skipped → `keep_file(abs_path, keep_dir, taken)`. On `Err(e)`: `Message::Error(format!("Couldn't keep file: {e}"))`, no advance. On Ok: status Kept, if it was Skipped call `skiplist.remove(rel_path)` (Err → Warning `"Couldn't update skip list: {e}"`), carry `outcome.warning` as Warning, advance. Already Kept → no-op, advance. Done → no-op.
- `skip_current`: Undecided → Skipped, `skiplist.add` (Err → Warning), advance. Kept → `unkeep_file`; Err with `AlreadyExists` → `Error("Can't undo: a file already exists at the original location")`, other Err → `Error(format!("Couldn't undo keep: {e}"))`, both no advance and stay Kept; Ok → Skipped, add to skip list, Warning if restore warned, advance. Already Skipped → no-op, advance.
- If two warnings occur in one action, join them with `"; "`.

- [ ] **Step 1: Write failing tests** (temp source + keep dirs, real files):
  - `empty_session_is_done`.
  - `resumes_at_first_unskipped`: 3 items, first two on skip list → `cursor() == 2`, statuses Skipped, Skipped, Undecided.
  - `keep_moves_and_advances`: → status Kept, file in keep dir, cursor 1, `path_of(0)` is the dest.
  - `keep_missing_file_errors_and_stays`: delete item 0's file before `keep_current` → `Message::Error(_)`, cursor 0, status Undecided.
  - `skip_records_and_advances`: → Skipped, `SkipList::load(source).contains(rel)`, cursor 1.
  - `skip_on_kept_undoes`: keep 0, `prev()`, skip → file back at original path, Skipped, on skip list.
  - `skip_on_kept_blocked_when_original_occupied`: keep 0, recreate file at original path, prev, skip → `Error(_)`, status still Kept, cursor 0.
  - `keep_on_skipped_removes_from_list`: skip 0, prev, keep → Kept, not on reloaded skip list.
  - `repeat_decisions_are_noops`: keep 0, prev, keep again → `Message::None`, cursor 1, only one file in keep dir.
  - `navigation_bounds`: `prev()` at 0 stays 0; `next()` past end reaches `len()` and stays; `prev()` from Done → `len()-1`.
  - `counts_reflect_statuses`.
- [ ] **Step 2:** `cargo test session` → FAIL.
- [ ] **Step 3:** Implement per rules above.
- [ ] **Step 4:** `cargo test session` → PASS.
- [ ] **Step 5:** Commit: `feat: review session state machine`.

---

### Task 8: Previews and Windows shell integration (`shell.rs`, `preview.rs`)

**Files:**
- Create: `src/shell.rs`, `src/preview.rs`; Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `media::MediaKind`.
- Produces:
  ```rust
  // shell.rs
  pub fn open_default(path: &Path) -> std::io::Result<()>;                 // ShellExecuteW "open", SW_SHOWNORMAL; result <= 32 → Err
  pub fn shell_thumbnail(path: &Path, size: u32) -> Option<image::RgbaImage>; // IShellItemImageFactory::GetImage
  // preview.rs
  pub const MAX_EDGE: u32 = 2560;
  pub fn load_preview(path: &Path, kind: MediaKind) -> Option<image::RgbaImage>;
  pub struct PreviewLoader { /* worker thread, request + result channels */ }
  impl PreviewLoader {
      pub fn new() -> PreviewLoader;
      pub fn request(&self, idx: usize, path: PathBuf, kind: MediaKind); // dedup: ignore if idx already pending
      pub fn poll(&self) -> Vec<(usize, Option<image::RgbaImage>)>;        // non-blocking
  }
  ```

- [ ] **Step 1: Write failing tests:**
  - `decodes_png` (preview): write a 40×20 PNG with `image` → `load_preview(.., Image)` is Some with dims 40×20.
  - `downscales_large_image`: 4000×100 PNG → result width 2560, height 64.
  - `undecodable_returns_none_or_thumbnail`: file `bad.jpg` containing `b"not an image"` → no panic (result may be None).
  - `shell_thumbnail_of_png` (shell): 200×100 PNG → `shell_thumbnail(path, 256)` is Some with width ≤ 256 and > 0.
  - `loader_round_trip`: `PreviewLoader::new()`, request idx 3 for the 40×20 PNG, poll in a loop (≤ 5 s, 10 ms sleeps) → receives `(3, Some(img))`.
- [ ] **Step 2:** `cargo test preview shell` → FAIL.
- [ ] **Step 3:** Implement.
  - `load_preview`: for Image kinds whose extension is one of `jpg jpeg png gif webp bmp tif tiff`, decode with `image::ImageReader::open(..)?.with_guessed_format()`, read EXIF orientation via the decoder (`ImageDecoder::orientation()`) and `DynamicImage::apply_orientation`; downscale with `resize` (Triangle filter) if the long edge > `MAX_EDGE`. If decoding fails, or for heic/heif/dng and all videos, fall back to `shell_thumbnail(path, 1600)`.
  - `shell_thumbnail`: `CoInitializeEx(None, COINIT_APARTMENTTHREADED)` (ignore "already initialized"), `SHCreateItemFromParsingName::<IShellItemImageFactory>`, `GetImage(SIZE{cx,cy}, SIIGBF_RESIZETOFIT | SIIGBF_BIGGERSIZEOK)`, read the HBITMAP with `GetObjectW` + `GetDIBits` (32-bpp, top-down via negative height), BGRA→RGBA, `DeleteObject`. Any failure → None.
  - `PreviewLoader`: one `std::thread` with `mpsc` channels; the worker thread calls `load_preview`.
- [ ] **Step 4:** `cargo test preview shell` → PASS.
- [ ] **Step 5:** Commit: `feat: image previews with Windows shell thumbnail fallback`.

---

### Task 9: egui application (`app.rs`, `main.rs`)

**Files:**
- Create: `src/app.rs`; Modify: `src/main.rs`, `src/lib.rs`

**Interfaces:**
- Consumes: everything above (`media::scan`, `SkipList::load`, `Session`, `PreviewLoader`, `shell::open_default`, `dates::format_taken`).
- Produces:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub enum Action { Keep, Skip, Prev, Next, Open }
  pub fn action_for_key(key: egui::Key, repeat: bool) -> Option<Action>;
  pub struct PhotoSorterApp { /* state: Start{source,keep} | Scanning{rx, count} | Reviewing{session, ...} */ }
  impl eframe::App for PhotoSorterApp { fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame); }
  pub fn folder_problem(source: &Path, keep: &Path) -> Option<&'static str>;
  ```

- [ ] **Step 1: Write failing tests:**
  - `maps_keys`: Y/ArrowRight→Keep, N/ArrowLeft→Skip, ArrowUp→Prev, ArrowDown→Next, Space→Open, A→None.
  - `repeat_ignored_for_decisions`: `action_for_key(Key::Y, true)` / `ArrowRight,true` / `N,true` / `ArrowLeft,true` / `Space,true` → None; `ArrowUp,true`→Prev; `ArrowDown,true`→Next.
  - `folder_problem_rules`: same path → `Some("Source and keep folders must be different")`; keep inside source → `Some("Keep folder can't be inside the source folder")`; source inside keep → `Some("Source folder can't be inside the keep folder")`; unrelated → None. (Compare canonicalized paths when they exist.)
- [ ] **Step 2:** `cargo test app` → FAIL.
- [ ] **Step 3:** Implement the UI per spec "Screens":
  - **Start:** two buttons using `rfd::FileDialog::new().pick_folder()`, paths shown beside them, Start disabled with the `folder_problem` text shown when set.
  - **Scanning:** spawn a thread running `media::scan(source, keep, ..)` sending progress counts and the final result over `mpsc`; show `Scanning… {n} files found`; call `ctx.request_repaint()` while scanning. A scan `Err` shows the message on the Start screen.
  - **Reviewing:** read keys from `ctx.input(|i| i.events)` — `egui::Event::Key { key, pressed: true, repeat, .. }` → `action_for_key`. Top panel: `{i+1} / {len} · {rel_path} · {format_taken(taken)} · [{Undecided|Kept|Skipped}]`, plus the current `Message` (red for Error, yellow for Warning; cleared on the next action). Bottom panel: the legend (exact text). Central panel: texture scaled to fit preserving aspect ratio; videos get a painter-drawn translucent circle with a triangle "play" overlay; while loading show `Loading…`; if the loader returned None show the unavailable text (image/video variant) and the filename.
  - **Preview cache:** `HashMap<usize, Option<egui::TextureHandle>>`; on each cursor change request `cursor-1, cursor, cursor+1` (using `session.path_of(idx)`), drop entries farther than 2 from the cursor. Invalidate the entry for an index after a keep/undo (its path changed — re-request).
  - **Done:** `All done — {kept} kept, {skipped} skipped` centred (if the session was empty, `No photos or videos found in this folder`); ↑ returns to the last item; legend still shown.
  - **Fonts:** if `←→↑↓` don't render with egui's default fonts, load `C:\Windows\Fonts\seguisym.ttf` at startup (if present) as a fallback font in both families.
  - `main.rs`: `eframe::run_native("Photo Sorter", options, Box::new(|_| Ok(Box::new(PhotoSorterApp::default()))))` (match the installed eframe's signature).
- [ ] **Step 4:** `cargo test` (all) → PASS; `cargo build --release` succeeds with no warnings that indicate dead code paths for required features.
- [ ] **Step 5:** Commit: `feat: egui photo sorter UI`.

---

### Task 10: Release verification and manual test fixture

**Files:**
- Create: `tools/make-fixture.ps1`, `README.md`

**Interfaces:**
- Consumes: the built exe.

- [ ] **Step 1:** `cargo build --release`; check imports with `dumpbin /dependents target\release\photo-sorter.exe` (from a VS developer shell, if available) or by inspecting with any PE tool — the list must contain only Windows system DLLs (no `VCRUNTIME140.dll`, no `MSVCP140.dll`). Record the exe size.
- [ ] **Step 2:** Write `tools/make-fixture.ps1 -Path <dir>` that creates a fake Takeout: `Takeout/Google Photos/Photos from 2019/` with a few generated JPG/PNG files (use `System.Drawing` to draw colored rectangles with text), a `.json` sidecar for some (one `.json`, one `.supplemental-metadata.json`, one `(1)` numbered, one `-edited` copy), one file with no metadata, an album folder containing a byte-identical copy of one photo, a long-name file with a truncated sidecar, and a copied `.mp4` if one exists at `C:\Windows\` media paths (skip otherwise).
- [ ] **Step 3:** Run the script into the scratch dir and launch the exe once (`Start-Process`) to make sure it starts without a console window and without crashing in the first 3 seconds; close it.
- [ ] **Step 4:** `README.md`: what it does, keys, how to build (`rustup update`, `cargo build --release`), where the exe lands, and the HEIC note.
- [ ] **Step 5:** Commit: `chore: release check, fixture script, README`.
