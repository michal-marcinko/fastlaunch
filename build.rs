// fastlaunch - a fast game launcher front end
// Copyright (C) 2026 marcin
// SPDX-License-Identifier: GPL-3.0-only

// Embeds the app icon into the .exe (Explorer, taskbar pins, shortcuts).
fn main() {
    println!("cargo:rerun-if-changed=assets/fastlaunch.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    embed_resource::compile("assets/fastlaunch.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("embedding the app icon");
}
