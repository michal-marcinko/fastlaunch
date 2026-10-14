// fastlaunch - a fast game launcher front end
// Copyright (C) 2026 marcin
// SPDX-License-Identifier: GPL-3.0-only

//! Reads installed Steam and Epic games straight from local files.
//! No network, no login: everything here is a handful of small file reads.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Steam,
    Epic,
}

#[derive(Clone, Debug)]
pub struct Game {
    /// Stable key, e.g. `steam:431960` or `epic:Fortnite`.
    pub key: String,
    pub name: String,
    pub source: Source,
    pub launch_uri: String,
    /// Artwork found on disk.
    pub art: Art,
    /// What's needed to start an Epic game without the Epic Games Launcher.
    pub epic: Option<EpicInstall>,
}

/// Local artwork files. All optional: tiles must look good without any of them.
#[derive(Clone, Debug, Default)]
pub struct Art {
    /// Portrait 2:3 cover.
    pub cover: Option<PathBuf>,
    /// Wide key art for backgrounds (Steam: 3840x1240, Epic: 2560x1440).
    pub hero: Option<PathBuf>,
    /// Transparent game logo (PNG). Often missing for Epic games.
    pub logo: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ArtKind {
    Cover,
    Hero,
    Logo,
}

impl Art {
    pub fn get(&self, kind: ArtKind) -> Option<&PathBuf> {
        match kind {
            ArtKind::Cover => self.cover.as_ref(),
            ArtKind::Hero => self.hero.as_ref(),
            ArtKind::Logo => self.logo.as_ref(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct EpicInstall {
    pub app_name: String,
    pub namespace: String,
    pub catalog_item_id: String,
    pub install_location: PathBuf,
    pub launch_executable: String,
    pub launch_command: String,
    pub requires_ownership_token: bool,
}

impl EpicInstall {
    pub fn executable_path(&self) -> Option<PathBuf> {
        let exe = self.launch_executable.trim_start_matches(['/', '\\']);
        (!exe.is_empty()).then(|| self.install_location.join(exe))
    }
}

pub fn scan() -> Vec<Game> {
    let art_dir = art_override_dir();
    let mut games = steam_games(art_dir.as_deref());
    games.extend(epic_games(art_dir.as_deref()));
    games.sort_by_key(|g| g.name.to_lowercase());
    games
}

/// `%APPDATA%\fastlaunch\art\<id>.jpg|png` overrides any cover art.
/// `<id>` is the Steam app id or the Epic AppName (e.g. `Fortnite`).
pub fn art_override_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("fastlaunch").join("art"))
}

/// Artwork fastlaunch downloaded itself (Epic key art), one folder per game:
/// `%LOCALAPPDATA%\fastlaunch\art-cache\epic\<AppName>\{cover,hero,logo}.<ext>`.
pub fn epic_art_cache_dir(app_name: &str) -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| {
        PathBuf::from(p).join("fastlaunch").join("art-cache").join("epic").join(app_name)
    })
}

fn cached_art(dir: Option<&Path>, stem: &str) -> Option<PathBuf> {
    let dir = dir?;
    ["jpg", "jpeg", "png"]
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .find(|p| p.is_file())
}

fn override_art(art_dir: Option<&Path>, id: &str) -> Option<PathBuf> {
    let dir = art_dir?;
    ["jpg", "jpeg", "png"]
        .iter()
        .map(|ext| dir.join(format!("{id}.{ext}")))
        .find(|p| p.is_file())
}

// ---------------------------------------------------------------- Steam

const STEAM_NON_GAMES: &[&str] = &["228980"]; // Steamworks Common Redistributables

fn steam_root() -> Option<PathBuf> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey(r"Software\Valve\Steam")
        && let Ok(path) = key.get_value::<String, _>("SteamPath")
    {
        return Some(PathBuf::from(path));
    }
    let default = PathBuf::from(r"C:\Program Files (x86)\Steam");
    default.is_dir().then_some(default)
}

fn steam_games(art_dir: Option<&Path>) -> Vec<Game> {
    let Some(root) = steam_root() else {
        return Vec::new();
    };
    let mut libraries = vec![root.clone()];
    if let Ok(text) = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf")) {
        let vdf = vdf::parse(&text);
        if let Some(folders) = vdf.get("libraryfolders") {
            for (_, folder) in folders.entries() {
                if let Some(path) = folder.get("path").and_then(vdf::Value::as_str) {
                    let path = PathBuf::from(path);
                    if !libraries.iter().any(|l| same_path(l, &path)) {
                        libraries.push(path);
                    }
                }
            }
        }
    }

    let cache = root.join("appcache").join("librarycache");
    let mut games = Vec::new();
    for lib in libraries {
        let Ok(dir) = std::fs::read_dir(lib.join("steamapps")) else {
            continue;
        };
        for entry in dir.flatten() {
            let path = entry.path();
            let is_manifest = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("appmanifest_") && n.ends_with(".acf"));
            if !is_manifest {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let vdf = vdf::parse(&text);
            let Some(state) = vdf.get("AppState") else {
                continue;
            };
            let (Some(appid), Some(name)) = (
                state.get("appid").and_then(vdf::Value::as_str),
                state.get("name").and_then(vdf::Value::as_str),
            ) else {
                continue;
            };
            if STEAM_NON_GAMES.contains(&appid) || is_steam_runtime(name) {
                continue;
            }
            games.push(Game {
                key: format!("steam:{appid}"),
                name: name.to_owned(),
                source: Source::Steam,
                launch_uri: format!("steam://rungameid/{appid}"),
                art: steam_art(&cache, appid, art_dir),
                epic: None,
            });
        }
    }
    games
}

fn is_steam_runtime(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("redistributable") || n.starts_with("proton") || n.starts_with("steam linux runtime")
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    norm(a) == norm(b)
}

/// Steam keeps artwork in `appcache/librarycache/<appid>/`, sometimes one
/// hash-named folder deeper.
fn steam_art(cache: &Path, appid: &str, art_dir: Option<&Path>) -> Art {
    let found = steam_cache_files(cache, appid);
    let pick = |names: &[&str]| {
        names.iter().find_map(|want| {
            found
                .iter()
                .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(want))
                .cloned()
        })
    };
    Art {
        cover: override_art(art_dir, appid).or_else(|| {
            pick(&["library_600x900.jpg", "library_capsule.jpg", "library_header.jpg", "header.jpg"])
        }),
        hero: pick(&["library_hero.jpg"]),
        logo: pick(&["logo.png"]),
    }
}

fn steam_cache_files(cache: &Path, appid: &str) -> Vec<PathBuf> {
    let dir = cache.join(appid);
    fn collect(d: &Path, found: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(d) {
            found.extend(rd.flatten().map(|e| e.path()));
        }
    }
    let mut found: Vec<PathBuf> = Vec::new();
    collect(&dir, &mut found);
    let subdirs: Vec<PathBuf> = found.iter().filter(|p| p.is_dir()).cloned().collect();
    for sub in &subdirs {
        collect(sub, &mut found);
    }
    found
}

// ---------------------------------------------------------------- Epic

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EpicItem {
    display_name: String,
    app_name: String,
    #[serde(default)]
    main_game_app_name: String,
    catalog_namespace: String,
    catalog_item_id: String,
    #[serde(default, rename = "bIsIncompleteInstall")]
    incomplete: bool,
    #[serde(default)]
    app_categories: Vec<String>,
    #[serde(default)]
    install_location: PathBuf,
    #[serde(default)]
    launch_executable: String,
    #[serde(default)]
    launch_command: String,
    /// `"true"` / `"false"` as a string in Epic's manifests.
    #[serde(default)]
    ownership_token: String,
}

fn epic_games(art_dir: Option<&Path>) -> Vec<Game> {
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    let manifests = program_data.join(r"Epic\EpicGamesLauncher\Data\Manifests");
    let Ok(dir) = std::fs::read_dir(manifests) else {
        return Vec::new();
    };
    let mut games = Vec::new();
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("item") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(item) = serde_json::from_slice::<EpicItem>(&bytes) else {
            continue;
        };
        let is_dlc = !item.main_game_app_name.is_empty() && item.main_game_app_name != item.app_name;
        let is_game = item.app_categories.iter().any(|c| c == "games");
        if item.incomplete || is_dlc || !is_game {
            continue;
        }
        games.push(Game {
            key: format!("epic:{}", item.app_name),
            art: {
                let cache = epic_art_cache_dir(&item.app_name);
                Art {
                    cover: override_art(art_dir, &item.app_name)
                        .or_else(|| cached_art(cache.as_deref(), "cover")),
                    hero: cached_art(cache.as_deref(), "hero"),
                    logo: cached_art(cache.as_deref(), "logo"),
                }
            },
            launch_uri: format!(
                "com.epicgames.launcher://apps/{}%3A{}%3A{}?action=launch&silent=true",
                item.catalog_namespace, item.catalog_item_id, item.app_name
            ),
            name: item.display_name,
            source: Source::Epic,
            epic: Some(EpicInstall {
                app_name: item.app_name,
                namespace: item.catalog_namespace,
                catalog_item_id: item.catalog_item_id,
                install_location: item.install_location,
                launch_executable: item.launch_executable,
                launch_command: item.launch_command,
                requires_ownership_token: item.ownership_token.eq_ignore_ascii_case("true"),
            }),
        });
    }
    games
}

// ---------------------------------------------------------------- VDF

/// Minimal parser for Valve's KeyValues text format (`"key" "value"` and
/// `"key" { ... }`). Enough for `libraryfolders.vdf` and `appmanifest_*.acf`.
mod vdf {
    pub enum Value {
        Str(String),
        Obj(Vec<(String, Value)>),
    }

    impl Value {
        pub fn get(&self, key: &str) -> Option<&Value> {
            match self {
                Value::Obj(entries) => entries
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(key))
                    .map(|(_, v)| v),
                Value::Str(_) => None,
            }
        }

        pub fn entries(&self) -> &[(String, Value)] {
            match self {
                Value::Obj(entries) => entries,
                Value::Str(_) => &[],
            }
        }

        pub fn as_str(&self) -> Option<&str> {
            match self {
                Value::Str(s) => Some(s),
                Value::Obj(_) => None,
            }
        }
    }

    enum Token {
        Str(String),
        Open,
        Close,
    }

    fn tokenize(text: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' => tokens.push(Token::Open),
                '}' => tokens.push(Token::Close),
                '"' => {
                    let mut s = String::new();
                    while let Some(c) = chars.next() {
                        match c {
                            '"' => break,
                            '\\' => {
                                if let Some(next) = chars.next() {
                                    s.push(match next {
                                        'n' => '\n',
                                        't' => '\t',
                                        other => other,
                                    });
                                }
                            }
                            other => s.push(other),
                        }
                    }
                    tokens.push(Token::Str(s));
                }
                '/' if chars.peek() == Some(&'/') => {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        tokens
    }

    pub fn parse(text: &str) -> Value {
        let tokens = tokenize(text);
        let mut pos = 0;
        Value::Obj(parse_entries(&tokens, &mut pos))
    }

    fn parse_entries(tokens: &[Token], pos: &mut usize) -> Vec<(String, Value)> {
        let mut entries = Vec::new();
        while *pos < tokens.len() {
            let key = match &tokens[*pos] {
                Token::Str(s) => s.clone(),
                Token::Close => {
                    *pos += 1;
                    return entries;
                }
                Token::Open => {
                    *pos += 1;
                    continue;
                }
            };
            *pos += 1;
            match tokens.get(*pos) {
                Some(Token::Str(v)) => {
                    entries.push((key, Value::Str(v.clone())));
                    *pos += 1;
                }
                Some(Token::Open) => {
                    *pos += 1;
                    entries.push((key, Value::Obj(parse_entries(tokens, pos))));
                }
                _ => break,
            }
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::vdf;

    #[test]
    fn parses_libraryfolders() {
        let text = r#""libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"apps" { "228980" "171517476" }
	}
	"1" { "path" "D:\\SteamLibrary" }
}"#;
        let v = vdf::parse(text);
        let paths: Vec<&str> = v
            .get("libraryfolders")
            .unwrap()
            .entries()
            .iter()
            .filter_map(|(_, f)| f.get("path").and_then(vdf::Value::as_str))
            .collect();
        assert_eq!(paths, [r"C:\Program Files (x86)\Steam", r"D:\SteamLibrary"]);
    }
}
