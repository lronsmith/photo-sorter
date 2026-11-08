//! The egui application: start screen, scan progress, review and done screens.

mod review;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui;

use crate::keptlist::KeptList;
use crate::media::{self, MediaItem};
use crate::session::Session;
use crate::skiplist::SkipList;
use review::Review;

/// Windows symbol font, used as a last-resort fallback for the legend arrows.
const SYMBOL_FONT_PATH: &str = r"C:\Windows\Fonts\seguisym.ttf";

/// What a key press asks the review screen to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Keep,
    Skip,
    Prev,
    Next,
    Open,
}

/// Maps a key press to an action. Auto-repeat only navigates: holding a
/// decision key (or Space) must not keep/skip/open a run of files.
pub fn action_for_key(key: egui::Key, repeat: bool) -> Option<Action> {
    use egui::Key;
    let action = match key {
        Key::Y | Key::ArrowRight => Action::Keep,
        Key::N | Key::ArrowLeft => Action::Skip,
        Key::ArrowUp => Action::Prev,
        Key::ArrowDown => Action::Next,
        Key::Space => Action::Open,
        _ => return None,
    };
    if repeat && !matches!(action, Action::Prev | Action::Next) {
        None
    } else {
        Some(action)
    }
}

/// Why this source/keep pair can't be used, or `None` if it's fine.
/// Paths are canonicalized when they exist, so different spellings of the
/// same folder compare equal.
pub fn folder_problem(source: &Path, keep: &Path) -> Option<&'static str> {
    let source = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let keep = fs::canonicalize(keep).unwrap_or_else(|_| keep.to_path_buf());
    if source == keep {
        Some("Source and keep folders must be different")
    } else if keep.starts_with(&source) {
        Some("Keep folder can't be inside the source folder")
    } else if source.starts_with(&keep) {
        Some("Source folder can't be inside the keep folder")
    } else {
        None
    }
}

/// Adds the Windows symbol font (if present) as the lowest-priority fallback
/// in both font families, so glyphs like `←→↑↓` render even if egui's
/// bundled fonts lack them. Fonts earlier in the chain still win.
pub fn install_fallback_fonts(ctx: &egui::Context) {
    let Ok(data) = fs::read(SYMBOL_FONT_PATH) else {
        return;
    };
    let families = [egui::FontFamily::Proportional, egui::FontFamily::Monospace]
        .into_iter()
        .map(|family| egui::InsertFontFamily {
            family,
            priority: egui::FontPriority::Lowest,
        })
        .collect();
    ctx.add_font(egui::FontInsert::new(
        "segoe-ui-symbol",
        egui::FontData::from_owned(data),
        families,
    ));
}

enum ScanMsg {
    Progress(usize),
    Done(io::Result<Vec<MediaItem>>),
}

enum Screen {
    Start,
    Scanning {
        rx: Receiver<ScanMsg>,
        count: usize,
        source: PathBuf,
        keep: PathBuf,
    },
    Reviewing(Box<Review>),
}

pub struct PhotoSorterApp {
    source: Option<PathBuf>,
    keep: Option<PathBuf>,
    /// Why the last scan failed, shown on the start screen.
    start_error: Option<String>,
    screen: Screen,
}

impl Default for PhotoSorterApp {
    fn default() -> Self {
        PhotoSorterApp {
            source: None,
            keep: None,
            start_error: None,
            screen: Screen::Start,
        }
    }
}

impl eframe::App for PhotoSorterApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_scan();
        match &mut self.screen {
            Screen::Start => self.start_ui(ui),
            Screen::Scanning { count, .. } => {
                let count = *count;
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.heading(format!("Scanning… {count} files found"));
                    });
                });
                // Progress arrives from the scan thread; keep repainting.
                ui.ctx().request_repaint();
            }
            Screen::Reviewing(review) => review.ui(ui),
        }
    }
}

impl PhotoSorterApp {
    fn start_ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(12.0);
            ui.heading("Photo Sorter");
            ui.add_space(12.0);
            folder_row(
                ui,
                "Choose source folder",
                &mut self.source,
                &mut self.start_error,
            );
            folder_row(
                ui,
                "Choose keep folder",
                &mut self.keep,
                &mut self.start_error,
            );
            ui.add_space(12.0);

            let (problem, ready) = match (&self.source, &self.keep) {
                (Some(source), Some(keep)) => {
                    let problem = folder_problem(source, keep);
                    (problem, problem.is_none())
                }
                _ => (None, false),
            };
            if ui.add_enabled(ready, egui::Button::new("Start")).clicked()
                && let (Some(source), Some(keep)) = (self.source.clone(), self.keep.clone())
            {
                self.start_scan(ui.ctx(), source, keep);
            }
            if let Some(problem) = problem {
                ui.colored_label(egui::Color32::RED, problem);
            } else if !ready {
                ui.weak("Choose both folders to start");
            }
            if let Some(err) = &self.start_error {
                ui.colored_label(egui::Color32::RED, err);
            }
        });
    }

    fn start_scan(&mut self, ctx: &egui::Context, source: PathBuf, keep: PathBuf) {
        self.start_error = None;
        let (tx, rx) = mpsc::channel();
        let (thread_source, thread_keep) = (source.clone(), keep.clone());
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("scan".into())
            .spawn(move || {
                let result = media::scan(&thread_source, &thread_keep, &mut |n| {
                    let _ = tx.send(ScanMsg::Progress(n));
                });
                let _ = tx.send(ScanMsg::Done(result));
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.screen = Screen::Scanning {
                    rx,
                    count: 0,
                    source,
                    keep,
                };
            }
            Err(e) => self.start_error = Some(format!("Couldn't start scanning: {e}")),
        }
    }

    /// Drains scan progress; on completion moves to Reviewing (or back to
    /// Start with the error).
    fn poll_scan(&mut self) {
        let Screen::Scanning {
            rx,
            count,
            source,
            keep,
        } = &mut self.screen
        else {
            return;
        };
        let outcome = loop {
            match rx.try_recv() {
                Ok(ScanMsg::Progress(n)) => *count = n,
                Ok(ScanMsg::Done(result)) => break Some(result),
                Err(TryRecvError::Empty) => break None,
                Err(TryRecvError::Disconnected) => {
                    break Some(Err(io::Error::other("the scan stopped unexpectedly")));
                }
            }
        };
        match outcome {
            None => {}
            Some(Ok(items)) => {
                let skiplist = SkipList::load(source);
                let keptlist = KeptList::load(source);
                let session = Session::new(items, keep.clone(), skiplist, keptlist);
                self.screen = Screen::Reviewing(Box::new(Review::new(session)));
            }
            Some(Err(e)) => {
                self.start_error = Some(format!("Couldn't scan the source folder: {e}"));
                self.screen = Screen::Start;
            }
        }
    }
}

/// A folder-picker button with the chosen path beside it.
fn folder_row(
    ui: &mut egui::Ui,
    label: &str,
    folder: &mut Option<PathBuf>,
    start_error: &mut Option<String>,
) {
    ui.horizontal(|ui| {
        if ui.button(label).clicked()
            && let Some(path) = rfd::FileDialog::new().pick_folder()
        {
            *folder = Some(path);
            *start_error = None;
        }
        match folder {
            Some(path) => ui.label(path.display().to_string()),
            None => ui.weak("(not chosen)"),
        };
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Key;

    #[test]
    fn maps_keys() {
        assert_eq!(action_for_key(Key::Y, false), Some(Action::Keep));
        assert_eq!(action_for_key(Key::ArrowRight, false), Some(Action::Keep));
        assert_eq!(action_for_key(Key::N, false), Some(Action::Skip));
        assert_eq!(action_for_key(Key::ArrowLeft, false), Some(Action::Skip));
        assert_eq!(action_for_key(Key::ArrowUp, false), Some(Action::Prev));
        assert_eq!(action_for_key(Key::ArrowDown, false), Some(Action::Next));
        assert_eq!(action_for_key(Key::Space, false), Some(Action::Open));
        assert_eq!(action_for_key(Key::A, false), None);
    }

    #[test]
    fn repeat_ignored_for_decisions() {
        assert_eq!(action_for_key(Key::Y, true), None);
        assert_eq!(action_for_key(Key::ArrowRight, true), None);
        assert_eq!(action_for_key(Key::N, true), None);
        assert_eq!(action_for_key(Key::ArrowLeft, true), None);
        assert_eq!(action_for_key(Key::Space, true), None);
        assert_eq!(action_for_key(Key::ArrowUp, true), Some(Action::Prev));
        assert_eq!(action_for_key(Key::ArrowDown, true), Some(Action::Next));
    }

    #[test]
    fn folder_problem_rules() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let keep_inside = source.join("keep");
        let other = dir.path().join("other");
        std::fs::create_dir_all(&keep_inside).unwrap();
        std::fs::create_dir_all(&other).unwrap();

        assert_eq!(
            folder_problem(&source, &source),
            Some("Source and keep folders must be different")
        );
        // Different spellings of the same folder still count as the same.
        assert_eq!(
            folder_problem(&source, &keep_inside.join("..")),
            Some("Source and keep folders must be different")
        );
        assert_eq!(
            folder_problem(&source, &keep_inside),
            Some("Keep folder can't be inside the source folder")
        );
        assert_eq!(
            folder_problem(&keep_inside, &source),
            Some("Source folder can't be inside the keep folder")
        );
        assert_eq!(folder_problem(&source, &other), None);
        // A sibling whose name merely starts with the other's is unrelated.
        let source_two = dir.path().join("source2");
        std::fs::create_dir_all(&source_two).unwrap();
        assert_eq!(folder_problem(&source, &source_two), None);
        // Paths that don't exist are compared as given.
        let missing = dir.path().join("missing");
        assert_eq!(
            folder_problem(&missing, &missing.join("keep")),
            Some("Keep folder can't be inside the source folder")
        );
    }
}
