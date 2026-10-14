<p align="center">
  <img src="docs/hero.jpg" alt="fastlaunch open on Fortnite, with a Play button and a 'Ready in 174 ms' badge" width="100%">
</p>

<h1 align="center">fastlaunch</h1>

<p align="center">
  <b>A game launcher that opens in 0.17 seconds.</b><br>
  Your Steam and Epic games, one Play button, nothing else.
</p>

<p align="center">
  <a href="https://github.com/michal-marcinko/fastlaunch/releases/latest"><img alt="Download" src="https://img.shields.io/badge/download-Windows-c6ff3d?style=for-the-badge&labelColor=07080c"></a>
  <img alt="Cold start 174 ms" src="https://img.shields.io/badge/cold%20start-174%20ms-c6ff3d?style=for-the-badge&labelColor=07080c">
  <img alt="Written in Rust" src="https://img.shields.io/badge/rust-egui-c6ff3d?style=for-the-badge&labelColor=07080c">
  <a href="LICENSE"><img alt="License: GPL-3.0" src="https://img.shields.io/badge/license-GPL--3.0-c6ff3d?style=for-the-badge&labelColor=07080c"></a>
</p>

<p align="center">
  <a href="#benchmarks">Benchmarks</a> ·
  <a href="#how-epic-games-start-without-the-epic-games-launcher">How it works</a> ·
  <a href="#build-it-yourself">Build</a> ·
  <a href="#faq">FAQ</a>
</p>

---

Epic recently shipped a new launcher and announced it with:

> We vibe coded the Epic Games Launcher to be faster than the other guy.

It cold-starts in 5.47 seconds, which Epic says is 46% faster than Steam.

We read that and thought: we can vibe code too.

<p align="center">
  <img src="docs/chart.png" alt="Bar chart: fastlaunch 0.17 s, Epic Games Launcher 5.47 s, Steam 10.07 s. fastlaunch is 31 times faster than the new Epic Games Launcher." width="100%">
</p>

To be fair to Epic, their launcher has a store, downloads, a friends list and plenty more that fastlaunch doesn't. fastlaunch is a front end. It finds the games you already installed and starts them. That's the whole job, and it turns out the job doesn't take five seconds.

## What it does

- **Finds your games.** It reads the Steam and Epic library files already on your PC. There's nothing to set up or point at.
- **Starts Epic games without the Epic Games Launcher.** Sign in once, and Play starts the game's own .exe with the same sign-in arguments the Epic launcher would pass. No launcher window, no store page, no waiting.
- **Has Steam ready for you.** Steam games need Steam running ([here's why](#why-steam-games-still-need-steam)), so fastlaunch starts Steam hidden in the tray as soon as it opens. By the time you've picked a game, Steam has finished loading.
- **Works from the keyboard.** Arrows to browse, Enter to play, start typing to search, Esc to clear.
- **Sits still when you do.** If nothing on screen is moving, it doesn't redraw. Idle CPU use is zero.

<p align="center">
  <img src="docs/library.jpg" alt="The fastlaunch library: full-window key art for Aimlabs, a row of game covers and a Play button" width="100%">
</p>

## What it doesn't do

- Sell you anything.
- Open on a store when you asked for your library. Epic's benchmark times how fast its launcher shows you the **store**. Ours times how fast fastlaunch shows you **your games**. We think that's the one you wanted.
- Download or update games. Install them with Steam or Epic as usual, and fastlaunch picks them up.
- Run on Mac or Linux. Epic's new launcher runs on Linux. Point to Epic.

## Benchmarks

| | Cold start | Measured by |
|---|---:|---|
| **fastlaunch** | **0.17 s** | Us: median of 10 cold starts, from the process starting to the library on screen |
| Epic Games Launcher (new) | 5.47 s | Epic (Keywords Studio, 13 PCs, cold start to store) |
| Steam | 10.07 s | Epic, same test |

**About the method:** these runs used different PCs, different methods and a different finish line. It's a meme with real numbers, not a lab result.

Our number is real, though, and you can check it on your own PC:

```powershell
$env:FASTLAUNCH_BENCH = "$env:TEMP\fastlaunch-ms.txt"
.\fastlaunch.exe          # opens, waits for the library, writes the time in ms, quits
Get-Content $env:TEMP\fastlaunch-ms.txt
```

**Where the 170 ms goes:** about 150 ms of it is Windows and the graphics driver creating an OpenGL context, before fastlaunch can draw anything. The library scan and art loading run on other threads during that wait. Link-time optimisation, removing accessibility support and preloading DLLs didn't speed it up any further. At this point the bottleneck is the driver.

## How Epic games start without the Epic Games Launcher

An Epic game doesn't need the launcher. It needs proof that you own it, and the launcher just passes that proof along. fastlaunch does the same:

1. You sign in once at epicgames.com and paste the code it gives you. The saved sign-in goes into **Windows Credential Manager**, not a file.
2. When you press Play, fastlaunch swaps that sign-in for a one-time exchange code.
3. It starts the game's .exe with that code (`-AUTH_PASSWORD=...`) and the same command-line arguments the Epic launcher uses. Games that need an ownership token get one too.

This launch path is a Rust port of the relevant parts of [Legendary](https://github.com/legendary-gl/legendary), the open-source Epic client, which worked all this out years ago. Thank you, Legendary.

If the direct start fails, fastlaunch hands the game to the Epic Games Launcher instead, so Play always does something.

## Why Steam games still need Steam

Epic games check ownership with Epic's servers themselves, so the launcher is just a middleman you can skip. Steam games are different: they talk to the **Steam app running on your PC** through `steam_api.dll`. Without it, most of them either quit or restart themselves through Steam.

The only way around that is to fake the Steam client, which is a cracking tool, not a launcher feature. So fastlaunch does the honest thing: it gets Steam running in the background early, so its 10-second start happens while you're still picking a game.

Turn this off with `FASTLAUNCH_NO_STEAM_PREWARM=1`.

## Build it yourself

You need Windows 10 or 11 and [Rust](https://rustup.rs) 1.95 or newer.

```powershell
git clone https://github.com/michal-marcinko/fastlaunch
cd fastlaunch
cargo build --release
.\target\release\fastlaunch.exe
```

It uses Rust and [egui](https://github.com/emilk/egui) with the OpenGL renderer, which opens about 2.5× faster than wgpu on Windows. The result is one 11 MB .exe.

| Environment variable | What it does |
|---|---|
| `FASTLAUNCH_BENCH=<file>` | Write the startup time in ms to `<file>`, then quit |
| `FASTLAUNCH_NO_STEAM_PREWARM=1` | Don't start Steam in the background |

## FAQ

**Was this vibe coded?**
Yes, with [Claude Code](https://claude.com/claude-code). Vibe coding was never the slow part. The slow part is everything that ships with the vibes.

**Is it really 31× faster than the Epic Games Launcher?**
On a chart, yes. In a fair test we don't know: different PCs, and a different finish line. What we can say is that it's ready before you let go of the mouse button.

**Can this get my Epic account banned?**
It signs in and launches games the same way Legendary has for years. It's still unofficial and Epic can change the rules, so use it at your own risk.

**Does it work with anti-cheat games?**
It started Rocket League directly on our PC. A game whose anti-cheat insists on the Epic launcher might refuse. If the direct start fails, fastlaunch opens the game through the Epic launcher instead.

**Why no store?**
Have you seen how long stores take to load?

## License

[GPL-3.0](LICENSE). The Epic launch code is ported from Legendary (GPL-3.0). The Geist font is under the [SIL Open Font License](assets/Geist-OFL.txt).

fastlaunch is not affiliated with, endorsed by or in any way approved of by Epic Games or Valve. Epic Games, the Epic Games Launcher and Fortnite are trademarks of Epic Games, Inc. Steam is a trademark of Valve Corporation. Game art belongs to its owners.
