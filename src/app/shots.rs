// fastlaunch - a fast game launcher front end
// Copyright (C) 2026 marcin
// SPDX-License-Identifier: GPL-3.0-only

//! Renders the launcher with the real library on this PC into `shots/`.
//! `cargo test --release shots -- --ignored --nocapture`

use super::*;
use egui_kittest::{Harness, kittest::Queryable};

fn shots_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("shots");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The real library and all its artwork, decoded before the first frame.
fn real_harness(size: Vec2) -> Harness<'static, FastLaunch> {
    let games = crate::library::scan();
    let (lib_tx, lib_rx) = mpsc::channel();
    let (art_tx, art_rx) = mpsc::channel();
    std::thread::scope(|s| {
        for g in &games {
            for kind in [ArtKind::Cover, ArtKind::Hero, ArtKind::Logo] {
                if let Some(path) = g.art.get(kind) {
                    let tx = art_tx.clone();
                    let key = g.key.clone();
                    s.spawn(move || {
                        if let Some(image) = crate::decode_art(path, kind) {
                            tx.send(crate::ArtMessage::Image(key, kind, image)).unwrap();
                        }
                    });
                }
            }
        }
    });
    let epic_account = crate::epic::load_account();
    lib_tx.send(Loaded { games, epic_account }).unwrap();
    let mut harness = Harness::builder()
        .with_size(size)
        .with_max_steps(40)
        .build_eframe(move |cc| FastLaunch::new(cc, Instant::now(), lib_rx, art_rx));
    harness.run();
    // Screenshots show the time a real start takes on this PC, not the test's.
    harness.state_mut().ready_ms = Some(153);
    settle(&mut harness);
    harness
}

fn settle(harness: &mut Harness<'_, FastLaunch>) {
    harness.run_steps(12);
}

fn save(harness: &mut Harness<'_, FastLaunch>, name: &str) {
    let path = shots_dir().join(format!("{name}.png"));
    harness.render().expect("needs a graphics adapter").save(&path).unwrap();
    eprintln!("{}", path.display());
}

#[test]
#[ignore = "renders PNGs of the real library for review"]
fn shots() {
    let mut h = real_harness(vec2(1920.0, 1080.0));
    save(&mut h, "default_1920");

    let mut h = real_harness(vec2(1280.0, 760.0));
    save(&mut h, "default_1280");

    // Hover a cover further along: it takes focus and its art crossfades in.
    let target = h.get_by_label("Rocket League®").rect().center();
    h.hover_at(target - vec2(5.0, 0.0));
    h.step();
    h.hover_at(target);
    settle(&mut h);
    save(&mut h, "hover_1280");

    // Mid-crossfade, from the keyboard.
    h.key_press(Key::ArrowLeft);
    h.step();
    save(&mut h, "crossfade_1280");
    settle(&mut h);

    let mut h = real_harness(vec2(1280.0, 760.0));
    h.input_mut().events.push(egui::Event::Text("ro".to_owned()));
    settle(&mut h);
    save(&mut h, "search_ro_1280");

    let mut h = real_harness(vec2(1280.0, 760.0));
    h.input_mut().events.push(egui::Event::Text("zzz".to_owned()));
    settle(&mut h);
    save(&mut h, "no_match_1280");

    let mut h = real_harness(vec2(1280.0, 760.0));
    let label = format!("Epic {}", h.state().counts[2]);
    h.get_by_label(&label).click();
    settle(&mut h);
    save(&mut h, "filter_epic_1280");

    let mut h = real_harness(vec2(1280.0, 760.0));
    h.state_mut().sign_in = Some(SignInDialog::default());
    settle(&mut h);
    save(&mut h, "sign_in_1280");

    let mut h = real_harness(vec2(1280.0, 760.0));
    h.state_mut().status = Some(Status::Launching("Started Fortnite directly, no Epic Games Launcher".to_owned()));
    h.key_press(Key::ArrowRight);
    settle(&mut h);
    save(&mut h, "launched_1280");

    let mut h = real_harness(vec2(640.0, 400.0));
    save(&mut h, "min_640");

    let mut h = real_harness(vec2(1920.0, 1080.0));
    h.state_mut().ready_at = h.ctx.input(|i| i.time) - 0.25;
    h.step();
    save(&mut h, "countup_1920");
}

/// One 1920x1080 frame per game, for marketing images.
/// `FASTLAUNCH_SHOT_MS` sets the ready time shown (default 174, the measured median).
#[test]
#[ignore = "renders marketing frames of the real library"]
fn marketing_frames() {
    let ms = std::env::var("FASTLAUNCH_SHOT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(174);
    let mut h = real_harness(vec2(1920.0, 1080.0));
    h.state_mut().ready_ms = Some(ms);
    // Public images: don't show the real Epic account name.
    if h.state().epic_name.is_some() {
        h.state_mut().epic_name = Some("Player".to_owned());
    }
    let count = h.state().visible.len();
    for i in 0..count {
        if i > 0 {
            h.key_press(Key::ArrowRight);
        }
        h.run_steps(40);
        save(&mut h, &format!("marketing_{i}"));
    }
}
