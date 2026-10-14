// fastlaunch - a fast game launcher front end
// Copyright (C) 2026 marcin
// SPDX-License-Identifier: GPL-3.0-only

//! Memory stays bounded: hero art is evicted, nothing piles up while browsing.

use super::*;
use crate::library::Art;
use egui_kittest::Harness;

const GAMES: usize = 40;

fn many_games() -> Vec<Game> {
    (0..GAMES)
        .map(|i| Game {
            key: format!("steam:{i}"),
            name: format!("Game {i:02}"),
            source: Source::Steam,
            launch_uri: String::new(),
            art: Art { cover: None, hero: Some(PathBuf::from(format!("hero-{i}.jpg"))), logo: None },
            epic: None,
        })
        .collect()
}

/// A hero worker that answers instantly with a small image, like the real one in main.rs.
fn spawn_fake_hero_worker(art_tx: Sender<ArtMessage>) -> Sender<HeroRequest> {
    let (tx, rx) = mpsc::channel::<HeroRequest>();
    std::thread::spawn(move || {
        while let Ok((key, _path)) = rx.recv() {
            let image = egui::ColorImage::filled([64, 32], Color32::from_rgb(40, 60, 90));
            if art_tx.send(ArtMessage::Image(key, ArtKind::Hero, image)).is_err() {
                break;
            }
        }
    });
    tx
}

fn heroes_loaded(h: &Harness<'_, FastLaunch>) -> usize {
    h.state().art.iter().filter(|a| a.hero.is_some()).count()
}

fn textures(h: &Harness<'_, FastLaunch>) -> usize {
    h.ctx.tex_manager().read().num_allocated()
}

#[test]
fn browsing_keeps_hero_textures_bounded() {
    let (lib_tx, lib_rx) = mpsc::channel();
    let (art_tx, art_rx) = mpsc::channel();
    lib_tx.send(Loaded { games: many_games(), epic_account: None }).unwrap();
    let hero_tx = spawn_fake_hero_worker(art_tx);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 760.0))
        .with_max_steps(200)
        .build_eframe(move |cc| FastLaunch::new(cc, Instant::now(), lib_rx, art_rx).with_hero_worker(hero_tx));
    h.run_steps(5);

    let mut peak_heroes = 0;
    let mut textures_after_warmup = None;
    // Sweep right across all games and back, twice: every hero gets loaded at some point.
    for sweep in 0..4 {
        let key = if sweep % 2 == 0 { egui::Key::ArrowRight } else { egui::Key::ArrowLeft };
        for _ in 0..GAMES {
            h.key_press(key);
            h.run_steps(3);
            // Let the worker's answers arrive.
            std::thread::sleep(std::time::Duration::from_millis(1));
            h.run_steps(2);
            peak_heroes = peak_heroes.max(heroes_loaded(&h));
        }
        if sweep == 1 {
            textures_after_warmup = Some(textures(&h));
        }
    }
    assert!(peak_heroes <= HERO_CACHE, "at most {HERO_CACHE} hero textures, saw {peak_heroes}");
    let warm = textures_after_warmup.unwrap();
    let end = textures(&h);
    assert!(end <= warm, "texture count grew while browsing: {warm} -> {end}");
    assert!(
        h.state().art.iter().all(|a| !a.hero_pending || a.hero.is_none()),
        "no request left dangling for a loaded hero"
    );
}
