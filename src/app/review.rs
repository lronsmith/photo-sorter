//! The review screen (and the Done screen it ends in): key handling, the
//! preview cache, and drawing.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Vec2, pos2};

use super::{Action, action_for_key};
use crate::dates::format_taken;
use crate::media::MediaKind;
use crate::preview::PreviewLoader;
use crate::session::{FileJobDone, Message, Session, Status, Step};
use crate::shell;

const LEGEND: &str =
    "Y / →  Keep    N / ←  Skip    ↑  Previous    ↓  Next    Space  Open in default app";
const UNAVAILABLE_IMAGE: &str = "Preview unavailable — press Space to open in your photo viewer";
const UNAVAILABLE_VIDEO: &str = "Preview unavailable — press Space to open in your video player";
const EMPTY_TEXT: &str = "No photos or videos found in this folder";
/// Previews further than this from the cursor are dropped.
const KEEP_RADIUS: usize = 2;
/// Most loader requests outstanding at once. The loader can't cancel, so
/// this bounds how many decodes the current item can be queued behind.
const MAX_IN_FLIGHT: usize = 3;
const WARNING_YELLOW: Color32 = Color32::from_rgb(230, 190, 40);
const MOVING_TEXT: &str = "Moving…";
/// How often to check whether a running file move has finished.
const BUSY_POLL: Duration = Duration::from_millis(50);

/// A keep/undo file move running on a worker thread.
struct Busy {
    rx: Receiver<FileJobDone>,
    /// The item being moved and where its file was before.
    idx: usize,
    before: PathBuf,
}

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
    /// `Some` while a file move runs. The session must not change meanwhile,
    /// so every review key is ignored until it finishes.
    busy: Option<Busy>,
}

impl Review {
    pub(super) fn new(session: Session) -> Review {
        Review {
            session,
            loader: PreviewLoader::new(),
            cache: HashMap::new(),
            in_flight: HashMap::new(),
            message: Message::None,
            busy: None,
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
        // Keys collected this frame were pressed while a move was running if
        // one was running at the end of the last frame, even if it has
        // finished since: drop them too.
        let was_busy = self.busy.is_some();
        self.poll_busy();
        for action in actions_to_apply(was_busy, actions) {
            self.apply(ui.ctx(), action);
        }
        if self.busy.is_some() {
            // The worker also requests a repaint when it finishes; this is a
            // backstop in case that wake-up is missed.
            ui.ctx().request_repaint_after(BUSY_POLL);
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

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        self.message = match action {
            Action::Keep => self.decide(ctx, Session::begin_keep),
            Action::Skip => self.decide(ctx, Session::begin_skip),
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

    /// Starts a keep/skip on the current item. A decision that moves a file
    /// runs on a worker thread and finishes in [`Review::poll_busy`].
    fn decide(&mut self, ctx: &egui::Context, begin: fn(&mut Session) -> Step) -> Message {
        if self.session.is_done() {
            return Message::None;
        }
        let idx = self.session.cursor();
        let before = self.session.path_of(idx).to_path_buf();
        let job = match begin(&mut self.session) {
            Step::Finished(message) => return message,
            Step::Pending(job) => job,
        };
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("move".into())
            .spawn(move || {
                let _ = tx.send(job.run());
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.busy = Some(Busy { rx, idx, before });
                Message::None
            }
            Err(e) => Message::Error(format!("Couldn't start moving the file: {e}")),
        }
    }

    /// Finishes the running file move, if it is done. If the item's file
    /// moved, drops its cached preview so it is reloaded from the new place.
    fn poll_busy(&mut self) {
        let Some(busy) = &self.busy else {
            return;
        };
        let result = match busy.rx.try_recv() {
            Ok(done) => Ok(done),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(()),
        };
        let Some(busy) = self.busy.take() else {
            return;
        };
        self.message = match result {
            Ok(done) => self.session.finish(done),
            // The worker died without reporting (a panic). The session is
            // unchanged; the file may or may not have moved.
            Err(()) => Message::Error(
                "The file move stopped unexpectedly; check the source and keep folders".into(),
            ),
        };
        if self.session.path_of(busy.idx) != busy.before {
            self.cache.remove(&busy.idx);
        }
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

        let in_flight: HashSet<usize> = self.in_flight.keys().copied().collect();
        let cached: HashSet<usize> = self.cache.keys().copied().collect();
        for idx in indices_to_request(cursor, len, &in_flight, &cached) {
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
        if self.busy.is_some() {
            ui.label(egui::RichText::new(MOVING_TEXT).size(15.0).strong());
            ui.add_space(4.0);
            return;
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

/// Which of this frame's key actions to apply, in order. While a file move
/// runs, none: those presses are discarded, not replayed later against items
/// the user hasn't seen. Otherwise actions apply up to and including the first
/// decision (Keep/Skip) and the rest are dropped, so a frame makes at most one
/// decision and nothing is applied behind a move that has just started.
fn actions_to_apply(busy: bool, actions: Vec<Action>) -> Vec<Action> {
    if busy {
        return Vec::new();
    }
    let mut out = Vec::new();
    for action in actions {
        out.push(action);
        if matches!(action, Action::Keep | Action::Skip) {
            break;
        }
    }
    out
}

/// Which previews to request now, in order. The loader handles requests one
/// at a time in order and can't cancel them, so:
/// - at most `MAX_IN_FLIGHT` requests are ever outstanding;
/// - the cursor comes first, and nothing is requested past it while it
///   waits for a slot;
/// - neighbours (next, then previous) are only requested once no request
///   for an index outside the cursor's ±1 window (e.g. one left behind by a
///   held ↑/↓) is still outstanding.
fn indices_to_request(
    cursor: usize,
    len: usize,
    in_flight: &HashSet<usize>,
    cached: &HashSet<usize>,
) -> Vec<usize> {
    let needed = |idx: usize| idx < len && !cached.contains(&idx) && !in_flight.contains(&idx);
    let mut slots = MAX_IN_FLIGHT.saturating_sub(in_flight.len());
    let mut out = Vec::new();
    if needed(cursor) {
        if slots == 0 {
            return out;
        }
        out.push(cursor);
        slots -= 1;
    }
    if in_flight.iter().any(|idx| idx.abs_diff(cursor) > 1) {
        return out;
    }
    for idx in [cursor.checked_add(1), cursor.checked_sub(1)]
        .into_iter()
        .flatten()
    {
        if slots > 0 && needed(idx) {
            out.push(idx);
            slots -= 1;
        }
    }
    out
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
    use std::collections::VecDeque;

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

    #[test]
    fn busy_drops_every_action() {
        use Action::*;
        assert_eq!(
            actions_to_apply(true, vec![Keep, Skip, Prev, Next, Open]),
            Vec::<Action>::new()
        );
        assert_eq!(actions_to_apply(true, vec![]), Vec::<Action>::new());
    }

    #[test]
    fn at_most_one_decision_per_frame() {
        use Action::*;
        assert_eq!(actions_to_apply(false, vec![Keep, Keep, Skip]), vec![Keep]);
        assert_eq!(actions_to_apply(false, vec![Skip, Next]), vec![Skip]);
        // Navigation before the decision still applies, in order.
        assert_eq!(
            actions_to_apply(false, vec![Next, Prev, Next, Keep, Prev]),
            vec![Next, Prev, Next, Keep]
        );
        assert_eq!(actions_to_apply(false, vec![Open, Next]), vec![Open, Next]);
        assert_eq!(actions_to_apply(false, vec![]), Vec::<Action>::new());
    }

    fn set(items: &[usize]) -> HashSet<usize> {
        items.iter().copied().collect()
    }

    fn plan(cursor: usize, len: usize, in_flight: &[usize], cached: &[usize]) -> Vec<usize> {
        indices_to_request(cursor, len, &set(in_flight), &set(cached))
    }

    #[test]
    fn requests_cursor_then_next_then_previous() {
        assert_eq!(plan(5, 10, &[], &[]), vec![5, 6, 4]);
    }

    #[test]
    fn requests_stay_within_the_list() {
        assert_eq!(plan(0, 10, &[], &[]), vec![0, 1]);
        assert_eq!(plan(9, 10, &[], &[]), vec![9, 8]);
        // Done: preload the last item, which ↑ returns to.
        assert_eq!(plan(3, 3, &[], &[]), vec![2]);
        assert_eq!(plan(0, 0, &[], &[]), Vec::<usize>::new());
    }

    #[test]
    fn skips_cached_and_in_flight_indices() {
        assert_eq!(plan(5, 10, &[6], &[5]), vec![4]);
        assert_eq!(plan(5, 10, &[5, 6], &[4]), Vec::<usize>::new());
    }

    #[test]
    fn never_more_than_max_in_flight() {
        // Three stale requests: even the cursor has to wait for a slot.
        assert_eq!(plan(10, 20, &[1, 2, 3], &[]), Vec::<usize>::new());
    }

    #[test]
    fn no_neighbours_while_stale_requests_are_outstanding() {
        // The cursor jumps the queue as soon as there's a slot...
        assert_eq!(plan(10, 20, &[1, 2], &[]), vec![10]);
        // ...but neighbours wait until the stale work has drained.
        assert_eq!(plan(10, 20, &[3, 10], &[]), Vec::<usize>::new());
        // Requests for the ±1 window aren't stale.
        assert_eq!(plan(10, 20, &[9], &[]), vec![10, 11]);
    }

    /// Holding ↓ moves the cursor every frame while the loader needs several
    /// frames per decode. The loader queue must stay bounded, and once the
    /// key is released the item the user stopped on must load after at most
    /// `MAX_IN_FLIGHT` decodes.
    #[test]
    fn held_key_does_not_build_a_backlog() {
        const DECODE_FRAMES: usize = 5;
        const HELD_FRAMES: usize = 60;
        let len = 1000;
        let mut cursor = 0;
        let mut queue: VecDeque<usize> = VecDeque::new();
        let mut cached: HashSet<usize> = HashSet::new();
        let mut decodes_after_release = 0;
        let mut ahead_of_final_cursor = None;

        for frame in 0.. {
            let holding = frame < HELD_FRAMES;
            if holding {
                cursor += 1;
            }
            if frame % DECODE_FRAMES == DECODE_FRAMES - 1
                && let Some(done) = queue.pop_front()
            {
                if !holding {
                    decodes_after_release += 1;
                }
                if done.abs_diff(cursor) <= KEEP_RADIUS {
                    cached.insert(done);
                }
            }
            let in_flight: HashSet<usize> = queue.iter().copied().collect();
            for idx in indices_to_request(cursor, len, &in_flight, &cached) {
                if !holding && idx == cursor {
                    ahead_of_final_cursor = Some(queue.len());
                }
                queue.push_back(idx);
            }
            cached.retain(|idx| idx.abs_diff(cursor) <= KEEP_RADIUS);
            assert!(queue.len() <= MAX_IN_FLIGHT, "frame {frame}: {queue:?}");

            if !holding && cached.contains(&cursor) {
                break;
            }
            assert!(frame < 1000, "final item never loaded");
        }
        assert!(
            decodes_after_release <= MAX_IN_FLIGHT,
            "{decodes_after_release}"
        );
        if let Some(ahead) = ahead_of_final_cursor {
            assert!(ahead < MAX_IN_FLIGHT, "cursor queued behind {ahead}");
        }
    }
}
