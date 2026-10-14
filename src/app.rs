// fastlaunch - a fast game launcher front end
// Copyright (C) 2026 marcin
// SPDX-License-Identifier: GPL-3.0-only

//! The launcher screen: the focused game's key art fills the window, its logo
//! (or title) and a big Play button sit on the left, and the library runs
//! along the bottom as a row of covers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontFamily, FontId, Key, KeyboardShortcut, Modifiers,
    Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, TextStyle, TextureHandle, Vec2,
    epaint::Mesh, epaint::RectShape, epaint::Shadow, pos2, text::LayoutJob, text::TextFormat,
    vec2,
};

use crate::{ArtMessage, HeroRequest};
use crate::epic::{self, Epic};
use crate::library::{ArtKind, Game, Source};
use crate::steam;

const BG: Color32 = Color32::from_rgb(0x07, 0x08, 0x0C);
/// Dialogs.
const SURFACE: Color32 = Color32::from_rgb(0x13, 0x15, 0x1C);
/// Buttons and text fields in dialogs.
const CONTROL: Color32 = Color32::from_rgb(0x22, 0x25, 0x2E);
const CONTROL_HOVER: Color32 = Color32::from_rgb(0x2E, 0x32, 0x3D);
/// Translucent panels over the artwork.
const GLASS: Color32 = Color32::from_rgba_premultiplied(9, 10, 15, 170);
const GLASS_HOVER: Color32 = Color32::from_rgba_premultiplied(26, 28, 36, 190);
const GLASS_EDGE: Color32 = Color32::from_rgba_premultiplied(30, 30, 30, 30);
const ACCENT: Color32 = Color32::from_rgb(0xC6, 0xFF, 0x3D);
const TEXT: Color32 = Color32::from_rgb(0xF4, 0xF5, 0xF7);
const TEXT_WEAK: Color32 = Color32::from_rgb(0xA3, 0xA8, 0xB3);
/// Backdrop glow when a game's art has no clear colour.
const DEFAULT_TINT: Color32 = Color32::from_rgb(0x4B, 0x5B, 0xFF);

/// The layout is designed at this window size and scaled from there.
const DESIGN_SIZE: Vec2 = vec2(1280.0, 760.0);
const TILE_RADIUS: f32 = 12.0;
const BACKDROP_FADE: f64 = 0.45;
const HERO_FADE: f64 = 0.28;
const COUNT_UP: f64 = 0.8;

const FULL_UV: Rect = Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0));

const FOCUS_SEARCH: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Steam,
    Epic,
}

enum Status {
    Launching(String),
    Error(String),
}

/// What the background loader hands over once the library is read.
pub struct Loaded {
    pub games: Vec<Game>,
    pub epic_account: Option<epic::Account>,
}

/// Results from worker threads.
enum Event {
    Status(Status),
    SignedIn(Result<String, String>),
    SignedOut,
    Steam(steam::State),
}

#[derive(Default)]
struct SignInDialog {
    code: String,
    busy: bool,
    error: Option<String>,
}

enum DialogAction {
    OpenPage,
    Submit,
    Close,
}

/// At most this many hero textures stay in GPU memory (about 8 MB each); the
/// least recently focused one is dropped first. Covers and logos are small and
/// stay for the app's lifetime.
const HERO_CACHE: usize = 5;

/// GPU textures for one game. Covers and logos are uploaded once; hero art is
/// loaded on demand and evicted (see [`HERO_CACHE`]).
struct GameArt {
    cover: Option<TextureHandle>,
    hero: Option<TextureHandle>,
    /// Where the hero art is on disk, if anywhere.
    hero_path: Option<PathBuf>,
    /// A decode request is out for the hero.
    hero_pending: bool,
    /// Last time this game was focused, for hero eviction.
    hero_used: f64,
    logo: Option<TextureHandle>,
    /// The part of the logo that is not transparent padding.
    logo_uv: Rect,
    /// When the hero art arrived, so it fades in instead of popping.
    hero_at: f64,
    /// The art's dominant colour, for the backdrop glow.
    tint: Option<Color32>,
}

impl Default for GameArt {
    fn default() -> Self {
        Self {
            cover: None,
            hero: None,
            hero_path: None,
            hero_pending: false,
            hero_used: 0.0,
            logo: None,
            logo_uv: FULL_UV,
            hero_at: 0.0,
            tint: None,
        }
    }
}

/// What the user did this frame, applied after drawing.
#[derive(Default)]
struct Actions {
    launch: Option<usize>,
    focus: Option<usize>,
    refilter: bool,
    open_sign_in: bool,
    sign_out: bool,
}

pub struct FastLaunch {
    start: Instant,
    ready_ms: Option<u128>,
    /// `ui.input().time` when the library was ready, for the count-up.
    ready_at: f64,
    lib_rx: Receiver<Loaded>,
    art_rx: Receiver<ArtMessage>,
    /// Hero decode requests; `None` in tests, where art is fed directly.
    hero_tx: Option<Sender<HeroRequest>>,
    games: Option<Vec<Game>>,
    /// Game names in capitals, for the display title.
    titles: Vec<String>,
    /// Same order as `games`.
    art: Vec<GameArt>,
    index_of: HashMap<String, usize>,
    /// Art that arrived before the library itself.
    early_art: Vec<ArtMessage>,
    counts: [usize; 3],
    filter: Filter,
    search: String,
    /// Indices into `games` that pass the filter and search. Rebuilt on change.
    visible: Vec<usize>,
    /// The game shown big, an index into `games`.
    focus: Option<usize>,
    /// Backdrop crossfade: the game fading out, and when the fade started.
    backdrop_prev: Option<usize>,
    focus_at: f64,
    /// Horizontal scroll of the cover row, in points at scale 1.
    scroll: f32,
    scroll_to_focus: bool,
    launched_at: f64,
    status: Option<Status>,
    /// Keep asking for search focus until the search box actually has it.
    focus_search_pending: bool,
    /// Frames spent retrying search focus since the window got focus; capped.
    focus_attempts: u8,
    /// `FASTLAUNCH_BENCH=<file>`: write the ready time there and quit.
    bench_out: Option<PathBuf>,
    epic: Arc<Epic>,
    /// Epic display name while signed in.
    epic_name: Option<String>,
    sign_in: Option<SignInDialog>,
    /// Steam's last known state; `None` until checked.
    steam: Option<steam::State>,
    /// Start Steam hidden when the library has Steam games. Off in tests.
    steam_prewarm: bool,
    events_tx: Sender<Event>,
    events_rx: Receiver<Event>,
    /// This frame's `ui.input().time`.
    now: f64,
    /// Set while a hand-made animation runs, so the next frame is requested.
    animating: bool,
}

impl FastLaunch {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        start: Instant,
        lib_rx: Receiver<Loaded>,
        art_rx: Receiver<ArtMessage>,
    ) -> Self {
        setup_fonts(&cc.egui_ctx);
        setup_style(&cc.egui_ctx);
        let (events_tx, events_rx) = mpsc::channel();
        Self {
            start,
            ready_ms: None,
            ready_at: 0.0,
            lib_rx,
            art_rx,
            hero_tx: None,
            games: None,
            titles: Vec::new(),
            art: Vec::new(),
            index_of: HashMap::new(),
            early_art: Vec::new(),
            counts: [0; 3],
            filter: Filter::All,
            search: String::new(),
            visible: Vec::new(),
            focus: None,
            backdrop_prev: None,
            focus_at: 0.0,
            scroll: 0.0,
            scroll_to_focus: false,
            launched_at: f64::NEG_INFINITY,
            status: None,
            focus_search_pending: true,
            focus_attempts: 0,
            bench_out: std::env::var_os("FASTLAUNCH_BENCH").map(PathBuf::from),
            epic: Arc::new(Epic::new()),
            epic_name: None,
            sign_in: None,
            steam: None,
            steam_prewarm: false,
            events_tx,
            events_rx,
            now: 0.0,
            animating: false,
        }
    }

    pub fn with_hero_worker(mut self, hero_tx: Sender<HeroRequest>) -> Self {
        self.hero_tx = Some(hero_tx);
        self
    }

    /// Start Steam hidden once the library is read, if it has Steam games, so
    /// they open without Steam's cold start. `FASTLAUNCH_NO_STEAM_PREWARM=1`
    /// turns it off; benchmark runs skip it.
    pub fn with_steam_prewarm(mut self) -> Self {
        self.steam_prewarm = self.bench_out.is_none() && std::env::var_os("FASTLAUNCH_NO_STEAM_PREWARM").is_none();
        self
    }

    fn start_steam_prewarm(&self, ctx: &egui::Context) {
        let tx = self.events_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            steam::prewarm(|state| {
                let sent = tx.send(Event::Steam(state)).is_ok();
                ctx.request_repaint();
                sent
            });
        });
    }

    /// The games whose hero art should be in memory: the focused one, the one
    /// fading out, and the focused one's neighbours in the row.
    fn hero_wanted(&self) -> [Option<usize>; 4] {
        let pos = self.focus.and_then(|f| self.visible.iter().position(|&v| v == f));
        let near = |d: isize| pos.and_then(|p| p.checked_add_signed(d)).and_then(|p| self.visible.get(p).copied());
        [self.focus, self.backdrop_prev, near(-1), near(1)]
    }

    /// Ask for missing hero art for the wanted games, then evict down to the cap.
    fn sync_heroes(&mut self) {
        let wanted = self.hero_wanted();
        if let Some(f) = self.focus
            && let Some(art) = self.art.get_mut(f)
        {
            art.hero_used = self.now;
        }
        if let (Some(tx), Some(games)) = (&self.hero_tx, &self.games) {
            for index in wanted.into_iter().flatten() {
                let art = &mut self.art[index];
                if art.hero.is_none() && !art.hero_pending && let Some(path) = &art.hero_path {
                    art.hero_pending = tx.send((games[index].key.clone(), path.clone())).is_ok();
                }
            }
        }
        // Without a worker (tests feed art directly) evicted art could not come back.
        while self.hero_tx.is_some() {
            let loaded = self.art.iter().filter(|a| a.hero.is_some()).count();
            if loaded <= HERO_CACHE {
                break;
            }
            let victim = (0..self.art.len())
                .filter(|i| self.art[*i].hero.is_some() && !wanted.contains(&Some(*i)))
                .min_by(|a, b| self.art[*a].hero_used.total_cmp(&self.art[*b].hero_used));
            let Some(victim) = victim else { break };
            // Dropping the handle frees the GPU texture.
            self.art[victim].hero = None;
        }
    }

    fn receive(&mut self, ctx: &egui::Context) {
        if self.games.is_none()
            && let Ok(loaded) = self.lib_rx.try_recv()
        {
            self.epic_name = loaded.epic_account.as_ref().map(|a| a.display_name.clone());
            self.epic.set_account(loaded.epic_account);
            let games = loaded.games;
            // Trademark signs are for logos; at display size they shout.
            self.titles = games.iter().map(|g| g.name.replace(['®', '™'], "").trim().to_uppercase()).collect();
            self.art = games
                .iter()
                .map(|g| GameArt { hero_path: g.art.hero.clone(), ..GameArt::default() })
                .collect();
            self.index_of = games.iter().enumerate().map(|(i, g)| (g.key.clone(), i)).collect();
            let steam = games.iter().filter(|g| g.source == Source::Steam).count();
            self.counts = [games.len(), steam, games.len() - steam];
            self.games = Some(games);
            self.refilter(true);
            if self.steam_prewarm && steam > 0 {
                self.start_steam_prewarm(ctx);
            }
            for message in std::mem::take(&mut self.early_art) {
                self.store_art(ctx, message);
            }
            self.sync_heroes();
        }
        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                Event::Status(status) => self.status = Some(status),
                Event::SignedIn(Ok(name)) => {
                    self.status = Some(Status::Launching(format!(
                        "Signed in to Epic as {name}. Epic games now start without the Epic Games Launcher."
                    )));
                    self.epic_name = Some(name);
                    self.sign_in = None;
                }
                Event::SignedIn(Err(message)) => {
                    if let Some(dialog) = &mut self.sign_in {
                        dialog.busy = false;
                        dialog.error = Some(message);
                    }
                }
                Event::SignedOut => self.epic_name = None,
                Event::Steam(state) => self.steam = Some(state),
            }
        }
        while let Ok(message) = self.art_rx.try_recv() {
            self.store_art(ctx, message);
        }
    }

    fn store_art(&mut self, ctx: &egui::Context, message: ArtMessage) {
        let key = match &message {
            ArtMessage::Image(key, ..) | ArtMessage::HeroPath(key, _) => key,
        };
        let Some(&index) = self.index_of.get(key) else {
            if self.games.is_none() {
                self.early_art.push(message);
            }
            return;
        };
        let (key, kind, image) = match message {
            ArtMessage::Image(key, kind, image) => (key, kind, image),
            ArtMessage::HeroPath(_, path) => {
                self.art[index].hero_path = Some(path);
                self.sync_heroes();
                return;
            }
        };
        let tint = if kind == ArtKind::Logo { None } else { dominant_color(&image) };
        let logo_uv = if kind == ArtKind::Logo { opaque_bounds(&image) } else { FULL_UV };
        let texture = ctx.load_texture(
            format!("{key}/{kind:?}"),
            image,
            egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear)),
        );
        // Replacing a handle frees the earlier texture for the same slot.
        let art = &mut self.art[index];
        match kind {
            ArtKind::Cover => {
                art.cover = Some(texture);
                if art.hero.is_none() {
                    art.tint = tint.or(art.tint);
                }
            }
            ArtKind::Hero => {
                art.hero = Some(texture);
                art.hero_pending = false;
                art.hero_at = self.now;
                art.hero_used = art.hero_used.max(self.now);
                art.tint = tint.or(art.tint);
                self.sync_heroes();
            }
            ArtKind::Logo => {
                art.logo = Some(texture);
                art.logo_uv = logo_uv;
            }
        }
    }

    /// Rebuild `visible`. A new search focuses its first match; otherwise the
    /// focused game stays focused while it is still listed.
    fn refilter(&mut self, focus_first: bool) {
        let Some(games) = &self.games else {
            return;
        };
        let needle = self.search.trim().to_lowercase();
        self.visible = games
            .iter()
            .enumerate()
            .filter(|(_, g)| match self.filter {
                Filter::All => true,
                Filter::Steam => g.source == Source::Steam,
                Filter::Epic => g.source == Source::Epic,
            })
            .filter(|(_, g)| needle.is_empty() || g.name.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
        let keep = !focus_first && self.focus.is_some_and(|f| self.visible.contains(&f));
        if !keep {
            self.set_focus(self.visible.first().copied());
            self.scroll_to_focus = true;
        }
    }

    fn set_focus(&mut self, index: Option<usize>) {
        if index.is_none() || index == self.focus {
            return;
        }
        self.backdrop_prev = self.focus;
        self.focus = index;
        self.focus_at = self.now;
        self.sync_heroes();
    }

    fn step_focus(&mut self, delta: isize) {
        let Some(pos) = self.focus.and_then(|f| self.visible.iter().position(|&v| v == f)) else {
            self.set_focus(self.visible.first().copied());
            return;
        };
        let next = pos.saturating_add_signed(delta).min(self.visible.len() - 1);
        self.set_focus(Some(self.visible[next]));
        self.scroll_to_focus = true;
    }

    fn launch(&mut self, index: usize, ctx: &egui::Context) {
        let Some(game) = self.games.as_ref().and_then(|g| g.get(index)).cloned() else {
            return;
        };
        self.launched_at = self.now;
        let direct = game.epic.clone().filter(|_| self.epic_name.is_some());
        let Some(install) = direct else {
            let waiting = if game.source == Source::Steam && self.steam.is_some() {
                // Cheap (a registry read): catches Steam closing since the last check.
                let state = steam::state();
                let state = if state == steam::State::NotRunning && self.steam == Some(steam::State::Starting) {
                    steam::State::Starting // ours, not registered yet
                } else {
                    state
                };
                self.steam = Some(state);
                match state {
                    steam::State::Ready => "",
                    steam::State::Starting => ". Steam is still starting, so this can take a few seconds",
                    steam::State::NotRunning => ". Steam starts first, so this can take a few seconds",
                }
            } else {
                ""
            };
            let mut status = open_uri(&game, waiting);
            if game.source == Source::Epic
                && let Status::Launching(text) = &mut status
            {
                text.push_str(". Sign in to Epic to skip it.");
            }
            self.status = Some(status);
            return;
        };

        self.status = Some(Status::Launching(format!(
            "Starting {} without the Epic Games Launcher...",
            game.name
        )));
        let epic = Arc::clone(&self.epic);
        let tx = self.events_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let status = match epic.launch(&install) {
                Ok(()) => Status::Launching(format!(
                    "Started {} directly, no Epic Games Launcher",
                    game.name
                )),
                Err(err) => {
                    if matches!(err, epic::Error::SignInExpired(_)) {
                        let _ = tx.send(Event::SignedOut);
                    }
                    open_uri(&game, &format!(" because the direct start failed: {err}"))
                }
            };
            let _ = tx.send(Event::Status(status));
            ctx.request_repaint();
        });
    }

    fn handle_dialog(&mut self, action: DialogAction, ctx: &egui::Context) {
        match action {
            DialogAction::OpenPage => {
                if let Err(err) = open::that_detached(epic::auth_url())
                    && let Some(dialog) = &mut self.sign_in
                {
                    dialog.error = Some(format!("Could not open your browser: {err}"));
                }
            }
            DialogAction::Submit => {
                let Some(dialog) = &mut self.sign_in else {
                    return;
                };
                dialog.busy = true;
                dialog.error = None;
                let code = dialog.code.clone();
                let epic = Arc::clone(&self.epic);
                let tx = self.events_tx.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let result = epic.sign_in(&code).map_err(|e| e.to_string());
                    let _ = tx.send(Event::SignedIn(result));
                    ctx.request_repaint();
                });
            }
            DialogAction::Close => self.sign_in = None,
        }
    }

    /// Progress of an animation that started at `since` and lasts `duration`
    /// seconds, from 0 to 1. Keeps frames coming until it reaches 1.
    fn progress(&mut self, since: f64, duration: f64) -> f32 {
        let t = ((self.now - since) / duration).clamp(0.0, 1.0) as f32;
        if t < 1.0 {
            self.animating = true;
        }
        t
    }
}

impl eframe::App for FastLaunch {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.now = ui.input(|i| i.time);
        self.animating = false;
        self.receive(&ctx);

        if self.ready_ms.is_none() && self.games.is_some() {
            let ms = self.start.elapsed().as_millis();
            self.ready_ms = Some(ms);
            self.ready_at = self.now;
            if let Some(path) = &self.bench_out {
                let _ = std::fs::write(path, format!("{ms}\n"));
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        // Left and Right browse the library, even while the search box has focus.
        if self.sign_in.is_none() {
            let (left, right) = ui.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowLeft),
                    i.consume_key(Modifiers::NONE, Key::ArrowRight),
                )
            });
            if left {
                self.step_focus(-1);
            }
            if right {
                self.step_focus(1);
            }
        }

        let mut actions = Actions::default();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ui, |ui| self.screen(ui, &mut actions));

        if actions.refilter {
            self.refilter(true);
        }
        if let Some(index) = actions.focus {
            self.set_focus(Some(index));
        }
        if actions.open_sign_in {
            self.sign_in = Some(SignInDialog::default());
        }
        if actions.sign_out {
            self.epic.sign_out();
            self.epic_name = None;
            self.status = Some(Status::Launching(
                "Signed out of Epic. Epic games will open through the Epic Games Launcher.".to_owned(),
            ));
        }
        if let Some(index) = actions.launch {
            self.launch(index, &ctx);
        }

        let mut dialog_action = None;
        if let Some(dialog) = &mut self.sign_in {
            let frame = egui::Frame::window(&ctx.global_style())
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, GLASS_EDGE))
                .corner_radius(20)
                .shadow(Shadow { offset: [0, 24], blur: 80, spread: 0, color: Color32::from_black_alpha(200) })
                .inner_margin(28);
            let modal = egui::Modal::new(egui::Id::new("epic_sign_in"))
                .frame(frame)
                .backdrop_color(Color32::from_black_alpha(170))
                .show(&ctx, |ui| sign_in_dialog(ui, dialog));
            let close = modal.should_close() && !dialog.busy;
            dialog_action = modal.inner;
            if close {
                dialog_action = Some(DialogAction::Close);
            }
        }
        if let Some(action) = dialog_action {
            self.handle_dialog(action, &ctx);
        }

        if self.animating {
            ctx.request_repaint();
        }
    }
}

/// Sizes for one frame, from the window size.
struct Layout {
    s: f32,
    screen: Rect,
    pad: f32,
    /// The row of top-bar controls.
    top: Rect,
    /// Where the hero block (logo, meta line, Play) may go.
    hero: Rect,
    /// The row label above the covers.
    row_label: Pos2,
    /// The cover row, one unscaled tile high, from the left padding to the window edge.
    row: Rect,
    tile: Vec2,
    gap: f32,
    footer: Rect,
}

impl Layout {
    fn new(screen: Rect) -> Self {
        let raw = (screen.width() / DESIGN_SIZE.x).min(screen.height() / DESIGN_SIZE.y);
        // Quantised so resizing does not rasterise a new font size every frame.
        let s = (raw.clamp(0.75, 1.6) * 20.0).round() / 20.0;
        let pad = (56.0 * s).min(screen.width() * 0.06).max(20.0);
        let top = Rect::from_min_size(
            pos2(screen.left() + pad, screen.top() + 26.0 * s),
            vec2(screen.width() - 2.0 * pad, 44.0 * s),
        );
        let footer_h = 30.0 * s;
        let footer = Rect::from_min_size(
            pos2(screen.left() + pad, screen.bottom() - footer_h - 18.0 * s),
            vec2(screen.width() - 2.0 * pad, footer_h),
        );
        let tile_h = (176.0 * s).min(screen.height() * 0.24);
        let tile = vec2(tile_h / 1.5, tile_h);
        let row = Rect::from_min_max(
            pos2(screen.left() + pad, footer.top() - 26.0 * s - tile_h),
            pos2(screen.right(), footer.top() - 26.0 * s),
        );
        let row_label = pos2(row.left(), row.top() - 46.0 * s);
        let hero_w = (screen.width() * 0.5).max(560.0 * s).min(screen.width() - 2.0 * pad);
        let hero = Rect::from_min_max(
            pos2(screen.left() + pad, top.bottom() + 24.0 * s),
            pos2(screen.left() + pad + hero_w, row_label.y - 30.0 * s),
        );
        Self { s, screen, pad, top, hero, row_label, row, tile, gap: 18.0 * s, footer }
    }
}

impl FastLaunch {
    fn screen(&mut self, ui: &mut egui::Ui, actions: &mut Actions) {
        let layout = Layout::new(ui.max_rect());
        let painter = ui.painter().clone();
        let loaded = self.games.is_some();
        // Taken out for the frame so drawing can borrow it while state changes.
        let games = self.games.take().unwrap_or_default();

        self.backdrop(&painter, &layout);
        self.top_bar(ui, &painter, &layout, actions);

        let message_area = layout.hero.with_max_y(layout.footer.top());
        if loaded && games.is_empty() {
            empty_state(
                ui,
                message_area,
                "No installed games found",
                "fastlaunch reads the Steam and Epic libraries on this PC. Install a game in either launcher, then reopen fastlaunch.",
            );
        } else if loaded && self.visible.is_empty() {
            let title = if self.search.trim().is_empty() {
                "No games in this library".to_owned()
            } else {
                format!("No games match \"{}\"", self.search.trim())
            };
            if empty_state(ui, message_area, &title, "") {
                self.search.clear();
                self.filter = Filter::All;
                actions.refilter = true;
            }
        } else if let Some(focus) = self.focus.filter(|&f| f < games.len()) {
            self.hero(ui, &painter, &layout, &games, focus, actions);
            self.cover_row(ui, &painter, &layout, &games, actions);
        }

        self.footer(ui, &painter, &layout, actions);
        if loaded {
            self.games = Some(games);
        }
    }

    fn backdrop(&mut self, painter: &Painter, layout: &Layout) {
        let screen = layout.screen;
        let t = ease_out(self.progress(self.focus_at, BACKDROP_FADE));
        let arrive = self.focus.and_then(|f| self.art.get(f)).map_or(0.0, |a| a.hero_at);
        let arrive = ease_out(self.progress(arrive, BACKDROP_FADE));
        let mut tint = DEFAULT_TINT;
        if t < 1.0
            && let Some(prev) = self.backdrop_prev.and_then(|p| self.art.get(p))
        {
            paint_hero_art(painter, screen, prev, 1.0, 1.0);
            tint = prev.tint.unwrap_or(DEFAULT_TINT);
        }
        if let Some(art) = self.focus.and_then(|f| self.art.get(f)) {
            // A slow push-in that settles as the new art fades up.
            paint_hero_art(painter, screen, art, t * arrive, 1.0 + 0.04 * (1.0 - t));
            tint = lerp_color(tint, art.tint.unwrap_or(DEFAULT_TINT), t);
        }

        let h = screen.height();
        let w = screen.width();
        let shade = |a: f32| BG.gamma_multiply(a);
        // Dark on the left for the title, at the bottom for the covers, at the top for the bar.
        gradient(
            painter,
            Rect::from_min_size(screen.min, vec2(w * 0.72, h)),
            false,
            &[(0.0, shade(0.92)), (0.35, shade(0.8)), (0.65, shade(0.38)), (1.0, shade(0.0))],
        );
        gradient(
            painter,
            Rect::from_min_max(pos2(screen.left(), screen.top() + h * 0.36), screen.max),
            true,
            &[(0.0, shade(0.0)), (0.42, shade(0.62)), (0.72, shade(0.93)), (1.0, shade(0.98))],
        );
        gradient(
            painter,
            Rect::from_min_max(pos2(screen.right() - w * 0.22, screen.top()), screen.max),
            false,
            &[(0.0, shade(0.0)), (1.0, shade(0.4))],
        );
        gradient(
            painter,
            Rect::from_min_size(screen.min, vec2(w, 150.0 * layout.s)),
            true,
            &[(0.0, shade(0.7)), (1.0, shade(0.0))],
        );
        // Coloured light from the art pooling behind the cover row.
        radial_glow(
            painter,
            pos2(layout.row.left() + layout.tile.x * 1.5, layout.row.center().y),
            vec2(w * 0.45, layout.tile.y * 1.6),
            tint.gamma_multiply(0.22),
        );
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, painter: &Painter, layout: &Layout, actions: &mut Actions) {
        let s = layout.s;
        let bar = layout.top;
        let narrow = bar.width() < 900.0 * s;

        // Brand.
        let mark = Rect::from_center_size(pos2(bar.left() + 16.0 * s, bar.center().y), Vec2::splat(32.0 * s));
        painter.rect_filled(mark, 9.0 * s, ACCENT);
        bolt(painter, mark.shrink(7.0 * s), BG);
        let mut x = mark.right() + 12.0 * s;
        if !narrow {
            let mut job = LayoutJob::default();
            append(&mut job, "fast", display(21.0 * s), TEXT, -0.3 * s);
            append(&mut job, "launch", display(21.0 * s), TEXT_WEAK, -0.3 * s);
            let galley = painter.layout_job(job);
            let width = galley.size().x;
            painter.galley(pos2(x, bar.center().y - galley.size().y / 2.0), galley, TEXT);
            x += width;
        }
        x += 28.0 * s;

        // Library filter.
        let pill_h = 42.0 * s;
        let [all, steam, epic] = self.counts;
        let tabs = [(Filter::All, "All", all), (Filter::Steam, "Steam", steam), (Filter::Epic, "Epic", epic)];
        let tab_pad = 16.0 * s;
        let galleys = tabs.map(|(_, name, n)| {
            (
                painter.layout_no_wrap(name.to_owned(), body(15.0 * s), TEXT),
                painter.layout_no_wrap(n.to_string(), body(13.0 * s), TEXT),
            )
        });
        let tab_w = |label: &egui::Galley, count: &egui::Galley| label.size().x + 7.0 * s + count.size().x + 2.0 * tab_pad;
        let inner: f32 = galleys.iter().map(|(a, b)| tab_w(a, b)).sum();
        let group = Rect::from_min_size(pos2(x, bar.center().y - pill_h / 2.0), vec2(inner + 8.0 * s, pill_h));
        glass(painter, group, pill_h / 2.0);
        let mut tx = group.left() + 4.0 * s;
        for ((value, name, n), (label, count)) in tabs.into_iter().zip(galleys) {
            let w = tab_w(&label, &count);
            let rect = Rect::from_min_size(pos2(tx, group.top() + 4.0 * s), vec2(w, pill_h - 8.0 * s));
            tx += w;
            let selected = self.filter == value;
            let response = ui.interact(rect, ui.id().with(("filter", name)), Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, format!("{name} {n}"))
            });
            if response.clicked() && !selected {
                self.filter = value;
                actions.refilter = true;
            }
            let hot = ui.ctx().animate_bool_responsive(response.id, response.hovered());
            let (fill, fg, fg_count) = if selected {
                (TEXT, BG, BG.gamma_multiply(0.6))
            } else {
                (Color32::from_white_alpha((16.0 * hot) as u8), lerp_color(TEXT_WEAK, TEXT, hot), TEXT_WEAK.gamma_multiply(0.8))
            };
            painter.rect_filled(rect, rect.height() / 2.0, fill);
            if response.has_focus() {
                painter.rect_stroke(rect, rect.height() / 2.0, Stroke::new(2.0, ACCENT), StrokeKind::Outside);
            }
            let y = rect.center().y;
            let lx = rect.left() + tab_pad;
            let label_w = label.size().x;
            painter.galley_with_override_text_color(pos2(lx, y - label.size().y / 2.0), label, fg);
            painter.galley_with_override_text_color(pos2(lx + label_w + 7.0 * s, y - count.size().y / 2.0), count, fg_count);
            response.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        x = group.right() + 12.0 * s;

        // Startup time, the number this launcher is about.
        let mut chip_left = bar.right();
        if let Some(ms) = self.ready_ms {
            chip_left = self.speed_chip(painter, layout, ms);
        }

        // Search.
        let search_w = (chip_left - 16.0 * s - x).min(300.0 * s);
        if search_w > 120.0 * s {
            let rect = Rect::from_min_size(pos2(x, bar.center().y - pill_h / 2.0), vec2(search_w, pill_h));
            self.search_box(ui, painter, rect, s, actions);
        }
    }

    /// Paints the "ready in" chip at the right end of the top bar; returns its left edge.
    fn speed_chip(&mut self, painter: &Painter, layout: &Layout, ms: u128) -> f32 {
        let s = layout.s;
        let t = self.progress(self.ready_at, COUNT_UP);
        let shown = (ms as f32 * ease_out(t)).round() as u128;
        let h = 46.0 * s;
        let label = painter.layout_job(spaced("READY IN", body(11.0 * s), TEXT_WEAK, 1.6 * s));
        let number_font = display(24.0 * s);
        // Width of the final number, so the chip keeps its size while counting.
        let slot = painter.layout_no_wrap(ms.to_string(), number_font.clone(), ACCENT).size().x;
        let number = painter.layout_no_wrap(shown.to_string(), number_font, ACCENT);
        let unit = painter.layout_no_wrap("ms".to_owned(), body(14.0 * s), TEXT);
        let icon = 14.0 * s;
        let inner = 16.0 * s + icon + 10.0 * s + label.size().x + 10.0 * s + slot + 4.0 * s + unit.size().x + 18.0 * s;
        let y = layout.top.center().y;
        let rect = Rect::from_min_max(pos2(layout.top.right() - inner, y - h / 2.0), pos2(layout.top.right(), y + h / 2.0));

        // A brief flare as the count lands, settling to a steady glow.
        let flare = 1.0 - ((t - 0.85) / 0.15).clamp(0.0, 1.0);
        let glow = 0.22 + 0.35 * flare * t;
        painter.add(Shadow { offset: [0, 0], blur: (34.0 * s) as u8, spread: 0, color: ACCENT.gamma_multiply(glow) }.as_shape(rect, h / 2.0));
        painter.rect(rect, h / 2.0, GLASS, Stroke::new(1.0, ACCENT.gamma_multiply(0.55)), StrokeKind::Inside);

        let mut x = rect.left() + 16.0 * s;
        bolt(painter, Rect::from_min_size(pos2(x, y - icon * 0.62), vec2(icon, icon * 1.24)), ACCENT);
        x += icon + 10.0 * s;
        let label_w = label.size().x;
        painter.galley(pos2(x, y - label.size().y / 2.0), label, TEXT_WEAK);
        x += label_w + 10.0 * s;
        let baseline_shift = number.size().y - unit.size().y;
        let ny = y - number.size().y / 2.0;
        painter.galley(pos2(x + slot - number.size().x, ny), number, ACCENT);
        x += slot + 4.0 * s;
        painter.galley(pos2(x, ny + baseline_shift * 0.78), unit, TEXT);
        rect.left()
    }

    fn search_box(&mut self, ui: &mut egui::Ui, painter: &Painter, rect: Rect, s: f32, actions: &mut Actions) {
        let focus_search = ui.input_mut(|i| i.consume_shortcut(&FOCUS_SEARCH));
        let id = egui::Id::new("search");
        let focused = ui.memory(|m| m.has_focus(id));
        glass(painter, rect, rect.height() / 2.0);
        if focused {
            painter.rect_stroke(rect, rect.height() / 2.0, Stroke::new(1.0, Color32::from_white_alpha(70)), StrokeKind::Inside);
        }
        let icon_c = pos2(rect.left() + 22.0 * s, rect.center().y);
        magnifier(painter, icon_c, 6.5 * s, if focused { TEXT } else { TEXT_WEAK }, 1.8 * s);

        let mut text_right = rect.right() - 16.0 * s;
        if self.search.is_empty() && rect.width() > 200.0 * s {
            let w = keycap(painter, Pos2::ZERO, "Ctrl F", s, false);
            text_right -= w - 6.0 * s;
            keycap(painter, pos2(text_right, rect.center().y), "Ctrl F", s, true);
            text_right -= 8.0 * s;
        }
        let text_rect = Rect::from_min_max(pos2(icon_c.x + 16.0 * s, rect.top()), pos2(text_right, rect.bottom()));
        let builder = egui::UiBuilder::new().max_rect(text_rect).layout(egui::Layout::left_to_right(Align::Center));
        let search = ui
            .scope_builder(builder, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .id(id)
                        .frame(egui::Frame::NONE)
                        .font(body(15.0 * s))
                        .text_color(TEXT)
                        .hint_text(egui::RichText::new("Search games").color(TEXT_WEAK).font(body(15.0 * s)))
                        .desired_width(text_rect.width()),
                )
            })
            .inner;
        if search.changed() {
            actions.refilter = true;
        }
        let (enter, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
        if search.lost_focus() && enter {
            actions.launch = self.focus;
            self.focus_search_pending = true;
        }
        if (search.has_focus() || search.lost_focus()) && escape {
            self.search.clear();
            actions.refilter = true;
            self.focus_search_pending = true;
        }
        // Focused from the start (and after Escape), so typing searches right away.
        if search.has_focus() {
            self.focus_search_pending = false;
        } else if focus_search || self.focus_search_pending {
            search.request_focus();
            // egui only grants focus while the window itself is focused. Retry on the
            // next frame then, and only a few times: retrying while the window is in
            // the background spun the UI thread at ~600 frames per second. Gaining
            // window focus triggers a frame anyway, which lands back here.
            let window_focused = ui.input(|i| i.focused);
            if window_focused && self.focus_attempts < 5 {
                self.focus_attempts += 1;
                ui.ctx().request_repaint();
            }
        }
        if search.has_focus() || !ui.input(|i| i.focused) {
            self.focus_attempts = 0;
        }
        // Enter with nothing focused plays the focused game too.
        if enter && !search.lost_focus() && ui.memory(|m| m.focused().is_none()) {
            actions.launch = self.focus;
        }
    }

    fn hero(&mut self, ui: &mut egui::Ui, painter: &Painter, layout: &Layout, games: &[Game], index: usize, actions: &mut Actions) {
        let s = layout.s;
        let game = &games[index];
        let area = layout.hero;
        let alpha = ease_out(self.progress(self.focus_at, HERO_FADE));
        let lift = (1.0 - alpha) * 14.0 * s;
        let direct = game.source == Source::Epic && self.epic_name.is_some();

        // Laid out bottom-up: Play sits just above the cover row, the title above it.
        let play = vec2(212.0 * s, 60.0 * s);
        let status_h = if self.status.is_some() { 34.0 * s } else { 0.0 };
        let play_rect = Rect::from_min_size(pos2(area.left(), area.bottom() - status_h - play.y), play);
        let meta_y = play_rect.top() - 30.0 * s;
        let title_bottom = meta_y - 26.0 * s;
        let eyebrow_h = 26.0 * s;
        let room = title_bottom - area.top() - eyebrow_h - 20.0 * s;
        let show_eyebrow = room > 60.0 * s;
        let title_box_h = if show_eyebrow { room } else { title_bottom - area.top() }.clamp(30.0 * s, 170.0 * s);
        // Narrow enough that long names stack into a block, like a poster.
        let title_w = area.width().min(480.0 * s);

        // Title: the game's logo when there is one, otherwise its name, big.
        let title_top = match self.art.get(index).and_then(|a| Some((a.logo.as_ref()?, a.logo_uv))) {
            Some((logo, uv)) => {
                let size = logo.size_vec2() * uv.size();
                // Same visual weight for wide and tall logos: aim for a fixed area.
                let area_fit = (440.0 * 130.0 * s * s / (size.x * size.y)).sqrt();
                let fit = area_fit.min(title_w * 0.85 / size.x).min(title_box_h / size.y);
                let rect = Rect::from_min_size(pos2(area.left(), title_bottom - size.y * fit + lift), size * fit);
                painter.image(logo.id(), rect, uv, Color32::WHITE.gamma_multiply(alpha));
                rect.top()
            }
            None => {
                let mut size = 76.0 * s;
                let galley = loop {
                    let mut job = spaced(&self.titles[index], display(size), TEXT, -0.02 * size);
                    job.wrap.max_width = title_w;
                    job.wrap.max_rows = 3;
                    let galley = painter.layout_job(job);
                    if galley.size().y <= title_box_h || size < 28.0 * s {
                        break galley;
                    }
                    size *= 0.85;
                };
                let pos = pos2(area.left() - size * 0.04, title_bottom - galley.size().y + lift);
                // A soft shadow so a light backdrop never swallows the letters.
                painter.galley(pos + vec2(0.0, 3.0 * s), galley.clone(), Color32::from_black_alpha((90.0 * alpha) as u8));
                painter.galley(pos, galley, TEXT.gamma_multiply(alpha));
                pos.y
            }
        };

        if show_eyebrow {
            let y = title_top - 18.0 * s - eyebrow_h;
            let source = match game.source {
                Source::Steam => "STEAM",
                Source::Epic => "EPIC GAMES",
            };
            let w = badge(painter, pos2(area.left(), y), eyebrow_h, source, TEXT, GLASS, s, alpha);
            if direct {
                badge(painter, pos2(area.left() + w + 8.0 * s, y), eyebrow_h, "DIRECT LAUNCH", BG, ACCENT, s, alpha);
            }
        }

        // What happens on Play.
        let (lead, rest, lead_color) = match (game.source, direct) {
            (Source::Epic, true) => ("Starts without the Epic Games Launcher", "", ACCENT),
            (Source::Epic, false) => ("Opens through the Epic Games Launcher", "  ·  Sign in to Epic to skip it", TEXT),
            (Source::Steam, _) => match self.steam {
                Some(steam::State::Ready) => ("Launches through Steam", "  ·  Steam is ready", TEXT),
                Some(steam::State::Starting) => ("Launches through Steam", "  ·  Starting Steam in the background", TEXT),
                Some(steam::State::NotRunning) => ("Launches through Steam", "  ·  Steam starts first", TEXT),
                None => ("Launches through Steam", "", TEXT),
            },
        };
        let mut mx = area.left();
        if direct {
            bolt(painter, Rect::from_min_size(pos2(mx, meta_y - 8.0 * s + lift), vec2(13.0 * s, 16.0 * s)), ACCENT.gamma_multiply(alpha));
            mx += 21.0 * s;
        }
        let mut job = spaced(lead, body(16.0 * s), lead_color.gamma_multiply(alpha), 0.0);
        append(&mut job, rest, body(16.0 * s), TEXT_WEAK.gamma_multiply(alpha), 0.0);
        let galley = painter.layout_job(job);
        painter.galley(pos2(mx, meta_y - galley.size().y / 2.0 + lift), galley, TEXT);

        // Play.
        let response = ui.interact(play_rect, ui.id().with("play"), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Play {}", game.name)));
        let hover = ui.ctx().animate_bool_responsive(response.id, response.hovered() || response.has_focus());
        let press = if response.is_pointer_button_down_on() { 0.97 } else { 1.0 };
        let rect = Rect::from_center_size(play_rect.center(), play_rect.size() * press);
        let r = rect.height() / 2.0;
        painter.add(
            Shadow {
                offset: [0, (10.0 * s) as i8],
                blur: (44.0 * s).min(255.0) as u8,
                spread: 0,
                color: ACCENT.gamma_multiply(0.38 + 0.3 * hover),
            }
            .as_shape(rect, r),
        );
        // A ring that ripples out when a game starts.
        let ripple = self.progress(self.launched_at, 0.6);
        if ripple < 1.0 {
            let grow = ease_out(ripple) * 26.0 * s;
            painter.rect_stroke(rect.expand(grow), r + grow, Stroke::new(2.0 * s, ACCENT.gamma_multiply(1.0 - ripple)), StrokeKind::Outside);
        }
        painter.rect_filled(rect, r, lerp_color(ACCENT, Color32::WHITE, 0.18 * hover));
        if response.has_focus() {
            painter.rect_stroke(rect.expand(3.0 * s), r + 3.0 * s, Stroke::new(2.0 * s, TEXT), StrokeKind::Outside);
        }
        let label = painter.layout_job(spaced("PLAY", display(19.0 * s), BG, 2.4 * s));
        let tri = 15.0 * s;
        let content_w = tri + 12.0 * s + label.size().x;
        let cx = rect.center().x - content_w / 2.0;
        let cy = rect.center().y;
        painter.add(Shape::convex_polygon(
            vec![pos2(cx, cy - tri * 0.58), pos2(cx + tri * 0.9, cy), pos2(cx, cy + tri * 0.58)],
            BG,
            Stroke::NONE,
        ));
        painter.galley(pos2(cx + tri + 12.0 * s, cy - label.size().y / 2.0), label, BG);
        if response.clicked() {
            actions.launch = Some(index);
        }
        response.on_hover_cursor(egui::CursorIcon::PointingHand);

        // The last launch result.
        if let Some(status) = &self.status {
            let (text, color, dot) = match status {
                Status::Launching(text) => (text.as_str(), TEXT, ACCENT),
                Status::Error(text) => (text.as_str(), ui.visuals().error_fg_color, ui.visuals().error_fg_color),
            };
            let y = play_rect.bottom() + 24.0 * s;
            painter.circle_filled(pos2(area.left() + 4.0 * s, y), 3.5 * s, dot);
            let mut job = spaced(text, body(14.0 * s), color, 0.0);
            job.wrap = egui::text::TextWrapping::truncate_at_width(layout.screen.width() - 2.0 * layout.pad - 16.0 * s);
            let galley = painter.layout_job(job);
            painter.galley(pos2(area.left() + 16.0 * s, y - galley.size().y / 2.0), galley, color);
        }
    }

    fn cover_row(&mut self, ui: &mut egui::Ui, painter: &Painter, layout: &Layout, games: &[Game], actions: &mut Actions) {
        let s = layout.s;
        let row = layout.row;
        let stride = layout.tile.x + layout.gap;
        let n = self.visible.len();

        // Row label: which library this is.
        let name = match self.filter {
            Filter::All => "ALL GAMES",
            Filter::Steam => "STEAM",
            Filter::Epic => "EPIC GAMES",
        };
        let mut job = spaced(name, display(13.0 * s), TEXT, 1.8 * s);
        append(&mut job, &format!("   {n}"), body(13.0 * s), TEXT_WEAK, 1.0 * s);
        let needle = self.search.trim();
        if !needle.is_empty() {
            append(&mut job, &format!("   ·   matching \"{needle}\""), body(13.0 * s), TEXT_WEAK, 0.0);
        }
        painter.add(Shape::galley(layout.row_label, painter.layout_job(job), TEXT));

        // Keyboard focus pulls its tile into view; the wheel scrolls freely.
        let view_w = row.width() - layout.pad;
        let max_scroll = (n as f32 * stride - layout.gap - view_w + layout.tile.x * 0.2).max(0.0);
        if self.scroll_to_focus
            && let Some(p) = self.focus.and_then(|f| self.visible.iter().position(|&v| v == f))
        {
            let x = p as f32 * stride;
            let margin = layout.tile.x * 0.3;
            if x - margin < self.scroll * s {
                self.scroll = (x - margin) / s;
            } else if x + layout.tile.x + margin > self.scroll * s + view_w {
                self.scroll = (x + layout.tile.x + margin - view_w) / s;
            }
            self.scroll_to_focus = false;
        }
        let row_hit = ui.interact(row.expand(16.0 * s), ui.id().with("cover_row"), Sense::hover());
        if row_hit.hovered() {
            let delta = ui.input(|i| i.smooth_scroll_delta);
            self.scroll -= (delta.x + delta.y) / s;
        }
        self.scroll = self.scroll.clamp(0.0, max_scroll / s);
        let offset = ui.ctx().animate_value_with_time(egui::Id::new("cover_row_scroll"), self.scroll * s, 0.25);

        let moved = ui.input(|i| i.pointer.delta() != Vec2::ZERO);
        let clip = Rect::from_min_max(pos2(layout.screen.left(), row.top() - 40.0 * s), pos2(layout.screen.right(), row.bottom() + 24.0 * s));
        let tile_painter = painter.with_clip_rect(clip);
        let mut focused_tile = None;
        for (pos, &index) in self.visible.iter().enumerate() {
            let rect = Rect::from_min_size(pos2(row.left() + pos as f32 * stride - offset, row.top()), layout.tile);
            if rect.right() < clip.left() || rect.left() > clip.right() {
                continue;
            }
            let game = &games[index];
            let response = ui.interact(rect, ui.id().with(("tile", &game.key)), Sense::click());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &game.name));
            if response.hovered() && moved {
                actions.focus = Some(index);
            }
            if response.clicked() {
                actions.launch = Some(index);
            }
            let focused = self.focus == Some(index);
            let k = ui.ctx().animate_value_with_time(response.id.with("focus"), if focused { 1.0 } else { 0.0 }, 0.2);
            let hot = ui.ctx().animate_bool_responsive(response.id.with("hot"), response.hovered() || response.has_focus());
            let tile = Tile { rect, focus: k, hot, keyboard: response.has_focus() };
            if focused {
                focused_tile = Some((tile, index));
            } else {
                self.paint_tile(&tile_painter, game, index, &tile, s);
            }
            response.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        // The focused tile last, so its glow lies over its neighbours.
        if let Some((tile, index)) = focused_tile {
            self.paint_tile(&tile_painter, &games[index], index, &tile, s);
        }
    }

    fn paint_tile(&self, painter: &Painter, game: &Game, index: usize, tile: &Tile, s: f32) {
        let k = tile.focus;
        let scale = 1.0 + 0.12 * k + 0.03 * tile.hot * (1.0 - k);
        let rect = Rect::from_center_size(tile.rect.center(), tile.rect.size() * scale);
        let radius = TILE_RADIUS * s;
        if k > 0.0 {
            painter.add(Shadow { offset: [0, (14.0 * s) as i8], blur: (40.0 * s) as u8, spread: 0, color: ACCENT.gamma_multiply(0.3 * k) }.as_shape(rect, radius));
        }
        painter.add(Shadow { offset: [0, (10.0 * s) as i8], blur: (24.0 * s) as u8, spread: 0, color: Color32::from_black_alpha(160) }.as_shape(rect, radius));
        match self.art.get(index).and_then(|a| a.cover.as_ref()) {
            Some(texture) => {
                painter.add(RectShape::filled(rect, radius, Color32::WHITE).with_texture(texture.id(), FULL_UV));
            }
            None => {
                painter.rect_filled(rect, radius, placeholder_color(&game.key));
                let mut job = LayoutJob::simple(game.name.clone(), display(18.0 * s), TEXT, rect.width() - 20.0 * s);
                job.halign = Align::Center;
                let galley = painter.layout_job(job);
                painter.galley(pos2(rect.center().x, rect.center().y - galley.size().y / 2.0), galley, TEXT);
            }
        }
        // Unfocused covers sit back a little.
        let dim = 0.45 * (1.0 - k) * (1.0 - 0.6 * tile.hot);
        if dim > 0.0 {
            painter.rect_filled(rect, radius, Color32::from_black_alpha((255.0 * dim) as u8));
        }
        painter.rect_stroke(rect, radius, Stroke::new(1.0, Color32::from_white_alpha((20.0 + 30.0 * tile.hot) as u8)), StrokeKind::Inside);
        if k > 0.0 {
            let ring = 4.0 * s;
            painter.rect_stroke(rect.expand(ring), radius + ring, Stroke::new(2.5 * s, ACCENT.gamma_multiply(k)), StrokeKind::Outside);
        }
        if tile.keyboard {
            painter.rect_stroke(rect.expand(9.0 * s), radius + 9.0 * s, Stroke::new(1.5 * s, TEXT), StrokeKind::Outside);
        }
    }

    fn footer(&mut self, ui: &mut egui::Ui, painter: &Painter, layout: &Layout, actions: &mut Actions) {
        let s = layout.s;
        let bar = layout.footer;

        // Epic account, right-aligned.
        let builder = egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::right_to_left(Align::Center));
        let account_left = ui
            .scope_builder(builder, |ui| {
                let v = ui.visuals_mut();
                for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
                    w.corner_radius = CornerRadius::same(255);
                }
                v.widgets.inactive.weak_bg_fill = GLASS;
                v.widgets.inactive.bg_stroke = Stroke::new(1.0, GLASS_EDGE);
                v.widgets.hovered.weak_bg_fill = GLASS_HOVER;
                ui.spacing_mut().button_padding = vec2(14.0 * s, 6.0 * s);
                let font = body(13.0 * s);
                let response = match &self.epic_name {
                    Some(name) => {
                        ui.menu_button(egui::RichText::new(format!("Epic: {name}")).font(font).color(TEXT), |ui| {
                            if ui.button("Sign out of Epic").clicked() {
                                actions.sign_out = true;
                            }
                        })
                        .response
                    }
                    None => {
                        let response = ui
                            .button(egui::RichText::new("Sign in to Epic").font(font).color(TEXT))
                            .on_hover_text("Start Epic games without the Epic Games Launcher");
                        if response.clicked() {
                            actions.open_sign_in = true;
                        }
                        response
                    }
                };
                response.rect.left()
            })
            .inner;

        // Keyboard hints, while there is room.
        let y = bar.center().y;
        let mut x = bar.left();
        for (key, what) in [("←  →", "Browse"), ("Enter", "Play"), ("Esc", "Clear search")] {
            let label = painter.layout_no_wrap(what.to_owned(), body(13.0 * s), TEXT_WEAK);
            let key_w = keycap(painter, Pos2::ZERO, key, s, false);
            let label_w = label.size().x;
            if x + key_w + 8.0 * s + label_w > account_left - 24.0 * s {
                break;
            }
            keycap(painter, pos2(x, y), key, s, true);
            x += key_w + 8.0 * s;
            painter.galley(pos2(x, y - label.size().y / 2.0), label, TEXT_WEAK);
            x += label_w + 22.0 * s;
        }
    }
}

/// One cover in the row, as laid out this frame.
struct Tile {
    rect: Rect,
    /// 0..1, animated: how focused this tile is.
    focus: f32,
    /// 0..1, animated: hovered or keyboard-focused.
    hot: f32,
    keyboard: bool,
}

/// Hand the game to its own launcher through its URL scheme.
fn open_uri(game: &Game, reason: &str) -> Status {
    let via = match game.source {
        Source::Steam => "Steam",
        Source::Epic => "the Epic Games Launcher",
    };
    match open::that_detached(&game.launch_uri) {
        Ok(()) => Status::Launching(format!("Launching {} through {via}{reason}", game.name)),
        Err(err) => Status::Error(format!(
            "Could not launch {}: {err}. Check that {via} is installed.",
            game.name
        )),
    }
}

fn sign_in_dialog(ui: &mut egui::Ui, dialog: &mut SignInDialog) -> Option<DialogAction> {
    let mut action = None;
    ui.set_width(460.0);
    ui.label(egui::RichText::new("Sign in to Epic").heading().color(TEXT));
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(
            "fastlaunch can start Epic games directly, without the Epic Games Launcher. Sign in once:",
        )
        .color(TEXT_WEAK),
    );
    ui.add_space(16.0);

    ui.label(egui::RichText::new("1. Sign in on Epic's website").color(TEXT));
    ui.add_space(2.0);
    if ui.button("Open Epic sign-in page").clicked() {
        action = Some(DialogAction::OpenPage);
    }
    ui.add_space(16.0);

    ui.label(egui::RichText::new("2. Paste what the page shows").color(TEXT));
    ui.label(
        egui::RichText::new(
            "After you sign in, the page shows text that contains \"authorizationCode\". Copy all of it.",
        )
        .color(TEXT_WEAK),
    );
    ui.add_space(4.0);
    let field = ui.add_enabled(
        !dialog.busy,
        egui::TextEdit::singleline(&mut dialog.code)
            .hint_text("Paste the page text or the code")
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(12, 9)),
    );
    let can_submit = !dialog.busy && !dialog.code.trim().is_empty();
    if can_submit && field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
        action = Some(DialogAction::Submit);
    }
    if let Some(error) = &dialog.error {
        ui.add_space(6.0);
        ui.label(egui::RichText::new(error).color(ui.visuals().error_fg_color));
    }
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Your sign-in is stored in Windows Credential Manager.")
            .small()
            .color(TEXT_WEAK),
    );
    ui.add_space(18.0);

    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // Right to left: primary action first.
        let selection = ui.visuals().selection;
        let sign_in = egui::Button::new(egui::RichText::new("Sign in").color(selection.stroke.color))
            .fill(selection.bg_fill)
            .min_size(vec2(96.0, 38.0));
        if ui
            .add_enabled(can_submit, sign_in)
            .on_disabled_hover_text("Paste the code from Epic's page first")
            .clicked()
        {
            action = Some(DialogAction::Submit);
        }
        if ui.add_enabled(!dialog.busy, egui::Button::new("Cancel").min_size(vec2(0.0, 38.0))).clicked() {
            action = Some(DialogAction::Close);
        }
        if dialog.busy {
            ui.spinner();
        }
    });
    action
}

/// A message in `area`, with a "Show all games" button unless `detail`
/// explains the situation instead. Returns whether the button was clicked.
fn empty_state(ui: &mut egui::Ui, area: Rect, title: &str, detail: &str) -> bool {
    let builder = egui::UiBuilder::new().max_rect(area).layout(egui::Layout::top_down(Align::Min));
    ui.scope_builder(builder, |ui| {
        ui.add_space(area.height() * 0.3);
        ui.set_max_width(560.0);
        ui.label(egui::RichText::new(title).font(display(34.0)).color(TEXT));
        ui.add_space(10.0);
        if !detail.is_empty() {
            ui.label(egui::RichText::new(detail).color(TEXT_WEAK));
            return false;
        }
        let button = egui::Button::new(egui::RichText::new("Show all games").color(BG)).fill(ACCENT).min_size(vec2(0.0, 40.0));
        ui.add(button).clicked()
    })
    .inner
}

// ---------------------------------------------------------------- painting

/// The game's wide art, scaled to cover `screen`, weighted to its upper right
/// where the subject usually is (the left and bottom are under the scrim).
fn paint_hero_art(painter: &Painter, screen: Rect, art: &GameArt, alpha: f32, zoom: f32) {
    if alpha <= 0.0 {
        return;
    }
    // Without hero art, the cover stretched wide reads as a soft blur behind the scrim.
    let (texture, alpha) = match (&art.hero, &art.cover) {
        (Some(hero), _) => (hero, alpha),
        (None, Some(cover)) => (cover, alpha * 0.55),
        (None, None) => return,
    };
    let image = texture.size_vec2();
    let (ia, sa) = (image.x / image.y, screen.width() / screen.height());
    let uv_size = if ia > sa { vec2(sa / ia, 1.0) } else { vec2(1.0, ia / sa) } / zoom;
    let uv_min = pos2((1.0 - uv_size.x) * 0.62, (1.0 - uv_size.y) * 0.3);
    painter.image(texture.id(), screen, Rect::from_min_size(uv_min, uv_size), Color32::WHITE.gamma_multiply(alpha));
}

/// A linear gradient through `stops` (position 0..1, colour), top to bottom
/// when `vertical`, otherwise left to right.
fn gradient(painter: &Painter, rect: Rect, vertical: bool, stops: &[(f32, Color32)]) {
    let mut mesh = Mesh::default();
    for (i, &(t, color)) in stops.iter().enumerate() {
        let (a, b) = if vertical {
            let y = egui::lerp(rect.top()..=rect.bottom(), t);
            (pos2(rect.left(), y), pos2(rect.right(), y))
        } else {
            let x = egui::lerp(rect.left()..=rect.right(), t);
            (pos2(x, rect.top()), pos2(x, rect.bottom()))
        };
        mesh.colored_vertex(a, color);
        mesh.colored_vertex(b, color);
        if i > 0 {
            let k = 2 * i as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k, k + 1);
        }
    }
    painter.add(mesh);
}

/// An elliptical pool of light, `color` at the centre fading out at `radius`.
fn radial_glow(painter: &Painter, center: Pos2, radius: Vec2, color: Color32) {
    const SEGMENTS: u32 = 48;
    const RINGS: [(f32, f32); 3] = [(0.35, 0.7), (0.65, 0.3), (1.0, 0.0)];
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, color);
    for (r, a) in RINGS {
        for i in 0..SEGMENTS {
            let angle = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let p = center + vec2(angle.cos() * radius.x * r, angle.sin() * radius.y * r);
            mesh.colored_vertex(p, color.gamma_multiply(a));
        }
    }
    for i in 0..SEGMENTS {
        let j = (i + 1) % SEGMENTS;
        mesh.add_triangle(0, 1 + i, 1 + j);
        for ring in 0..RINGS.len() as u32 - 1 {
            let (a, b) = (1 + ring * SEGMENTS, 1 + (ring + 1) * SEGMENTS);
            mesh.add_triangle(a + i, a + j, b + i);
            mesh.add_triangle(a + j, b + j, b + i);
        }
    }
    painter.add(mesh);
}

/// A dark translucent capsule with a hairline edge.
fn glass(painter: &Painter, rect: Rect, radius: f32) {
    painter.rect(rect, radius, GLASS, Stroke::new(1.0, GLASS_EDGE), StrokeKind::Inside);
}

/// A lightning bolt filling `rect`.
fn bolt(painter: &Painter, rect: Rect, color: Color32) {
    let p = |x: f32, y: f32| pos2(rect.left() + rect.width() * x, rect.top() + rect.height() * y);
    // Two convex halves that overlap a little, so no seam shows between them.
    painter.add(Shape::convex_polygon(vec![p(0.66, 0.0), p(0.08, 0.6), p(0.5, 0.6), p(0.58, 0.38)], color, Stroke::NONE));
    painter.add(Shape::convex_polygon(vec![p(0.58, 0.38), p(0.48, 0.62), p(0.34, 1.0), p(0.92, 0.38)], color, Stroke::NONE));
}

fn magnifier(painter: &Painter, center: Pos2, r: f32, color: Color32, width: f32) {
    let c = center - vec2(r * 0.18, r * 0.18);
    painter.circle_stroke(c, r, Stroke::new(width, color));
    let d = r * std::f32::consts::FRAC_1_SQRT_2;
    painter.line_segment([c + vec2(d, d), c + vec2(d + r * 0.6, d + r * 0.6)], Stroke::new(width, color));
}

/// A key label like "Esc" whose left edge is vertically centred on `left_center`.
/// Returns its width; paints nothing unless `paint`.
fn keycap(painter: &Painter, left_center: Pos2, text: &str, s: f32, paint: bool) -> f32 {
    let galley = painter.layout_no_wrap(text.to_owned(), body(11.5 * s), TEXT);
    let size = vec2(galley.size().x + 14.0 * s, 22.0 * s);
    if paint {
        let rect = Rect::from_min_size(pos2(left_center.x, left_center.y - size.y / 2.0), size);
        painter.rect(rect, 6.0 * s, Color32::from_white_alpha(14), Stroke::new(1.0, Color32::from_white_alpha(34)), StrokeKind::Inside);
        painter.galley(rect.center() - galley.size() / 2.0, galley, TEXT);
    }
    size.x
}

/// A small capsule with spaced capitals. Returns its width.
#[allow(clippy::too_many_arguments)]
fn badge(painter: &Painter, top_left: Pos2, h: f32, text: &str, fg: Color32, fill: Color32, s: f32, alpha: f32) -> f32 {
    let galley = painter.layout_job(spaced(text, display(11.0 * s), fg.gamma_multiply(alpha), 1.4 * s));
    let rect = Rect::from_min_size(top_left, vec2(galley.size().x + 22.0 * s, h));
    let edge = if fill == GLASS { GLASS_EDGE } else { Color32::TRANSPARENT };
    painter.rect(rect, h / 2.0, fill.gamma_multiply(alpha), Stroke::new(1.0, edge.gamma_multiply(alpha)), StrokeKind::Inside);
    painter.galley(rect.center() - galley.size() / 2.0, galley, fg);
    rect.width()
}

fn append(job: &mut LayoutJob, text: &str, font_id: FontId, color: Color32, letter_spacing: f32) {
    job.append(text, 0.0, TextFormat { font_id, color, extra_letter_spacing: letter_spacing, ..Default::default() });
}

fn spaced(text: &str, font_id: FontId, color: Color32, letter_spacing: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    append(&mut job, text, font_id, color, letter_spacing);
    job
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

/// A stable, muted colour per game for tiles without cover art.
fn placeholder_color(key: &str) -> Color32 {
    let hash = key
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    let hue = (hash % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.45, 0.38, 1.0).into()
}

/// The bounding box of the image's visible pixels, as a UV rect.
fn opaque_bounds(image: &egui::ColorImage) -> Rect {
    let [w, h] = image.size;
    let (mut min, mut max) = ([w, h], [0, 0]);
    for (i, c) in image.pixels.iter().enumerate() {
        if c.a() > 12 {
            let (x, y) = (i % w, i / w);
            min = [min[0].min(x), min[1].min(y)];
            max = [max[0].max(x), max[1].max(y)];
        }
    }
    if min[0] > max[0] || min[1] > max[1] {
        return FULL_UV;
    }
    Rect::from_min_max(
        pos2(min[0] as f32 / w as f32, min[1] as f32 / h as f32),
        pos2((max[0] + 1) as f32 / w as f32, (max[1] + 1) as f32 / h as f32),
    )
}

/// The image's most common vivid hue, brightened for use as light.
/// Samples about 4000 pixels, so it is cheap enough for the UI thread.
fn dominant_color(image: &egui::ColorImage) -> Option<Color32> {
    const BUCKETS: usize = 24;
    let mut weight = [0.0f32; BUCKETS];
    let mut sum = [[0.0f32; 3]; BUCKETS];
    let step = (image.pixels.len() / 4000).max(1) | 1;
    for c in image.pixels.iter().step_by(step) {
        let hsva = egui::ecolor::HsvaGamma::from(*c);
        let w = hsva.s * hsva.v * hsva.v;
        if w < 0.05 {
            continue;
        }
        let b = ((hsva.h * BUCKETS as f32) as usize).min(BUCKETS - 1);
        weight[b] += w;
        for (acc, v) in sum[b].iter_mut().zip([c.r(), c.g(), c.b()]) {
            *acc += w * f32::from(v);
        }
    }
    let (best, &w) = weight.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    if w < 1.0 {
        return None;
    }
    let [r, g, b] = sum[best].map(|v| (v / w) as u8);
    let mut hsva = egui::ecolor::HsvaGamma::from(Color32::from_rgb(r, g, b));
    hsva.s = hsva.s.clamp(0.55, 0.9);
    hsva.v = 0.95;
    Some(hsva.into())
}

const DISPLAY_FAMILY: &str = "display";

/// Geist ExtraBold, for titles and numbers.
fn display(size: f32) -> FontId {
    static FAMILY: OnceLock<FontFamily> = OnceLock::new();
    FontId::new(size, FAMILY.get_or_init(|| FontFamily::Name(DISPLAY_FAMILY.into())).clone())
}

/// Geist Medium, for everything else.
fn body(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "geist-medium".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!("../assets/Geist-Medium.ttf"))),
    );
    fonts.font_data.insert(
        "geist-extrabold".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!("../assets/Geist-ExtraBold.ttf"))),
    );
    // egui's own fonts stay behind Geist for symbols and other scripts.
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    fonts.families.insert(
        FontFamily::Proportional,
        std::iter::once("geist-medium".to_owned()).chain(fallback.iter().cloned()).collect(),
    );
    fonts.families.insert(
        FontFamily::Name(DISPLAY_FAMILY.into()),
        std::iter::once("geist-extrabold".to_owned()).chain(fallback).collect(),
    );
    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    // A game launcher: dark only.
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.text_styles = [
            (TextStyle::Heading, display(26.0)),
            (TextStyle::Body, body(15.0)),
            (TextStyle::Button, body(15.0)),
            (TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace)),
            (TextStyle::Small, body(12.0)),
        ]
        .into();
        style.spacing.button_padding = vec2(16.0, 8.0);
        style.spacing.item_spacing = vec2(10.0, 8.0);
        style.animation_time = 0.15;

        let v = &mut style.visuals;
        v.panel_fill = BG;
        v.window_fill = SURFACE;
        v.window_corner_radius = CornerRadius::same(14);
        v.menu_corner_radius = CornerRadius::same(14);
        v.window_stroke = Stroke::new(1.0, GLASS_EDGE);
        v.extreme_bg_color = Color32::from_rgb(0x0B, 0x0C, 0x11);
        v.override_text_color = None;
        v.selection.bg_fill = ACCENT;
        v.selection.stroke = Stroke::new(1.5, BG);
        v.text_cursor.stroke = Stroke::new(2.0, ACCENT);
        // A blinking cursor would wake the app twice a second while idle.
        v.text_cursor.blink = false;

        let radius = CornerRadius::same(10);
        for w in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = radius;
        }
        v.widgets.inactive.weak_bg_fill = CONTROL;
        v.widgets.inactive.bg_fill = CONTROL;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.hovered.weak_bg_fill = CONTROL_HOVER;
        v.widgets.hovered.bg_fill = CONTROL_HOVER;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, TEXT_WEAK);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.active.weak_bg_fill = CONTROL_HOVER;
        v.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};
    use std::sync::mpsc;

    fn game(key: &str, name: &str, source: Source) -> Game {
        Game {
            key: key.to_owned(),
            name: name.to_owned(),
            source,
            launch_uri: String::new(),
            art: Default::default(),
            epic: None,
        }
    }

    fn harness() -> Harness<'static, FastLaunch> {
        let (lib_tx, lib_rx) = mpsc::channel();
        let (_art_tx, art_rx) = mpsc::channel();
        lib_tx
            .send(Loaded {
                games: vec![
                    game("steam:1", "Aimlabs", Source::Steam),
                    game("epic:Fortnite", "Fortnite", Source::Epic),
                    game("epic:Sugar", "Rocket League", Source::Epic),
                ],
                epic_account: None,
            })
            .unwrap();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1280.0, 760.0))
            .build_eframe(move |cc| FastLaunch::new(cc, Instant::now(), lib_rx, art_rx));
        harness.run();
        harness
    }

    fn type_text(harness: &mut Harness<'_, FastLaunch>, text: &str) {
        harness.input_mut().events.push(egui::Event::Text(text.to_owned()));
        harness.run();
    }

    #[test]
    fn typing_at_startup_goes_into_search() {
        let mut harness = harness();
        harness.get_by_label("Fortnite");
        type_text(&mut harness, "roc");
        assert_eq!(harness.state().search, "roc", "search box should have focus at startup");
        assert!(harness.query_by_label("Rocket League").is_some());
        assert!(harness.query_by_label("Fortnite").is_none(), "non-matching games should be hidden");
    }

    #[test]
    fn escape_clears_search_and_keeps_focus() {
        let mut harness = harness();
        type_text(&mut harness, "roc");
        harness.get_by_label("Rocket League");
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert_eq!(harness.state().search, "");
        harness.get_by_label("Fortnite");
        type_text(&mut harness, "aim");
        assert_eq!(harness.state().search, "aim", "search should be focused again after Escape");
    }

    #[test]
    fn epic_filter_hides_steam_games() {
        let mut harness = harness();
        harness.get_by_label("Aimlabs");
        harness.get_by_label("Epic 2").click();
        harness.run();
        harness.get_by_label("Fortnite");
        assert!(harness.query_by_label("Aimlabs").is_none());
    }

    #[test]
    fn no_match_shows_empty_state() {
        let mut harness = harness();
        type_text(&mut harness, "zzz");
        harness.get_by_label("No games match \"zzz\"");
        harness.get_by_label("Show all games").click();
        harness.run();
        harness.get_by_label("Aimlabs");
    }

    #[test]
    fn arrow_keys_browse_while_search_has_focus() {
        let mut harness = harness();
        type_text(&mut harness, "e");
        harness.get_by_label("Play Fortnite");
        harness.key_press(egui::Key::ArrowRight);
        harness.run();
        harness.get_by_label("Play Rocket League");
        harness.key_press(egui::Key::ArrowLeft);
        harness.run();
        harness.get_by_label("Play Fortnite");
        assert_eq!(harness.state().search, "e", "arrows should not edit the search");
    }

    #[test]
    fn idle_screen_stops_repainting() {
        let mut harness = harness();
        harness.run_steps(8);
        harness.step();
        assert!(!harness.ctx.has_requested_repaint(), "an idle launcher should not keep repainting");
    }
}

#[cfg(test)]
mod sign_in_tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};

    fn harness() -> Harness<'static, FastLaunch> {
        let (lib_tx, lib_rx) = mpsc::channel();
        let (_art_tx, art_rx) = mpsc::channel();
        lib_tx.send(Loaded { games: Vec::new(), epic_account: None }).unwrap();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1280.0, 760.0))
            .build_eframe(move |cc| FastLaunch::new(cc, Instant::now(), lib_rx, art_rx));
        harness.run();
        harness
    }

    #[test]
    fn sign_in_dialog_opens_and_cancels() {
        let mut harness = harness();
        harness.get_by_label("Sign in to Epic").click();
        harness.run();
        harness.get_by_label("Open Epic sign-in page");
        assert!(harness.state().sign_in.is_some());
        harness.get_by_label("Cancel").click();
        harness.run();
        assert!(harness.state().sign_in.is_none(), "Cancel should close the dialog");
    }

    #[test]
    fn bad_code_shows_error_without_network() {
        let mut harness = harness();
        harness.state_mut().sign_in = Some(SignInDialog { code: "not a code".to_owned(), ..Default::default() });
        harness.run();
        harness.get_by_label("Sign in").click();
        // The worker thread answers asynchronously.
        for _ in 0..50 {
            harness.run();
            if harness.state().sign_in.as_ref().is_some_and(|d| d.error.is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let error = harness.state().sign_in.as_ref().and_then(|d| d.error.clone());
        assert_eq!(error.as_deref(), Some("That doesn't look like an Epic authorization code."));
    }
}

#[cfg(test)]
mod shots;
#[cfg(test)]
mod memory_tests;
