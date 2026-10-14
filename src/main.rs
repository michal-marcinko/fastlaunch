// fastlaunch - a fast game launcher front end
// Copyright (C) 2026 marcin
// SPDX-License-Identifier: GPL-3.0-only

// Hide the console window in Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod credstore;
mod epic;
mod library;
mod steam;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use eframe::egui;
use library::ArtKind;

/// Cover art is decoded at this size (2:3), off the UI thread.
const COVER_W: u32 = 400;
const COVER_H: u32 = 600;
/// Hero art is scaled to fit this box, keeping its aspect ratio. Big enough
/// to stay sharp as a full-window backdrop at 1920x1080.
const HERO_MAX: u32 = 1920;
/// Logos are scaled to fit this box, keeping their aspect ratio.
const LOGO_MAX: (u32, u32) = (800, 400);
const ART_THREADS: usize = 4;

/// Shown for games that have no cover anywhere.
static FALLBACK_COVER: &[u8] = include_bytes!("../assets/cover-fallback-600x900.jpg");
/// 64x64 RGBA window icon, pre-decoded so startup does no PNG work.
static WINDOW_ICON: &[u8] = include_bytes!("../assets/icon-64.rgba");

/// Messages from the art threads to the UI thread.
pub enum ArtMessage {
    /// A decoded image to upload.
    Image(String, library::ArtKind, egui::ColorImage),
    /// A game's hero art is now on disk (downloaded after startup). Heroes are
    /// decoded only when the UI asks for them, see [`HeroRequest`].
    HeroPath(String, PathBuf),
}

/// The UI asks the hero worker to decode one game's hero art: (game key, path).
pub type HeroRequest = (String, PathBuf);

fn main() -> eframe::Result {
    let start = Instant::now();

    // Read the library while the window and GPU are still starting up.
    let (lib_tx, lib_rx) = mpsc::channel();
    let (art_tx, art_rx) = mpsc::channel();
    let (hero_tx, hero_rx) = mpsc::channel::<HeroRequest>();
    let ui_ctx: Arc<OnceLock<egui::Context>> = Arc::default();
    {
        let ui_ctx = Arc::clone(&ui_ctx);
        let art_tx = art_tx.clone();
        std::thread::spawn(move || load_everything(&lib_tx, &art_tx, &ui_ctx));
    }
    {
        // Decodes hero art on request; ends when the app (holding `hero_tx`) is gone.
        let ui_ctx = Arc::clone(&ui_ctx);
        std::thread::spawn(move || {
            while let Ok((key, path)) = hero_rx.recv() {
                if let Some(image) = decode_art(&path, ArtKind::Hero) {
                    if art_tx.send(ArtMessage::Image(key, ArtKind::Hero, image)).is_err() {
                        break;
                    }
                    if let Some(ctx) = ui_ctx.get() {
                        ctx.request_repaint();
                    }
                }
            }
        });
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("fastlaunch")
            .with_inner_size([1280.0, 760.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(Arc::new(egui::IconData {
                rgba: WINDOW_ICON.to_vec(),
                width: 64,
                height: 64,
            })),
        // OpenGL starts ~2.5x faster than wgpu on Windows (~165 ms vs ~420 ms).
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "fastlaunch",
        options,
        Box::new(move |cc| {
            let _ = ui_ctx.set(cc.egui_ctx.clone());
            Ok(Box::new(app::FastLaunch::new(cc, start, lib_rx, art_rx).with_hero_worker(hero_tx).with_steam_prewarm()))
        }),
    )
}

fn load_everything(
    lib_tx: &Sender<app::Loaded>,
    art_tx: &Sender<ArtMessage>,
    ui_ctx: &Arc<OnceLock<egui::Context>>,
) {
    let wake = || {
        if let Some(ctx) = ui_ctx.get() {
            ctx.request_repaint();
        }
    };

    let games = library::scan();
    // Covers first (they are on screen first), then logos. Hero art is big, so
    // it is decoded only for the games the UI asks for (see the hero worker).
    let mut jobs: Vec<(String, ArtKind, PathBuf)> = Vec::new();
    for kind in [ArtKind::Cover, ArtKind::Logo] {
        for g in &games {
            if let Some(path) = g.art.get(kind) {
                jobs.push((g.key.clone(), kind, path.clone()));
            }
        }
    }
    // Epic games with no artwork yet: fetch it once from Epic's public catalog.
    let missing_epic: Vec<(String, library::EpicInstall)> = games
        .iter()
        .filter(|g| g.art.cover.is_none() || g.art.hero.is_none())
        .filter_map(|g| g.epic.clone().map(|e| (g.key.clone(), e)))
        .collect();
    let no_cover: Vec<String> = games
        .iter()
        .filter(|g| g.art.cover.is_none() && g.epic.is_none())
        .map(|g| g.key.clone())
        .collect();
    let epic_account = epic::load_account();
    let _ = lib_tx.send(app::Loaded { games, epic_account });
    wake();

    let chunk = jobs.len().div_ceil(ART_THREADS).max(1);
    std::thread::scope(|s| {
        for part in jobs.chunks(chunk) {
            s.spawn(move || {
                for (key, kind, path) in part {
                    if let Some(image) = decode_art(path, *kind) {
                        let _ = art_tx.send(ArtMessage::Image(key.clone(), *kind, image));
                        wake();
                    }
                }
            });
        }
    });

    let mut no_cover = no_cover;
    if !missing_epic.is_empty() {
        let epic = epic::Epic::new();
        for (key, install) in missing_epic {
            let files = library::epic_art_cache_dir(&install.app_name)
                .and_then(|dir| epic.download_art(&install, &dir).ok())
                .unwrap_or_default();
            let mut got_cover = false;
            for path in files {
                let kind = match path.file_stem().and_then(|s| s.to_str()) {
                    Some("cover") => ArtKind::Cover,
                    Some("hero") => {
                        let _ = art_tx.send(ArtMessage::HeroPath(key.clone(), path));
                        wake();
                        continue;
                    }
                    Some("logo") => ArtKind::Logo,
                    _ => continue,
                };
                if let Some(image) = decode_art(&path, kind) {
                    got_cover |= kind == ArtKind::Cover;
                    let _ = art_tx.send(ArtMessage::Image(key.clone(), kind, image));
                    wake();
                }
            }
            if !got_cover {
                no_cover.push(key);
            }
        }
    }

    if !no_cover.is_empty()
        && let Some(image) = decode_cover_bytes(FALLBACK_COVER)
    {
        for key in no_cover {
            let _ = art_tx.send(ArtMessage::Image(key, ArtKind::Cover, image.clone()));
        }
        wake();
    }
}

fn decode_cover_bytes(bytes: &[u8]) -> Option<egui::ColorImage> {
    use image::imageops::FilterType::Triangle;
    let image = image::load_from_memory(bytes).ok()?.resize_to_fill(COVER_W, COVER_H, Triangle).to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}

fn decode_art(path: &Path, kind: ArtKind) -> Option<egui::ColorImage> {
    use image::imageops::FilterType::Triangle;
    let image = image::open(path).ok()?;
    let image = match kind {
        ArtKind::Cover => image.resize_to_fill(COVER_W, COVER_H, Triangle),
        ArtKind::Hero if image.width() > HERO_MAX => image.resize(HERO_MAX, HERO_MAX, Triangle),
        ArtKind::Logo if image.width() > LOGO_MAX.0 || image.height() > LOGO_MAX.1 => {
            image.resize(LOGO_MAX.0, LOGO_MAX.1, Triangle)
        }
        _ => image,
    }
    .to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}
