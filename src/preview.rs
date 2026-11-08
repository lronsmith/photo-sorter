//! Preview decoding (with EXIF orientation and downscaling) plus a background
//! loader thread so the UI never blocks on disk or the shell.

use std::collections::HashSet;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};

use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageReader};

use crate::media::MediaKind;
use crate::shell;

/// Longest edge, in pixels, of any preview returned by `load_preview`.
pub const MAX_EDGE: u32 = 2560;

/// Extensions the `image` crate decodes directly; everything else goes to the
/// Windows shell thumbnailer.
const DECODABLE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff"];

/// Size requested from the shell thumbnailer for the fallback path.
const SHELL_THUMBNAIL_SIZE: u32 = 1600;

/// Loads a display-ready preview: decoded and EXIF-oriented for common image
/// formats, otherwise (HEIC/HEIF/DNG, videos, or decode failure) the Windows
/// shell thumbnail. Never larger than `MAX_EDGE` on its long edge.
pub fn load_preview(path: &Path, kind: MediaKind) -> Option<image::RgbaImage> {
    let decoded = if kind == MediaKind::Image && is_decodable(path) {
        // `image` decoders can panic on malformed files; treat that exactly
        // like a decode error and fall back to the shell thumbnail.
        catch_panic(|| decode(path)).flatten()
    } else {
        None
    };
    let img = match decoded {
        Some(img) => img,
        None => DynamicImage::ImageRgba8(shell::shell_thumbnail(path, SHELL_THUMBNAIL_SIZE)?),
    };
    Some(downscale(img).into_rgba8())
}

fn is_decodable(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| DECODABLE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn decode(path: &Path) -> Option<DynamicImage> {
    let mut decoder = ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(orientation) = orientation {
        img.apply_orientation(orientation);
    }
    Some(img)
}

/// Runs `f`, turning a panic into `None`.
fn catch_panic<T>(f: impl FnOnce() -> T) -> Option<T> {
    // AssertUnwindSafe: on panic every value touched by `f` is discarded.
    panic::catch_unwind(AssertUnwindSafe(f)).ok()
}

fn downscale(img: DynamicImage) -> DynamicImage {
    if img.width().max(img.height()) > MAX_EDGE {
        img.resize(MAX_EDGE, MAX_EDGE, FilterType::Triangle)
    } else {
        img
    }
}

type PreviewResult = (usize, Option<image::RgbaImage>);

/// Loads previews on a single background thread. Requests are processed in
/// order; results are collected with the non-blocking `poll`. The worker exits
/// once the loader is dropped (after finishing any in-flight request).
pub struct PreviewLoader {
    requests: Sender<(usize, PathBuf, MediaKind)>,
    results: Receiver<PreviewResult>,
    pending: Mutex<HashSet<usize>>,
}

impl PreviewLoader {
    pub fn new() -> PreviewLoader {
        Self::with_loader(load_preview)
    }

    /// Builds a loader whose worker calls `load` for each request. Any panic
    /// in `load` is caught and reported as `(idx, None)`, so one bad file can
    /// never kill the worker (and with it every later preview).
    fn with_loader<F>(load: F) -> PreviewLoader
    where
        F: Fn(&Path, MediaKind) -> Option<image::RgbaImage> + Send + 'static,
    {
        let (request_tx, request_rx) = mpsc::channel::<(usize, PathBuf, MediaKind)>();
        let (result_tx, result_rx) = mpsc::channel::<PreviewResult>();
        std::thread::Builder::new()
            .name("preview-loader".into())
            .spawn(move || {
                // Keep COM initialized for the thread's lifetime so each shell
                // thumbnail call doesn't load/unload COM.
                let _com = shell::ComGuard::new();
                for (idx, path, kind) in request_rx {
                    let img = catch_panic(|| load(&path, kind)).flatten();
                    if result_tx.send((idx, img)).is_err() {
                        break;
                    }
                }
            })
            .expect("failed to spawn preview loader thread");
        PreviewLoader {
            requests: request_tx,
            results: result_rx,
            pending: Mutex::new(HashSet::new()),
        }
    }

    /// Queues a preview load for item `idx`; ignored if `idx` is already pending.
    pub fn request(&self, idx: usize, path: PathBuf, kind: MediaKind) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.insert(idx) && self.requests.send((idx, path, kind)).is_err() {
            // Worker is gone; don't leave the index stuck as pending.
            pending.remove(&idx);
        }
    }

    /// Returns every result finished since the last call, without blocking.
    pub fn poll(&self) -> Vec<(usize, Option<image::RgbaImage>)> {
        let results: Vec<PreviewResult> = self.results.try_iter().collect();
        if !results.is_empty() {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            for (idx, _) in &results {
                pending.remove(idx);
            }
        }
        results
    }
}

impl Default for PreviewLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn write_png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
        let path = dir.join(name);
        image::RgbaImage::from_pixel(w, h, image::Rgba([200, 50, 50, 255]))
            .save(&path)
            .unwrap();
        path
    }

    #[test]
    fn decodes_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "small.png", 40, 20);
        let img = load_preview(&path, MediaKind::Image).expect("decoded");
        assert_eq!(img.dimensions(), (40, 20));
    }

    #[test]
    fn downscales_large_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "large.png", 4000, 100);
        let img = load_preview(&path, MediaKind::Image).expect("decoded");
        assert_eq!(img.dimensions(), (2560, 64));
    }

    #[test]
    fn undecodable_returns_none_or_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.jpg");
        std::fs::write(&path, b"not an image").unwrap();
        // Decoding fails and the shell has no thumbnail for garbage bytes; with
        // SIIGBF_THUMBNAILONLY it no longer substitutes the file-type icon.
        assert!(load_preview(&path, MediaKind::Image).is_none());
    }

    #[test]
    fn loader_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "small.png", 40, 20);
        let loader = PreviewLoader::new();
        loader.request(3, path, MediaKind::Image);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = None;
        while Instant::now() < deadline {
            if let Some(result) = loader.poll().into_iter().next() {
                got = Some(result);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let (idx, img) = got.expect("loader produced a result within 5 s");
        assert_eq!(idx, 3);
        assert_eq!(img.expect("decoded").dimensions(), (40, 20));
    }

    /// Polls until `n` results have arrived or 5 s pass.
    fn collect(loader: &PreviewLoader, n: usize) -> Vec<(usize, Option<image::RgbaImage>)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut results = Vec::new();
        while results.len() < n && Instant::now() < deadline {
            results.extend(loader.poll());
            std::thread::sleep(Duration::from_millis(10));
        }
        results
    }

    #[test]
    fn catch_panic_turns_panic_into_none() {
        assert_eq!(catch_panic(|| 5), Some(5));
        assert_eq!(catch_panic(|| -> i32 { panic!("decoder blew up") }), None);
    }

    #[test]
    fn loader_survives_panicking_load() {
        let loader = PreviewLoader::with_loader(|path, _| {
            if path.file_name().is_some_and(|n| n == "boom.gif") {
                panic!("decoder blew up");
            }
            Some(image::RgbaImage::new(2, 2))
        });
        loader.request(0, PathBuf::from("boom.gif"), MediaKind::Image);
        loader.request(1, PathBuf::from("fine.png"), MediaKind::Image);
        let got: Vec<(usize, bool)> = collect(&loader, 2)
            .into_iter()
            .map(|(i, img)| (i, img.is_some()))
            .collect();
        assert_eq!(got, vec![(0, false), (1, true)]);

        // The worker is still alive and the panicked index is no longer stuck.
        loader.request(0, PathBuf::from("boom.gif"), MediaKind::Image);
        loader.request(2, PathBuf::from("fine.png"), MediaKind::Image);
        let got: Vec<(usize, bool)> = collect(&loader, 2)
            .into_iter()
            .map(|(i, img)| (i, img.is_some()))
            .collect();
        assert_eq!(got, vec![(0, false), (2, true)]);
    }

    #[test]
    fn loader_ignores_duplicate_pending_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "small.png", 40, 20);
        let loader = PreviewLoader::new();
        loader.request(1, path.clone(), MediaKind::Image);
        loader.request(1, path, MediaKind::Image);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut results = Vec::new();
        while results.is_empty() && Instant::now() < deadline {
            results = loader.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(200));
        results.extend(loader.poll());
        let indices: Vec<usize> = results.iter().map(|(i, _)| *i).collect();
        assert_eq!(indices, vec![1]);
    }
}
