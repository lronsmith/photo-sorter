# Photo Sorter

A small Windows app for culling a Google Takeout photo export. Pick the Takeout
folder and a "keep" folder, then step through every photo and video one at a
time: keep it (moved to the keep folder, with its dates fixed) or skip it (left
where it is).

## Download and run

Just `photo-sorter.exe`. Double-click it. Nothing needs to be installed (no
Rust, no installer, no Visual C++ runtime). Windows SmartScreen may warn about an
unrecognised app the first time; choose "More info" then "Run anyway".

1. **Choose source folder**: your extracted Takeout folder.
2. **Choose keep folder**: where kept files go. It must be a different folder,
   and neither folder may be inside the other.
3. **Start**.

## Keys

| Key       | Action |
|-----------|--------|
| Y or →    | Keep: move to the keep folder, set its dates, go to the next item |
| N or ←    | Skip: leave it in place, remember the decision, go to the next item |
| ↑         | Previous item |
| ↓         | Next item, without deciding |
| Space     | Open the current file in its default Windows app |

The key legend is always shown at the bottom of the window. You can change your
mind: go back with ↑ and press N on a file kept earlier in this session to move
it back to where it was (with its original dates), or Y on a skipped file to
keep it.

## What "keep" does

- The file is **moved** (not copied) into the keep folder. The keep folder is
  flat: album and year folders are not recreated. If the name is already taken,
  it becomes `name (1).jpg`, `name (2).jpg`, and so on. Nothing is ever
  overwritten.
- The file's **Created** and **Modified** dates are set to the date the photo
  was taken, read from the Takeout JSON sidecar (`photoTakenTime`) or, failing
  that, the photo's EXIF data. If neither has a date, the file's dates are left
  alone and the app shows `Taken: unknown`.
- The JSON sidecar stays in the Takeout folder; only the photo or video moves.
- Moving between drives copies the file, checks the copy's size, and only then
  removes the original. The app never deletes anything else.
- While a file is being moved (a large video going to another drive can take a
  while), the top bar shows "Moving…" and keys are ignored until it finishes.
  Keys pressed during the move are thrown away, not applied afterwards.

## Resuming

Two small text files inside the source folder remember your decisions:

- `.photo-sorter-skipped.txt` lists skipped files (one relative path per line).
- `.photo-sorter-kept.txt` lists kept photos by file name and size (one per
  line). Kept files have already left the source folder, but Takeout often has
  other copies of them (see Duplicates); this list stops those copies from
  coming back.

So when you open the same source folder again, kept photos (and their copies)
don't appear at all, and the app starts at the first file you have not decided
on yet. Skipped files are still reachable with ↑. Delete the skip-list file to
review the skipped files from the start again. Pressing N on a file you kept
earlier in the same session removes it from the kept list again.

## Duplicates

Takeout often has the same photo in several places (for example in
`Photos from 2019` and again in an album folder). Files with the same name and
the same size are shown only once. The other copies are left untouched in the
source folder. Once you keep a photo, its other copies are not shown in later
sessions either, so you won't be asked about it twice or end up with
`name (1).jpg` copies of the same photo in the keep folder.

## HEIC photos

Previews of HEIC/HEIF photos (common from iPhones) need Windows' free
**HEIF Image Extensions** from the Microsoft Store. Without it the app shows
"Preview unavailable — press Space to open in your photo viewer", and Space
still opens the file. Keeping and skipping work either way. Videos show their
first frame (where Windows can make a thumbnail); press Space to play one.

## Building from source

Requires Rust (MSVC toolchain) on Windows.

```
rustup update
cargo build --release
```

The exe is written to `target\release\photo-sorter.exe`. It is fully
self-contained (the C runtime is statically linked via `.cargo/config.toml`),
so that one file is all you need to send someone. Run the tests with
`cargo test`.

## Test fixture

`tools/make-fixture.ps1` builds a small fake Takeout for trying the app by hand.
It has coloured, labelled images with every kind of sidecar Takeout produces
(`.json`, `.supplemental-metadata.json`, `(1)` duplicates, `-edited` copies,
names cut off at 51 characters), a file with no date, an album duplicate,
non-media files that should be ignored, and a video if it can find one under
`C:\Windows`. From PowerShell:

```
powershell -ExecutionPolicy Bypass -File tools\make-fixture.ps1 -Path $env:TEMP\photo-sorter-fixture -Force
```

Choose `<Path>\Takeout` as the source and `<Path>\Keep` as the keep folder. The
script prints the date each file should get, so you can check the dates in
Explorer after keeping it.

`-Force` rebuilds the fixture from scratch. It only deletes folders the script
made itself (they contain a `.photo-sorter-fixture` marker file); if
`<Path>\Takeout` or `<Path>\Keep` exists without that marker, for example a
real Takeout export, the script stops with an error and deletes nothing.
