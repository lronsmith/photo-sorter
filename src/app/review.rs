//! The review screen (and the Done screen it ends in): key handling, the
//! preview cache, and drawing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Vec2, pos2};

use super::{Action, action_for_key};
use crate::dates::format_taken;
use crate::media::MediaKind;
use crate::preview::PreviewLoader;
use crate::session::{Message, Session, Status};
use crate::shell;

const LEGEND: &str =
    "Y / →  Keep    N / ←  Skip    ↑  Previous    ↓  Next    Space  Open in default app";
const UNAVAILABLE_IMAGE: &str = "Preview unavailable — press Space to open in your photo viewer";
const UNAVAILABLE_VIDEO: &str = "Preview unavailable — press Space to open in your video player";
const EMPTY_TEXT: &str = "No photos or videos found in this folder";
/// Previews further than this from the cursor are dropped.
const KEEP_RADIUS: usize = 2;
const WARNING_YELLOW: Color32 = Color32::from_rgb(230, 190, 40);

pub(super) struct Review {
    session: Session,
    loader: PreviewLoader,
    /// Loaded previews by index; `None` means no preview could be produced.
    cache: HashMap<usize, Option<egui::TextureHandle>>,
    /// Requests sent to the loader and not yet answered, with the path each
    /// was made for. A result whose path no longer matches `path_of(idx)`
    /// (the file was kept/unkept meanwhile) is stale and dropped.
    in_flight: HashMap<usize, PathBuf>,
    message: Message,
}

impl Review {
    pub(super) fn new(session: Session) -> Review {
        Review {
            session,
            loader: PreviewLoader::new(),
            cache: HashMap::new(),
            in_flight: HashMap::new(),
            message: Message::None,
        }
    }

    pub(super) fn ui(&mut self, ui: &mut egui::Ui) {
        let actions: Vec<Action> = ui.ctx().input(|i| {
            i.events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        repeat,
                        ..
                    } => action_for_key(*key, *repeat),
                    _ => None,
                })
                .collect()
        });
        for action in actions {
            self.apply(action);
        }
        self.update_previews(ui.ctx());

        egui::Panel::top("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::bottom("legend").show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(LEGEND).size(15.0));
                ui.add_space(4.0);
            });
        });
        egui::CentralPanel::default().show(ui, |ui| self.centre(ui));
    }

    fn apply(&mut self, action: Action) {
        self.message = match action {
            Action::Keep => self.decide(Session::keep_current),
            Action::Skip => self.decide(Session::skip_current),
            Action::Prev => {
                self.session.prev();
                Message::None
            }
            Action::Next => {
                self.session.next();
                Message::None
            }
            Action::Open => {
                if self.session.is_done() {
                    Message::None
                } else {
                    let path = self.session.path_of(self.session.cursor());
                    match shell::open_default(path) {
                        Ok(()) => Message::None,
                        Err(e) => Message::Error(format!("Couldn't open file: {e}")),
                    }
                }
            }
        };
    }

    /// Runs a keep/skip on the current item; if that moved its file, drops
    /// its cached preview so it is reloaded from the new location.
    fn decide(&mut self, op: fn(&mut Session) -> Message) -> Message {
        if self.session.is_done() {
            return Message::None;
        }
        let idx = self.session.cursor();
        let before = self.session.path_of(idx).to_path_buf();
        let message = op(&mut self.session);
        if self.session.path_of(idx) != before {
            self.cache.remove(&idx);
        }
        message
    }

    /// Collects finished previews, requests the ones around the cursor, and
    /// drops those far from it.
    fn update_previews(&mut self, ctx: &egui::Context) {
        let cursor = self.session.cursor();
        let len = self.session.len();
        for (idx, image) in self.loader.poll() {
            let requested = self.in_flight.remove(&idx);
            let wanted = idx < len && idx.abs_diff(cursor) <= KEEP_RADIUS;
            if wanted && requested.as_deref() == Some(self.session.path_of(idx)) {
                let texture = image.map(|img| to_texture(ctx, idx, img));
                self.cache.insert(idx, texture);
            }
        }

        // Current first: the loader works through requests in order.
        let window = [Some(cursor), cursor.checked_add(1), cursor.checked_sub(1)];
        for idx in window.into_iter().flatten().filter(|&i| i < len) {
            if self.cache.contains_key(&idx) || self.in_flight.contains_key(&idx) {
                continue;
            }
            let path = self.session.path_of(idx).to_path_buf();
            let kind = self.session.item(idx).kind;
            self.loader.request(idx, path.clone(), kind);
            self.in_flight.insert(idx, path);
        }

        self.cache
            .retain(|&idx, _| idx.abs_diff(cursor) <= KEEP_RADIUS);

        // Results arrive from the loader thread without waking egui.
        if !self.in_flight.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        if !self.session.is_done() {
            let idx = self.session.cursor();
            let taken = format_taken(self.session.taken(idx));
            let status = match self.session.status(idx) {
                Status::Undecided => "Undecided",
                Status::Kept(_) => "Kept",
                Status::Skipped => "Skipped",
            };
            let line = format!(
                "{} / {} · {} · {} · [{}]",
                idx + 1,
                self.session.len(),
                self.session.item(idx).rel_path,
                taken,
                status
            );
            ui.label(egui::RichText::new(line).size(15.0));
        }
        match &self.message {
            Message::None => {}
            Message::Error(text) => {
                ui.colored_label(Color32::RED, text);
            }
            Message::Warning(text) => {
                ui.colored_label(WARNING_YELLOW, text);
            }
        }
        ui.add_space(4.0);
    }

    fn centre(&self, ui: &mut egui::Ui) {
        if self.session.is_done() {
            let text = if self.session.len() == 0 {
                EMPTY_TEXT.to_string()
            } else {
                let (kept, skipped) = self.session.counts();
                format!("All done — {kept} kept, {skipped} skipped")
            };
            ui.centered_and_justified(|ui| ui.heading(text));
            return;
        }
        let idx = self.session.cursor();
        let item = self.session.item(idx);
        match self.cache.get(&idx) {
            None => {
                ui.centered_and_justified(|ui| ui.heading("Loading…"));
            }
            Some(None) => {
                let message = match item.kind {
                    MediaKind::Image => UNAVAILABLE_IMAGE,
                    MediaKind::Video => UNAVAILABLE_VIDEO,
                };
                let name = self
                    .session
                    .path_of(idx)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.centered_and_justified(|ui| {
                    ui.heading(format!("{message}\n\n{name}"));
                });
            }
            Some(Some(texture)) => {
                let (area, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
                let rect = fit_rect(texture.size_vec2(), area);
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                let painter = ui.painter_at(area);
                painter.image(texture.id(), rect, uv, Color32::WHITE);
                if item.kind == MediaKind::Video {
                    paint_play_overlay(&painter, rect);
                }
            }
        }
    }
}

fn to_texture(ctx: &egui::Context, idx: usize, img: image::RgbaImage) -> egui::TextureHandle {
    // Previews are capped at MAX_EDGE, but respect a smaller GPU limit too.
    let max_side = ctx.input(|i| i.max_texture_side) as u32;
    let img = if img.width().max(img.height()) > max_side {
        let (w, h) = (img.width() as f32, img.height() as f32);
        let scale = max_side as f32 / w.max(h);
        let (nw, nh) = ((w * scale) as u32, (h * scale) as u32);
        image::imageops::resize(
            &img,
            nw.max(1),
            nh.max(1),
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    ctx.load_texture(
        format!("preview-{idx}"),
        color,
        egui::TextureOptions::LINEAR,
    )
}

/// The largest rect with `size`'s aspect ratio that fits in `area`, centred.
fn fit_rect(size: Vec2, area: Rect) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return Rect::from_center_size(area.center(), Vec2::ZERO);
    }
    let scale = (area.width() / size.x).min(area.height() / size.y);
    Rect::from_center_size(area.center(), size * scale)
}

/// A translucent circle with a white "play" triangle, centred on `rect`.
fn paint_play_overlay(painter: &egui::Painter, rect: Rect) {
    let c = rect.center();
    let r = (rect.width().min(rect.height()) * 0.12).clamp(24.0, 60.0);
    painter.circle_filled(c, r, Color32::from_black_alpha(140));
    let points: Vec<Pos2> = vec![
        pos2(c.x - r * 0.35, c.y - r * 0.5),
        pos2(c.x + r * 0.55, c.y),
        pos2(c.x - r * 0.35, c.y + r * 0.5),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        Color32::from_white_alpha(230),
        egui::Stroke::NONE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_rect_preserves_aspect_and_centres() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(200.0, 100.0));
        let wide = fit_rect(Vec2::new(400.0, 100.0), area);
        assert_eq!(wide.size(), Vec2::new(200.0, 50.0));
        assert_eq!(wide.center(), area.center());
        let tall = fit_rect(Vec2::new(50.0, 100.0), area);
        assert_eq!(tall.size(), Vec2::new(50.0, 100.0));
        assert_eq!(tall.center(), area.center());
    }
}
