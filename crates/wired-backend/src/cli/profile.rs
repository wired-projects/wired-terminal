//! Where the CLI points, and what it authenticates with.
//!
//! Saved servers live in `cli.json` next to the settings the app writes, in
//! JSON rather than something more hand-friendly so that the one config format
//! in this project stays the one config format.
//!
//! Finding the token is the fiddly part. On the server the API's token is in a
//! root-owned env file the service group can read; in the desktop app it is in
//! the keychain; in a dev shell it is an environment variable. All three are
//! tried, cheapest and least surprising first, so `wired status` just works
//! wherever it is run from.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use wired_backend::paths;

use super::args::Global;

/// Written by `install-ubuntu.sh`, read by systemd, and 0640 root:wired.
const SERVER_ENV_FILE: &str = "/etc/wired-terminal/wired.env";
/// The unit `install-ubuntu.sh` installs.
pub const DEFAULT_UNIT: &str = "wired-terminal";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Remote {
    /// `[user@]host` as ssh would take it.
    pub host: String,
    /// The API port on the far side of the tunnel.
    pub port: u16,
    pub ssh_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// The systemd unit to drive for start/stop/restart/logs.
    pub unit: Option<String>,
}

impl Default for Remote {
    fn default() -> Self {
        Remote {
            host: String::new(),
            port: 8000,
            ssh_port: None,
            token: None,
            unit: None,
        }
    }
}

impl Remote {
    pub fn unit(&self) -> &str {
        self.unit.as_deref().unwrap_or(DEFAULT_UNIT)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Used when `--remote` is not given. Absent means "this machine".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub remotes: BTreeMap<String, Remote>,
}

pub fn config_path() -> PathBuf {
    paths::config_dir().join("cli.json")
}

impl Config {
    pub fn load() -> Config {
        // A corrupt file must not stop `wired status` from telling you the
        // service is down — that is exactly when you are least able to fix it.
        std::fs::read_to_string(config_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // A remote's token is a password to a shell on that box, so this gets
        // the same 0600 treatment as the settings file.
        paths::write_private(&config_path(), &format!("{body}\n"))
            .map_err(|e| format!("could not write {}: {e}", config_path().display()))
    }
}

/// What a command is going to act on, once the flags and the config agree.
#[derive(Debug, Clone)]
pub enum Target {
    Local {
        base: String,
        token: String,
        unit: String,
    },
    Remote {
        name: String,
        remote: Remote,
        token: String,
    },
    /// `--url` given explicitly: talk HTTP, and admit there is no service to
    /// start or stop because we do not know what is on the other end.
    Url { base: String, token: String },
}

impl Target {
    pub fn describe(&self) -> String {
        match self {
            Target::Local { base, .. } => base.clone(),
            Target::Remote { name, remote, .. } => format!("{name} ({})", remote.host),
            Target::Url { base, .. } => base.clone(),
        }
    }

    pub fn token(&self) -> &str {
        match self {
            Target::Local { token, .. } | Target::Url { token, .. } => token,
            Target::Remote { token, .. } => token,
        }
    }
}

/// Resolve flags + config + environment into one target.
pub fn resolve(global: &Global, config: &Config) -> Result<Target, String> {
    // An explicit URL is a deliberate override and beats every default.
    if let Some(url) = &global.url {
        return Ok(Target::Url {
            base: normalize_base(url),
            token: global
                .token
                .clone()
                .or_else(env_token)
                .unwrap_or_else(local_token),
        });
    }

    let name = global.remote.clone().or_else(|| config.default.clone());
    if let Some(name) = name {
        let remote = config.remotes.get(&name).cloned().ok_or_else(|| {
            let known: Vec<&str> = config.remotes.keys().map(String::as_str).collect();
            if known.is_empty() {
                format!(
                    "no remote called `{name}` — add one with `wired remote add {name} user@host`"
                )
            } else {
                format!("no remote called `{name}` — known: {}", known.join(", "))
            }
        })?;
        let token = global
            .token
            .clone()
            .or_else(|| remote.token.clone())
            .unwrap_or_default();
        return Ok(Target::Remote {
            name,
            remote,
            token,
        });
    }

    Ok(Target::Local {
        base: local_base(),
        token: global
            .token
            .clone()
            .or_else(env_token)
            .unwrap_or_else(local_token),
        unit: DEFAULT_UNIT.to_string(),
    })
}

fn normalize_base(url: &str) -> String {
    let url = url.trim_end_matches('/');
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("http://{url}")
    }
}

/// Where the local API is listening, according to whoever configured it last.
pub fn local_base() -> String {
    let env = server_env();
    let host = std::env::var("WIRED_HOST")
        .ok()
        .or_else(|| env.get("WIRED_HOST").cloned())
        .unwrap_or_else(|| "127.0.0.1".into());
    let port = std::env::var("WIRED_PORT")
        .ok()
        .or_else(|| env.get("WIRED_PORT").cloned())
        .and_then(|p| p.parse::<u16>().ok())
        .or(wired_backend::settings_store::get().port)
        .unwrap_or(8000);

    // A wildcard bind is not an address you can connect to.
    let host = match host.as_str() {
        "0.0.0.0" | "::" | "" => "127.0.0.1".to_string(),
        "::1" => "[::1]".to_string(),
        other => other.to_string(),
    };
    format!("http://{host}:{port}")
}

fn env_token() -> Option<String> {
    std::env::var("WIRED_AUTH_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
}

/// The token for a server on this machine.
///
/// The env file comes first because on a server that is the truth the running
/// process was started with; the settings store is the desktop app's answer and
/// may reach for a keychain, which is slower and can prompt.
fn local_token() -> String {
    if let Some(token) = server_env().get("WIRED_AUTH_TOKEN") {
        if !token.is_empty() {
            return token.clone();
        }
    }
    wired_backend::settings_store::auth_token()
}

fn server_env() -> BTreeMap<String, String> {
    let path = std::env::var("WIRED_ENV_FILE").unwrap_or_else(|_| SERVER_ENV_FILE.to_string());
    std::fs::read_to_string(path)
        .map(|raw| parse_env(&raw))
        .unwrap_or_default()
}

/// Enough of the shell's syntax for a file systemd wrote and systemd reads.
fn parse_env(raw: &str) -> BTreeMap<String, String> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            (key.trim().to_string(), value.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_parsing_skips_comments_and_blank_lines() {
        let env = parse_env(
            "# Wired Terminal — read by systemd.\n\
             WIRED_HOST=127.0.0.1\n\
             \n\
             # WIRED_AUTH_TOKEN=   # required to bind off-loopback\n\
             WIRED_PORT=8000\n",
        );
        assert_eq!(env.get("WIRED_HOST").unwrap(), "127.0.0.1");
        assert_eq!(env.get("WIRED_PORT").unwrap(), "8000");
        assert!(!env.contains_key("WIRED_AUTH_TOKEN"));
    }

    #[test]
    fn env_parsing_unwraps_quotes() {
        let env = parse_env("WIRED_AUTH_TOKEN=\"s3cret\"\nWIRED_AGENT_CWD='/home/ubuntu'\n");
        assert_eq!(env.get("WIRED_AUTH_TOKEN").unwrap(), "s3cret");
        assert_eq!(env.get("WIRED_AGENT_CWD").unwrap(), "/home/ubuntu");
    }

    #[test]
    fn a_value_containing_equals_survives() {
        let env = parse_env("WIRED_AUTH_TOKEN=a=b=c\n");
        assert_eq!(env.get("WIRED_AUTH_TOKEN").unwrap(), "a=b=c");
    }

    #[test]
    fn bare_hosts_and_ports_become_urls() {
        assert_eq!(normalize_base("127.0.0.1:8000"), "http://127.0.0.1:8000");
        assert_eq!(normalize_base("http://box:8000/"), "http://box:8000");
        assert_eq!(normalize_base("https://box"), "https://box");
    }

    #[test]
    fn an_unknown_remote_lists_the_known_ones() {
        let mut config = Config::default();
        config.remotes.insert("pilot".into(), Remote::default());
        let global = Global {
            remote: Some("staging".into()),
            ..Default::default()
        };
        let err = resolve(&global, &config).unwrap_err();
        assert!(err.contains("pilot"), "{err}");
    }

    #[test]
    fn an_empty_config_suggests_adding_the_remote() {
        let global = Global {
            remote: Some("pilot".into()),
            ..Default::default()
        };
        let err = resolve(&global, &Config::default()).unwrap_err();
        assert!(err.contains("remote add pilot"), "{err}");
    }

    #[test]
    fn the_default_remote_applies_when_no_flag_is_given() {
        let mut config = Config::default();
        config.remotes.insert("pilot".into(), Remote::default());
        config.default = Some("pilot".into());
        match resolve(&Global::default(), &config).unwrap() {
            Target::Remote { name, .. } => assert_eq!(name, "pilot"),
            other => panic!("expected the default remote, got {other:?}"),
        }
    }

    #[test]
    fn an_explicit_url_beats_the_default_remote() {
        let mut config = Config::default();
        config.remotes.insert("pilot".into(), Remote::default());
        config.default = Some("pilot".into());
        let global = Global {
            url: Some("127.0.0.1:9000".into()),
            ..Default::default()
        };
        match resolve(&global, &config).unwrap() {
            Target::Url { base, .. } => assert_eq!(base, "http://127.0.0.1:9000"),
            other => panic!("expected a url target, got {other:?}"),
        }
    }
}
