# Photo Sorter — Design

Date: 2026-11-08
Status: Approved in conversation, pending written-spec review

## Purpose

A standalone Windows desktop app for culling a Google Takeout photo export.
The user picks a source folder (the Takeout) and a "keep" folder, then steps
through every photo and video one at a time, deciding to keep (move to the keep
folder, with dates fixed) or skip (leave in place).

The deliverable is a single `photo-sorter.exe` the author can send to someone
else, who runs it by double-clicking — no Rust, installer, or runtime needed.

## Success criteria

- One `.exe`, runs on a stock Windows 10/11 machine with no extra installs.
- Finds every supported media file under the source folder, recursively.
- Y/→ keeps, N/← skips, ↑ previous, ↓ next, Space opens in default app; the
  legend for these is always visible at the bottom of the window.
- Kept files land flat in the keep folder with Created and Modified set to the
  date taken (Takeout JSON first, EXIF fallback).
- Skip decisions persist so a second session resumes where the first left off.
- No file is ever lost or deleted except a cross-drive move's original after its
  copy is verified.

## Technology

- Rust (toolchain updated via `rustup update`; ≥ 1.95, the minimum for
  egui/eframe 0.37; the crate's own code needs ≥ 1.88 for let-chains and
  `File::set_times` / `FileTimesExt::set_created`).
- GUI: `eframe` / `egui`.
- Folder pickers: `rfd` (native Windows dialogs).
- Image decoding: `image` crate (jpg, jpeg, png, gif, webp, bmp, tif, tiff),
  with EXIF orientation applied.
- Fallback previews (videos' first frame, HEIC/HEIF, DNG, anything `image`
  can't decode): Windows Shell thumbnails via `IShellItemImageFactory` from the
  `windows` crate, requested at a large size (~1600 px).
- EXIF: `kamadak-exif`.
- JSON: `serde` / `serde_json`.
- Open in default app: `ShellExecuteW` "open" (via `windows` crate) or
  equivalent.

### Build configuration

- `#![windows_subsystem = "windows"]` in release so no console window appears.
- `.cargo/config.toml` sets `-C target-feature=+crt-static` for the MSVC target
  so the exe does not depend on the Visual C++ Redistributable.
- `cargo build --release` → `target/release/photo-sorter.exe`.

## Screens

### Start screen

- Button "Choose source folder" + displayed path.
- Button "Choose keep folder" + displayed path.
- "Start" button, disabled until both are set, they differ, and neither is
  inside the other. An inline message explains why Start is disabled.

### Scan

- Recursive walk of the source folder, skipping the keep folder if it somehow
  appears beneath it.
- Supported extensions (case-insensitive):
  - Images: jpg, jpeg, png, gif, webp, bmp, tif, tiff, heic, heif, dng
  - Videos: mp4, mov, m4v, avi, mkv, 3gp, webm, wmv, mts
- Everything else (including `.json`) is ignored.
- Sort by relative path (folder, then filename), case-insensitive, so order is
  stable across sessions.
- Duplicate collapsing: files with the same filename (case-insensitive) and the
  same byte size are treated as one item; only the first in sort order is shown.
  Other copies are left untouched in the source.
- The resume position is the first item that is not on the skip list. Items on
  the skip list remain in the list (reachable with ↑) with status Skipped.
- A "Scanning… N files found" message is shown while the walk runs (on a
  background thread).

### Main screen

- Top bar: `<index> / <total> · <relative path> · Taken <date> · [status]`
  - Date formatted like `12 Mar 2019 14:02` in local time, or `Taken: unknown`.
  - Status is one of Undecided / Kept / Skipped.
  - Error (red) or warning (yellow) messages appear here too and clear on the
    next navigation.
- Centre: the media scaled to fit the window, aspect ratio preserved.
  - Videos: first-frame thumbnail with a ▶ overlay.
  - If no preview can be produced: text "Preview unavailable — press Space to
    open in your photo viewer" (wording adapts to "video player" for videos),
    plus the filename.
- Bottom legend, always visible:
  `Y / →  Keep    N / ←  Skip    ↑  Previous    ↓  Next    Space  Open in default app`

### Done screen

- Shown after deciding/moving past the last item:
  `All done — <kept> kept, <skipped> skipped`. ↑ returns to the last item.

## Controls

| Key       | Action |
|-----------|--------|
| Y / →     | Keep: move to keep folder, set dates, advance |
| N / ←     | Skip: record on skip list, leave in place, advance |
| ↑         | Previous item |
| ↓         | Next item, no decision |
| Space     | Open current file (at its current location) in default Windows app |

### Changing a decision

The current file is always displayed and opened from where it currently lives.

- N on an item kept earlier this session: move it back to its original path,
  restore its original Created/Modified times (captured before the keep), mark
  Skipped and add to the skip list. If the original path is now occupied, show
  an error and leave it kept.
- Y on a Skipped item: keep it as normal and remove it from the skip list.
- Y on an already Kept item, or N on an already Skipped item: no-op, advance.

Files kept in previous sessions are no longer in the source folder and do not
appear in the list.

## File operations

### Keep (move)

1. Resolve date taken (see below) — already resolved for display.
2. Choose destination `keep/<filename>`. If taken, try `keep/<stem> (1).<ext>`,
   `(2)`, … until free.
3. `std::fs::rename`. If that fails because source and destination are on
   different volumes, copy → verify destination size equals source size →
   delete source. If verification fails, delete the partial copy and report an
   error; the source is untouched.
4. Set Created and Modified on the destination to the date taken, if known.
5. Record the original path, destination path, and original times in session
   state for undo.

Failure in steps 2–3: red error, item stays Undecided, no advance.
Failure in step 4: file counts as Kept, yellow warning, advance.

The Takeout JSON sidecar is never moved.

### Date taken resolution

1. Takeout JSON sidecar → `photoTakenTime.timestamp` (UTC seconds, stored as a
   string in the JSON).
2. EXIF `DateTimeOriginal` (images only), interpreted as local time.
3. Otherwise unknown; file times are left unchanged.

Sidecar lookup for media file `<name>` (= `<stem>.<ext>`) in directory `D`,
trying in order and using the first that exists:

1. `D/<name>.json`
2. `D/<name>.supplemental-metadata.json`
3. Truncated forms: Takeout caps sidecar filenames at 51 characters including
   `.json`. For each of the two suffixes `.json` and
   `.supplemental-metadata.json`, compute `<name><suffix>`; if longer than 51
   characters, truncate the part before `.json` so the whole is 51 characters
   and look for that.
4. Numbered duplicates: if `<stem>` ends in `(N)`, i.e. `<base>(N).<ext>`, look
   for `<base>.<ext>(N).json` and `<base>.<ext>.supplemental-metadata(N).json`.
5. Edited copies: if `<stem>` ends in `-edited` (English only), strip it and
   repeat steps 1–4 with the original name.

Each rule has a unit test.

### Skip list

- File: `<source>/.photo-sorter-skipped.txt`, UTF-8, one relative path (using
  `/` separators) per line.
- N appends the line (file created if missing).
- Keeping a skipped item rewrites the file without that line.
- Loaded once at startup; unknown/stale lines are kept but ignored.
- Failure to write: yellow warning; the in-memory decision still stands.

## Background loading

- Image decoding and shell thumbnail extraction happen on a worker thread.
- The current item plus the next and previous items are preloaded; decoded
  textures for items far from the cursor are dropped to bound memory.
- While the current item's preview is loading, show "Loading…".

## Architecture (units)

- `media` — supported extensions, `MediaKind` (Image/Video), scan + sort +
  duplicate collapsing. Pure, testable on a temp dir.
- `sidecar` — Takeout JSON lookup and `photoTakenTime` parsing. Pure.
- `exif_date` — EXIF `DateTimeOriginal` reading.
- `dates` — combines sidecar + EXIF into `Option<DateTime>`; sets file times.
- `fileops` — collision-free destination naming, move with cross-drive
  fallback, undo move.
- `skiplist` — load/append/remove.
- `preview` — decode with `image`, fallback to Windows Shell thumbnail; worker
  thread + cache.
- `shell` — Windows-specific calls (thumbnail, open in default app).
- `app` — egui state machine (Start → Scanning → Reviewing → Done), key
  handling, rendering.
- `main` — window setup.

## Testing

- Unit tests: sidecar matching (each rule), JSON timestamp parsing, EXIF date
  parsing, collision naming, skip list load/append/remove, duplicate collapsing,
  move + undo on a temp dir.
- Manual: a small fake Takeout folder with nested folders, long filenames,
  `(1)` duplicates, `-edited` files, an album duplicate, a video, a HEIC, and
  a file with no metadata. Verify keys, legend, dates in Explorer, resume, undo.

## Out of scope

- In-app video playback.
- Moving or rewriting JSON sidecars.
- Preserving folder structure in the keep folder.
- Deleting files.
- Localised `-edited` suffixes (e.g. `-bearbeitet`).
- Remembering last-used folders between runs.
