// fastlaunch - a fast game launcher front end
// Copyright (C) 2026 marcin
// SPDX-License-Identifier: GPL-3.0-only
//
// The Epic launch path in this file is a Rust port of parts of Legendary
// (https://github.com/legendary-gl/legendary), Copyright the Legendary
// contributors, licensed under GPL-3.0:
//   - legendary/api/egs.py: OAuth token, exchange code, ownership token, catalog
//   - legendary/core.py `get_launch_parameters`: the game's command line

//! Start Epic games directly, without the Epic Games Launcher.
//!
//! Flow: refresh token -> access token -> one-time exchange code, which the game
//! takes on its command line as proof of ownership (`-AUTH_PASSWORD=<code>`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::credstore;
use crate::library::EpicInstall;

// The Epic Games Launcher's own OAuth client, as used by Legendary.
const CLIENT_ID: &str = "34a02cf8f4414e29b15921876da36f9a";
/// `base64("34a02cf8f4414e29b15921876da36f9a:daafbccc737745039dffe53d94fc76cf")`
const CLIENT_BASIC: &str =
    "Basic MzRhMDJjZjhmNDQxNGUyOWIxNTkyMTg3NmRhMzZmOWE6ZGFhZmJjY2M3Mzc3NDUwMzlkZmZlNTNkOTRmYzc2Y2Y=";
const USER_AGENT: &str =
    "UELauncher/11.0.1-14907503+++Portal+Release-Live Windows/10.0.19041.1.256.64bit";

const OAUTH_HOST: &str = "https://account-public-service-prod03.ol.epicgames.com";
const CATALOG_HOST: &str = "https://catalog-public-service-prod06.ol.epicgames.com";
const ECOMMERCE_HOST: &str = "https://ecommerceintegration-public-service-ecomprod02.ol.epicgames.com";

const CRED_TARGET: &str = "fastlaunch/epic";
const LOCALE: &str = "en";

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub refresh_token: String,
    pub account_id: String,
    pub display_name: String,
}

#[derive(Debug)]
pub enum Error {
    NotSignedIn,
    /// The saved sign-in no longer works; the user must sign in again.
    SignInExpired(String),
    /// Could not reach Epic, or Epic returned something unexpected.
    Network(String),
    /// Signing in with a pasted code failed.
    BadCode(String),
    /// The game's files are missing or it would not start.
    Launch(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotSignedIn => write!(f, "not signed in to Epic"),
            Error::SignInExpired(why) => write!(f, "the Epic sign-in expired ({why})"),
            Error::Network(why) => write!(f, "could not reach Epic ({why})"),
            Error::BadCode(why) => write!(f, "{why}"),
            Error::Launch(why) => write!(f, "{why}"),
        }
    }
}

/// URL of Epic's sign-in page. After signing in, it shows JSON containing
/// `authorizationCode`, which the user pastes back into fastlaunch.
pub fn auth_url() -> String {
    let redirect =
        format!("https://www.epicgames.com/id/api/redirect?clientId={CLIENT_ID}&responseType=code");
    format!("https://www.epicgames.com/id/login?redirectUrl={}", percent_encode(&redirect))
}

pub fn load_account() -> Option<Account> {
    credstore::load(CRED_TARGET).and_then(|s| serde_json::from_str(&s).ok())
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: String,
    account_id: String,
    #[serde(rename = "displayName", default)]
    display_name: String,
}

#[derive(serde::Deserialize)]
struct EpicApiError {
    #[serde(rename = "errorCode", default)]
    error_code: String,
    #[serde(rename = "errorMessage", default)]
    error_message: String,
}

#[derive(Clone, Default)]
struct CatalogInfo {
    additional_command_line: Option<String>,
}

#[derive(Default)]
struct State {
    account: Option<Account>,
    access: Option<(String, Instant)>,
    /// App-only token (no user), enough for public catalog data such as artwork.
    anonymous: Option<(String, Instant)>,
    catalog: HashMap<String, CatalogInfo>,
}

pub struct Epic {
    agent: ureq::Agent,
    state: Mutex<State>,
}

impl Epic {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .user_agent(USER_AGENT)
            .build();
        Self { agent: ureq::Agent::new_with_config(config), state: Mutex::default() }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set_account(&self, account: Option<Account>) {
        let mut state = self.state();
        state.account = account;
        state.access = None;
    }

    /// Sign in with what the user pasted: either the bare authorization code or
    /// the whole JSON page. Returns the Epic display name.
    pub fn sign_in(&self, pasted: &str) -> Result<String, Error> {
        let code = extract_code(pasted).ok_or_else(|| {
            Error::BadCode("That doesn't look like an Epic authorization code.".to_owned())
        })?;
        let token = self
            .token_request(&[("grant_type", "authorization_code"), ("code", &code), ("token_type", "eg1")])
            .map_err(|e| match e {
                Error::SignInExpired(why) => Error::BadCode(format!(
                    "Epic rejected the code ({why}). Codes expire after a few minutes; open the sign-in page again and paste the new one."
                )),
                other => other,
            })?;
        let name = token.display_name.clone();
        self.store(token)?;
        Ok(name)
    }

    pub fn sign_out(&self) {
        credstore::delete(CRED_TARGET);
        self.set_account(None);
    }

    fn token_request(&self, form: &[(&str, &str)]) -> Result<TokenResponse, Error> {
        let mut response = self
            .agent
            .post(format!("{OAUTH_HOST}/account/api/oauth/token"))
            .header("Authorization", CLIENT_BASIC)
            .send_form(form.iter().copied())
            .map_err(|e| Error::Network(e.to_string()))?;
        let status = response.status();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| Error::Network(e.to_string()))?;
        if status.is_success() {
            return serde_json::from_str(&body).map_err(|e| Error::Network(e.to_string()));
        }
        if status.is_server_error() {
            return Err(Error::Network(format!("HTTP {status}")));
        }
        let err: EpicApiError = serde_json::from_str(&body).unwrap_or(EpicApiError {
            error_code: format!("HTTP {status}"),
            error_message: String::new(),
        });
        let why = if err.error_message.is_empty() { err.error_code } else { err.error_message };
        Err(Error::SignInExpired(why))
    }

    /// Save a fresh token: Epic rotates the refresh token on every use.
    fn store(&self, token: TokenResponse) -> Result<(), Error> {
        let mut state = self.state();
        let display_name = if token.display_name.is_empty() {
            state.account.as_ref().map(|a| a.display_name.clone()).unwrap_or_default()
        } else {
            token.display_name
        };
        let account = Account {
            refresh_token: token.refresh_token,
            account_id: token.account_id,
            display_name,
        };
        let json = serde_json::to_string(&account).map_err(|e| Error::Launch(e.to_string()))?;
        credstore::save(CRED_TARGET, &json)
            .map_err(|e| Error::Launch(format!("Could not save the Epic sign-in: {e}")))?;
        // Renew a minute early.
        let lifetime = Duration::from_secs(token.expires_in.saturating_sub(60));
        state.access = Some((token.access_token, Instant::now() + lifetime));
        state.account = Some(account);
        Ok(())
    }

    fn access_token(&self) -> Result<(String, Account), Error> {
        let refresh = {
            let state = self.state();
            let account = state.account.clone().ok_or(Error::NotSignedIn)?;
            if let Some((token, until)) = &state.access
                && Instant::now() < *until
            {
                return Ok((token.clone(), account));
            }
            account.refresh_token
        };
        let token = self
            .token_request(&[("grant_type", "refresh_token"), ("refresh_token", &refresh), ("token_type", "eg1")])
            .inspect_err(|e| {
                if matches!(e, Error::SignInExpired(_)) {
                    self.sign_out();
                }
            })?;
        let access = token.access_token.clone();
        self.store(token)?;
        let account = self.state().account.clone().ok_or(Error::NotSignedIn)?;
        Ok((access, account))
    }

    fn get_json(&self, url: &str, token: &str, query: &[(&str, &str)]) -> Result<serde_json::Value, Error> {
        let mut request = self.agent.get(url).header("Authorization", format!("bearer {token}"));
        for (k, v) in query {
            request = request.query(*k, *v);
        }
        let mut response = request.call().map_err(|e| Error::Network(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Error::Network(format!("HTTP {} from {url}", response.status())));
        }
        response.body_mut().read_json().map_err(|e| Error::Network(e.to_string()))
    }

    fn catalog_info(&self, token: &str, game: &EpicInstall) -> CatalogInfo {
        if let Some(info) = self.state().catalog.get(&game.app_name) {
            return info.clone();
        }
        let url = format!(
            "{CATALOG_HOST}/catalog/api/shared/namespace/{}/bulk/items",
            game.namespace
        );
        let query = [
            ("id", game.catalog_item_id.as_str()),
            ("includeDLCDetails", "true"),
            ("includeMainGameDetails", "true"),
            ("country", "US"),
            ("locale", LOCALE),
        ];
        // Optional extra arguments only; a failed lookup should not block the launch.
        let info = self
            .get_json(&url, token, &query)
            .map(|json| CatalogInfo {
                additional_command_line: json[&game.catalog_item_id]["customAttributes"]
                    ["AdditionalCommandLine"]["value"]
                    .as_str()
                    .map(str::to_owned),
            })
            .unwrap_or_default();
        self.state().catalog.insert(game.app_name.clone(), info.clone());
        info
    }

    fn ownership_token(&self, token: &str, account_id: &str, game: &EpicInstall) -> Result<PathBuf, Error> {
        let mut response = self
            .agent
            .post(format!(
                "{ECOMMERCE_HOST}/ecommerceintegration/api/public/platforms/EPIC/identities/{account_id}/ownershipToken"
            ))
            .header("Authorization", format!("bearer {token}"))
            .send_form([("nsCatalogItemId", format!("{}:{}", game.namespace, game.catalog_item_id))])
            .map_err(|e| Error::Network(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Error::Network(format!("ownership token: HTTP {}", response.status())));
        }
        let bytes = response.body_mut().read_to_vec().map_err(|e| Error::Network(e.to_string()))?;
        let dir = std::env::temp_dir().join("fastlaunch");
        std::fs::create_dir_all(&dir).map_err(|e| Error::Launch(e.to_string()))?;
        let path = dir.join(format!("{}{}.ovt", game.namespace, game.catalog_item_id));
        std::fs::write(&path, bytes).map_err(|e| Error::Launch(e.to_string()))?;
        Ok(path)
    }

    /// Start the game's executable directly, signed in as the user.
    pub fn launch(&self, game: &EpicInstall) -> Result<(), Error> {
        let exe = game.executable_path().ok_or_else(|| {
            Error::Launch("Epic did not record which file starts this game.".to_owned())
        })?;
        if !exe.is_file() {
            return Err(Error::Launch(format!("Game file not found: {}", exe.display())));
        }

        let (token, account) = self.access_token()?;
        let exchange = self.get_json(&format!("{OAUTH_HOST}/account/api/oauth/exchange"), &token, &[])?;
        let code = exchange["code"]
            .as_str()
            .ok_or_else(|| Error::Network("Epic returned no exchange code".to_owned()))?
            .to_owned();
        let catalog = self.catalog_info(&token, game);
        let ovt = if game.requires_ownership_token {
            Some(self.ownership_token(&token, &account.account_id, game)?)
        } else {
            None
        };

        let args = launch_args(game, &catalog, &code, &account, ovt.as_deref());
        let workdir = exe.parent().map(PathBuf::from).unwrap_or_else(|| game.install_location.clone());
        std::process::Command::new(&exe)
            .args(&args)
            .current_dir(workdir)
            .spawn()
            .map(drop)
            .map_err(|e| Error::Launch(format!("Could not start {}: {e}", exe.display())))
    }
}

impl Epic {
    fn anonymous_token(&self) -> Result<String, Error> {
        if let Some((token, until)) = &self.state().anonymous
            && Instant::now() < *until
        {
            return Ok(token.clone());
        }
        #[derive(serde::Deserialize)]
        struct AppToken {
            access_token: String,
            expires_in: u64,
        }
        let mut response = self
            .agent
            .post(format!("{OAUTH_HOST}/account/api/oauth/token"))
            .header("Authorization", CLIENT_BASIC)
            .send_form([("grant_type", "client_credentials"), ("token_type", "eg1")])
            .map_err(|e| Error::Network(e.to_string()))?;
        if !response.status().is_success() {
            return Err(Error::Network(format!("app token: HTTP {}", response.status())));
        }
        let token: AppToken = response.body_mut().read_json().map_err(|e| Error::Network(e.to_string()))?;
        let until = Instant::now() + Duration::from_secs(token.expires_in.saturating_sub(60));
        self.state().anonymous = Some((token.access_token.clone(), until));
        Ok(token.access_token)
    }

    /// Download the game's store artwork into `dir` as `cover.*`, `hero.*` and
    /// `logo.*`. Uses public catalog data; no user sign-in needed. Returns the
    /// files written.
    pub fn download_art(&self, game: &EpicInstall, dir: &std::path::Path) -> Result<Vec<PathBuf>, Error> {
        let token = self.anonymous_token()?;
        let url = format!("{CATALOG_HOST}/catalog/api/shared/namespace/{}/bulk/items", game.namespace);
        let json = self.get_json(
            &url,
            &token,
            &[("id", game.catalog_item_id.as_str()), ("country", "US"), ("locale", LOCALE)],
        )?;
        let images = json[&game.catalog_item_id]["keyImages"].as_array().cloned().unwrap_or_default();
        let find = |types: &[&str]| {
            types.iter().find_map(|t| {
                images
                    .iter()
                    .find(|i| i["type"].as_str() == Some(t))
                    .and_then(|i| i["url"].as_str())
                    .map(str::to_owned)
            })
        };
        let wanted = [
            ("cover", find(&["DieselGameBoxTall", "OfferImageTall", "Thumbnail"])),
            ("hero", find(&["DieselGameBox", "OfferImageWide", "DieselStoreFrontWide"])),
            ("logo", find(&["DieselGameBoxLogo"])),
        ];
        std::fs::create_dir_all(dir).map_err(|e| Error::Launch(e.to_string()))?;
        let mut written = Vec::new();
        for (stem, url) in wanted {
            let Some(url) = url else { continue };
            let mut response = self.agent.get(&url).call().map_err(|e| Error::Network(e.to_string()))?;
            if !response.status().is_success() {
                continue;
            }
            let bytes = response
                .body_mut()
                .with_config()
                .limit(32 * 1024 * 1024)
                .read_to_vec()
                .map_err(|e| Error::Network(e.to_string()))?;
            let ext = match image::guess_format(&bytes) {
                Ok(image::ImageFormat::Png) => "png",
                Ok(image::ImageFormat::Jpeg) => "jpg",
                _ => continue,
            };
            let path = dir.join(format!("{stem}.{ext}"));
            std::fs::write(&path, &bytes).map_err(|e| Error::Launch(e.to_string()))?;
            written.push(path);
        }
        Ok(written)
    }
}

/// The game's command line, in the order Legendary builds it:
/// manifest arguments, catalog arguments, then the Epic auth arguments.
fn launch_args(
    game: &EpicInstall,
    catalog: &CatalogInfo,
    code: &str,
    account: &Account,
    ovt: Option<&std::path::Path>,
) -> Vec<String> {
    let mut args = split_args(&game.launch_command);
    if let Some(extra) = &catalog.additional_command_line {
        args.extend(split_args(extra));
    }
    args.extend([
        "-AUTH_LOGIN=unused".to_owned(),
        format!("-AUTH_PASSWORD={code}"),
        "-AUTH_TYPE=exchangecode".to_owned(),
        format!("-epicapp={}", game.app_name),
        "-epicenv=Prod".to_owned(),
    ]);
    if let Some(path) = ovt {
        args.push(format!("-epicovt={}", path.display()));
    }
    args.extend([
        "-EpicPortal".to_owned(),
        format!("-epicusername={}", account.display_name),
        format!("-epicuserid={}", account.account_id),
        format!("-epiclocale={LOCALE}"),
        format!("-epicsandboxid={}", game.namespace),
    ]);
    args
}

/// Split a Windows-style argument string on whitespace, honouring double quotes.
fn split_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_arg = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_arg = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_arg {
                    args.push(std::mem::take(&mut current));
                    has_arg = false;
                }
            }
            c => {
                current.push(c);
                has_arg = true;
            }
        }
    }
    if has_arg {
        args.push(current);
    }
    args
}

/// Accepts the bare code, the code in quotes, or the whole JSON page Epic shows.
fn extract_code(pasted: &str) -> Option<String> {
    let pasted = pasted.trim();
    if pasted.starts_with('{') {
        let json: serde_json::Value = serde_json::from_str(pasted).ok()?;
        return json["authorizationCode"].as_str().map(str::to_owned);
    }
    let code = pasted.trim_matches('"');
    let valid = code.len() >= 16 && code.chars().all(|c| c.is_ascii_alphanumeric());
    valid.then(|| code.to_owned())
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fortnite() -> EpicInstall {
        EpicInstall {
            app_name: "Fortnite".to_owned(),
            namespace: "fn".to_owned(),
            catalog_item_id: "4fe75bbc5a674f4f9b356b5c90567da5".to_owned(),
            install_location: PathBuf::from(r"D:\Fortnite"),
            launch_executable: "FortniteGame/Binaries/Win64/FortniteBootstrapper.exe".to_owned(),
            launch_command: " -obfuscationid=abc".to_owned(),
            requires_ownership_token: false,
        }
    }

    #[test]
    fn launch_args_match_legendary_order() {
        let account = Account {
            refresh_token: String::new(),
            account_id: "acc123".to_owned(),
            display_name: "marcin".to_owned(),
        };
        let catalog = CatalogInfo { additional_command_line: Some("-extra \"a b\"".to_owned()) };
        let args = launch_args(&fortnite(), &catalog, "CODE", &account, None);
        assert_eq!(
            args,
            [
                "-obfuscationid=abc",
                "-extra",
                "a b",
                "-AUTH_LOGIN=unused",
                "-AUTH_PASSWORD=CODE",
                "-AUTH_TYPE=exchangecode",
                "-epicapp=Fortnite",
                "-epicenv=Prod",
                "-EpicPortal",
                "-epicusername=marcin",
                "-epicuserid=acc123",
                "-epiclocale=en",
                "-epicsandboxid=fn",
            ]
        );
    }

    #[test]
    fn extracts_code_from_json_page_or_bare_code() {
        let page = r#"{"warning":"Do not share this code","redirectUrl":"https://localhost/launcher/authorized?code=abc","authorizationCode":"0123456789abcdef0123456789abcdef","exchangeCode":null,"sid":null}"#;
        assert_eq!(extract_code(page).as_deref(), Some("0123456789abcdef0123456789abcdef"));
        assert_eq!(
            extract_code(" \"0123456789abcdef0123456789abcdef\" ").as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(extract_code("hello world"), None);
    }

    #[test]
    fn executable_path_joins_install_dir() {
        assert_eq!(
            fortnite().executable_path().unwrap(),
            PathBuf::from(r"D:\Fortnite").join("FortniteGame/Binaries/Win64/FortniteBootstrapper.exe")
        );
    }
}
