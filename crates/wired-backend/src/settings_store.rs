//! `settings.json` — the file behind the Settings screen.
//!
//! Environment variables still win over everything here, so `npm run start`,
//! `install-ubuntu.sh` and every documented `WIRED_*` variable behave exactly as
//! before. The store only answers the question the environment left open, which
//! is what lets a packaged app be configured by clicking rather than by editing
//! a shell profile it cannot see.
//!
//! Order of resolution, highest first:
//!   1. `WIRED_*` environment variable
//!   2. `settings.json`
//!   3. the built-in default
//!
//! Secrets are the exception: they prefer the OS keychain (`secrets.rs`) and
//! only fall back into this file, which is written 0600.

use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::paths;
use crate::secrets;

pub const CURRENT_VERSION: u32 = 1;

/// Keychain account names. Stable — renaming one orphans a stored secret.
const KEY_AUTH_TOKEN: &str = "auth-token";
const KEY_BOT_TOKEN: &str = "telegram-bot-token";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Telegram {
    pub enabled: bool,
    /// Only present when no keychain would take it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_token: Option<String>,
    /// Chats allowed to drive the agent. Populated by approving a pairing code.
    pub allowed_chats: Vec<i64>,
    /// Where unsolicited output goes: scheduled runs, approval requests.
    pub owner_chat: Option<i64>,
    /// Long-poll cursor, so a restart does not replay yesterday's messages.
    pub update_offset: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    pub version: u32,
    /// Set once the setup wizard has been completed, so it is not shown again.
    pub onboarded: bool,
    /// "claude" | "grok" | "codex" | "gemini"
    pub assistant: Option<String>,
    /// The one folder the agent may read and write.
    pub folder: Option<String>,
    /// Restart the CLI when it exits — the "always on" switch.
    pub always_on: Option<bool>,
    pub auto_start: Option<bool>,
    /// `true` = stop and ask before acting. The inverse of the CLI flags in
    /// `providers.rs`, phrased the way the Settings screen asks the question.
    pub ask_before_acting: Option<bool>,
    pub start_at_login: Option<bool>,
    pub notifications: Option<bool>,
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    pub telegram: Telegram,
}

impl Default for Stored {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            onboarded: false,
            assistant: None,
            folder: None,
            always_on: None,
            auto_start: None,
            ask_before_acting: None,
            start_at_login: None,
            notifications: None,
            port: None,
            auth_token: None,
            telegram: Telegram::default(),
        }
    }
}

fn cell() -> &'static RwLock<Stored> {
    static STORE: OnceLock<RwLock<Stored>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(read_from_disk()))
}

fn read_from_disk() -> Stored {
    let path = paths::settings_file();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Stored::default();
    };
    match serde_json::from_str::<Stored>(&raw) {
        Ok(stored) => stored,
        Err(e) => {
            // Never overwrite a file we could not understand: the user may have
            // hand-edited it, and silently resetting their configuration is a
            // worse outcome than running on defaults for one launch.
            tracing::error!("{} is not valid JSON ({e}); using defaults", path.display());
            Stored::default()
        }
    }
}

/// A snapshot of the current settings.
pub fn get() -> Stored {
    cell().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Mutate and persist in one step, returning the saved state.
pub fn update<F: FnOnce(&mut Stored)>(f: F) -> Result<Stored, String> {
    let snapshot = {
        let mut guard = cell().write().unwrap_or_else(|e| e.into_inner());
        f(&mut guard);
        guard.version = CURRENT_VERSION;
        guard.clone()
    };
    save(&snapshot)?;
    Ok(snapshot)
}

fn save(stored: &Stored) -> Result<(), String> {
    let body = serde_json::to_string_pretty(stored).map_err(|e| e.to_string())?;
    paths::write_private(&paths::settings_file(), &body).map_err(|e| {
        format!(
            "Could not save settings to {}: {e}",
            paths::settings_file().display()
        )
    })
}

// ── secrets ─────────────────────────────────────────────────────────────

/// Where a secret would be kept on this machine, for the diagnostics screen.
pub fn secret_backing() -> secrets::Backing {
    if secrets::available() {
        secrets::Backing::Keychain
    } else {
        secrets::Backing::File
    }
}

fn read_secret(key: &str, from_file: Option<String>) -> Option<String> {
    secrets::get(key)
        .or(from_file)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_secret<F>(key: &str, value: &str, put_in_file: F) -> Result<(), String>
where
    F: FnOnce(&mut Stored, Option<String>),
{
    let value = value.trim().to_string();
    if value.is_empty() {
        secrets::delete(key);
        update(|s| put_in_file(s, None))?;
        return Ok(());
    }
    // Keep exactly one copy: if the keychain took it, the file must not also
    // hold it, or rotating the secret would leave a stale credential behind.
    let stored_in_keychain = secrets::set(key, &value);
    update(|s| put_in_file(s, (!stored_in_keychain).then_some(value)))?;
    Ok(())
}

/// The API bearer token, or empty when the API is open (the loopback default).
pub fn auth_token() -> String {
    read_secret(KEY_AUTH_TOKEN, get().auth_token).unwrap_or_default()
}

pub fn set_auth_token(value: &str) -> Result<(), String> {
    write_secret(KEY_AUTH_TOKEN, value, |s, file| s.auth_token = file)
}

pub fn telegram_bot_token() -> String {
    read_secret(KEY_BOT_TOKEN, get().telegram.bot_token).unwrap_or_default()
}

pub fn set_telegram_bot_token(value: &str) -> Result<(), String> {
    write_secret(KEY_BOT_TOKEN, value, |s, file| s.telegram.bot_token = file)
}

// ── resolution helpers ──────────────────────────────────────────────────

/// `WIRED_*` if set, else the stored value, else the default.
pub fn flag_or(env: &str, stored: Option<bool>, default: bool) -> bool {
    if std::env::var_os(env).is_some() {
        return crate::config::flag(env, default);
    }
    stored.unwrap_or(default)
}

pub fn string_or(env: &str, stored: Option<String>, default: &str) -> String {
    let from_env = std::env::var(env).unwrap_or_default().trim().to_string();
    if !from_env.is_empty() {
        return from_env;
    }
    stored
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}
