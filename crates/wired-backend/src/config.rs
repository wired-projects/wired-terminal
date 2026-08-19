//! Runtime settings, resolved from the environment.
//!
//! This process hands an HTTP caller a live PTY running an agent CLI, so the
//! defaults here are the closed ones: loopback only, an explicit CORS
//! allow-list rather than `*`, and a token required the moment the socket
//! leaves localhost.

use std::collections::HashSet;
use std::fmt;

use rand::Rng;

/// Vite dev server, plus the origins a packaged Tauri window reports.
pub const DEFAULT_ORIGINS: [&str; 4] = [
    "http://localhost:5173",
    "http://127.0.0.1:5173",
    "tauri://localhost",
    "http://tauri.localhost",
];

const LOOPBACK: [&str; 4] = ["127.0.0.1", "::1", "localhost", ""];
const TRUTHY: [&str; 4] = ["1", "true", "yes", "on"];

/// The port everything documents. Still the first thing tried; no longer the
/// only thing, because a non-coder cannot be asked to free it.
pub const DEFAULT_PORT: u16 = 8000;

/// Refusing to start with a configuration that would expose the PTY.
#[derive(Debug)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}

pub fn flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(raw) => TRUTHY.contains(&raw.trim().to_ascii_lowercase().as_str()),
        Err(_) => default,
    }
}

pub fn int_env(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

/// Is this the desktop app rather than the server binary?
///
/// It changes desktop-only defaults such as moving off a busy port. The
/// desktop shell sets this before reading settings.
pub fn is_desktop() -> bool {
    std::env::var("WIRED_PROFILE")
        .map(|v| v.trim().eq_ignore_ascii_case("desktop"))
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub auth_token: String,
    pub allowed_origins: HashSet<String>,
    pub allow_any_origin: bool,
    /// The product premise is an unattended 24/7 agent, so the CLIs launch with
    /// their approval prompts pre-answered. `WIRED_AGENT_AUTO_APPROVE=0` turns
    /// that off — the agent then stops and waits (a `prompt` event on the stream).
    pub agent_auto_approve: bool,
    /// Bind the next free port when `port` is taken, instead of failing to
    /// start. Off for the server binary, whose port is somebody's firewall rule.
    pub port_fallback: bool,
    /// Write the transcript and the schedule store to disk. `Settings::default()`
    /// leaves it off so tests and embedders touch no user directory.
    pub persist: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8000,
            auth_token: String::new(),
            allowed_origins: DEFAULT_ORIGINS.iter().map(|s| s.to_string()).collect(),
            allow_any_origin: false,
            agent_auto_approve: true,
            port_fallback: false,
            persist: false,
        }
    }
}

impl Settings {
    pub fn is_loopback(&self) -> bool {
        LOOPBACK.contains(&self.host.as_str())
    }

    pub fn auth_required(&self) -> bool {
        !self.auth_token.is_empty()
    }

    pub fn origin_allowed(&self, origin: &str) -> bool {
        self.allow_any_origin || self.allowed_origins.contains(origin)
    }
}

/// Read settings from the environment, rejecting unsafe combinations.
///
/// Returns `ConfigError` when the API would be reachable off-host without a
/// token. That combination is remote code execution for anyone on the network,
/// so it has to be an explicit choice (`WIRED_ALLOW_INSECURE=1`), not a
/// forgotten variable.
pub fn load_settings() -> Result<Settings, ConfigError> {
    let stored = crate::settings_store::get();

    let host = std::env::var("WIRED_HOST")
        .unwrap_or_default()
        .trim()
        .to_string();
    let host = if host.is_empty() {
        "127.0.0.1".to_string()
    } else {
        host
    };
    // The token may live in the OS keychain, which is what lets a packaged app
    // be given one after it was built — `VITE_AUTH_TOKEN` never could.
    let token = crate::settings_store::string_or(
        "WIRED_AUTH_TOKEN",
        Some(crate::settings_store::auth_token()),
        "",
    );

    let raw_origins = std::env::var("WIRED_ALLOWED_ORIGINS")
        .unwrap_or_default()
        .trim()
        .to_string();
    let (allow_any, origins) = if raw_origins == "*" {
        (
            true,
            DEFAULT_ORIGINS.iter().map(|s| s.to_string()).collect(),
        )
    } else if !raw_origins.is_empty() {
        (
            false,
            raw_origins
                .split(',')
                .map(|o| o.trim().to_string())
                .filter(|o| !o.is_empty())
                .collect(),
        )
    } else {
        (
            false,
            DEFAULT_ORIGINS.iter().map(|s| s.to_string()).collect(),
        )
    };

    let desktop = is_desktop();
    let default_port = stored.port.unwrap_or(DEFAULT_PORT) as i64;

    let settings = Settings {
        host,
        port: int_env("WIRED_PORT", default_port).clamp(1, 65535) as u16,
        auth_token: token,
        allowed_origins: origins,
        allow_any_origin: allow_any,
        agent_auto_approve: crate::providers::auto_approve_enabled(),
        // Only the app moves off a busy port on its own: it can tell its own
        // window where to connect, and nobody wrote a firewall rule for it.
        port_fallback: flag("WIRED_PORT_FALLBACK", desktop),
        persist: flag("WIRED_PERSIST", true),
    };

    let exposed = !settings.is_loopback() && !settings.auth_required();
    if exposed && !flag("WIRED_ALLOW_INSECURE", false) {
        return Err(ConfigError(format!(
            "WIRED_HOST={} exposes the agent PTY beyond this machine, but \
WIRED_AUTH_TOKEN is unset — anyone who can reach the port could run commands as you.\n\
  Set a token:      WIRED_AUTH_TOKEN={}\n\
  Or bind locally:  WIRED_HOST=127.0.0.1\n\
  Or override:      WIRED_ALLOW_INSECURE=1",
            settings.host,
            suggest_token()
        )));
    }

    Ok(settings)
}

/// A ready-to-paste token for the error above — the same courtesy the Python
/// build offered, so the fix is copy/paste rather than a trip to the docs.
/// Also what the Settings screen hands out when someone turns auth on.
pub fn suggest_token() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut rng = rand::rng();
    (0..32)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}
