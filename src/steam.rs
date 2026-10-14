// fastlaunch - a fast game launcher front end
// Copyright (C) 2026 marcin
// SPDX-License-Identifier: GPL-3.0-only

//! Keep Steam ready so Steam games open without Steam's cold start.
//!
//! Steam games talk to the local Steam client through `steam_api.dll`, so
//! unlike Epic games they cannot start without it. What we can do is start
//! Steam hidden (`-silent`, straight to the tray) as soon as fastlaunch opens,
//! so it has finished starting by the time Play is pressed.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

/// How long to wait for a Steam we started to sign in. Steam may update
/// itself first, which can take a while.
const START_TIMEOUT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Steam is not running; launching a game starts it first.
    NotRunning,
    /// Steam is running but not signed in yet.
    Starting,
    /// Steam is running and signed in: games start right away.
    Ready,
}

/// Steam's state, from what Steam writes to the registry, checked against the
/// live process (the values go stale if Steam crashes).
pub fn state() -> State {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam\ActiveProcess");
    let read = |name: &str| key.as_ref().ok().and_then(|k| k.get_value::<u32, _>(name).ok()).unwrap_or(0);
    classify(read("pid"), read("ActiveUser"), is_steam_process)
}

fn classify(pid: u32, active_user: u32, alive: impl Fn(u32) -> bool) -> State {
    if pid == 0 || !alive(pid) {
        State::NotRunning
    } else if active_user == 0 {
        State::Starting
    } else {
        State::Ready
    }
}

/// Start Steam hidden if it is not running, then report each state change
/// until it is ready. `report` returns `false` to stop early (the app closed).
pub fn prewarm(mut report: impl FnMut(State) -> bool) {
    let mut last = state();
    let started = last == State::NotRunning;
    if started {
        let Some(exe) = exe() else {
            report(last);
            return;
        };
        if std::process::Command::new(exe).arg("-silent").spawn().is_err() {
            report(last);
            return;
        }
        last = State::Starting;
    }
    if !report(last) {
        return;
    }
    let since = Instant::now();
    while last != State::Ready && since.elapsed() < START_TIMEOUT {
        std::thread::sleep(POLL);
        let now = state();
        // Steam we just started takes a moment to register itself.
        if started && now == State::NotRunning {
            continue;
        }
        if now != last {
            last = now;
            if !report(now) || now == State::NotRunning {
                return;
            }
        }
    }
}

fn exe() -> Option<PathBuf> {
    let steam = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam").ok()?;
    let exe = steam
        .get_value::<String, _>("SteamExe")
        .map(PathBuf::from)
        .or_else(|_| steam.get_value::<String, _>("SteamPath").map(|p| PathBuf::from(p).join("steam.exe")))
        .ok()?;
    exe.is_file().then_some(exe)
}

/// Whether `pid` is a live process whose executable is steam.exe (process
/// IDs are reused, so the number alone proves nothing).
fn is_steam_process(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    // SAFETY: plain Win32 calls on a handle we own and close; the buffer
    // length passed matches the buffer.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let alive = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE as u32;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let named = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(handle);
        alive
            && named
            && String::from_utf16_lossy(&buf[..len as usize]).to_lowercase().ends_with("\\steam.exe")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_needs_a_live_process() {
        assert_eq!(classify(0, 5, |_| true), State::NotRunning);
        assert_eq!(classify(42, 5, |_| false), State::NotRunning, "stale pid after a crash");
        assert_eq!(classify(42, 0, |_| true), State::Starting);
        assert_eq!(classify(42, 5, |_| true), State::Ready);
    }

    #[test]
    #[ignore = "reads this PC's Steam state"]
    fn print_state() {
        eprintln!("steam: {:?}, exe: {:?}", state(), exe());
    }
}
