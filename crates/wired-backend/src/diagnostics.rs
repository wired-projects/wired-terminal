//! `hermes doctor`, for Wired.
//!
//! Two things live here. **Checks** walk the chain — binary found, signed in,
//! folder writable, port free, bridge connected — and name the broken link,
//! each with one sentence and one thing to do about it. **The report** is the
//! block of text whoever helps him is going to ask for, ready to paste.
//!
//! Both exist so that the alternative to a working app is not a phone call.

use std::io::{Read, Seek, SeekFrom};
use std::net::{SocketAddr, TcpListener};
use std::path::Path;

use serde::Serialize;
use serde_json::{json, Value};

use crate::paths;
use crate::providers;
use crate::settings_store;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub label: String,
    /// `None` when we genuinely cannot tell, which is not the same as broken.
    pub ok: Option<bool>,
    pub detail: String,
    /// What the button next to this row should do, if anything.
    pub fix: Option<&'static str>,
}

fn check(
    id: &'static str,
    label: impl Into<String>,
    ok: Option<bool>,
    detail: impl Into<String>,
) -> Check {
    Check {
        id,
        label: label.into(),
        ok,
        detail: detail.into(),
        fix: None,
    }
}

impl Check {
    fn with_fix(mut self, fix: &'static str) -> Self {
        self.fix = Some(fix);
        self
    }
}

/// Who is holding a port, when it is not us.
///
/// "Wired can't start" is unhelpful; "Docker is using port 8000" is a thing a
/// person can act on. Best effort, and silent when the tools are missing.
pub fn port_holder(port: u16) -> Option<String> {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("lsof")
            .args(["-nP", "-t", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
            .output()
            .ok()?;
        let pid = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()?
            .to_string();
        let name = std::process::Command::new("ps")
            .args(["-p", &pid, "-o", "comm="])
            .output()
            .ok()?;
        let name = String::from_utf8_lossy(&name.stdout).trim().to_string();
        let name = Path::new(&name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(name);
        (!name.is_empty()).then(|| format!("{name} (process {pid})"))
    }
    #[cfg(not(unix))]
    {
        let _ = port;
        None
    }
}

pub fn port_is_free(host: &str, port: u16) -> bool {
    let Ok(addr) = format!("{host}:{port}").parse::<SocketAddr>() else {
        return false;
    };
    TcpListener::bind(addr).is_ok()
}

/// The last `lines` lines of today's log, without reading the whole file.
pub fn log_tail(lines: usize) -> Vec<String> {
    let path = log_file();
    let Ok(mut file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let Ok(size) = file.seek(SeekFrom::End(0)) else {
        return Vec::new();
    };
    // 64 KB is far more than `lines` lines of tracing output, and bounds what a
    // runaway log can pull into memory.
    let window = size.min(64 * 1024);
    if file.seek(SeekFrom::Start(size - window)).is_err() {
        return Vec::new();
    }
    let mut buffer = String::new();
    if file.take(window).read_to_string(&mut buffer).is_err() {
        return Vec::new();
    }
    let all: Vec<&str> = buffer.lines().collect();
    all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Today's log file. Named by day, matching the rolling appender in `lib.rs`.
pub fn log_file() -> std::path::PathBuf {
    let day = chrono::Local::now().format("%Y-%m-%d");
    paths::log_dir().join(format!("wired.log.{day}"))
}

fn writable(dir: &Path) -> bool {
    if paths::ensure_dir(dir).is_err() {
        return false;
    }
    let probe = dir.join(".wired-write-test");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Walk the chain and name the broken link.
pub fn checks(host: &str, port: u16, gateway: &Value) -> Vec<Check> {
    let mut out = Vec::new();
    let stored = settings_store::get();
    let provider = providers::default_assistant_provider();

    let found = providers::resolve_cmd(&provider);
    out.push(match &found {
        Some(argv) => check(
            "cli",
            format!("{provider} is installed"),
            Some(true),
            &argv[0],
        ),
        None => check(
            "cli",
            format!("{provider} is installed"),
            Some(false),
            "Wired can't find it on this computer.",
        )
        .with_fix("install"),
    });

    out.push(match providers::signed_in(&provider) {
        Some(true) => check("login", "Signed in", Some(true), "Credentials found."),
        _ if found.is_none() => check("login", "Signed in", None, "Install it first."),
        _ => check(
            "login",
            "Signed in",
            None,
            "Can't tell from here — open it once and sign in if it asks.",
        )
        .with_fix("login"),
    });

    let folder = providers::agent_cwd();
    out.push(if writable(&folder) {
        check(
            "folder",
            "Working folder",
            Some(true),
            folder.display().to_string(),
        )
    } else {
        check(
            "folder",
            "Working folder",
            Some(false),
            format!("Wired can't write to {}.", folder.display()),
        )
        .with_fix("folder")
    });

    // We are listening on it, so "free" would be wrong — the useful question is
    // whether anything *else* wants it.
    out.push(check(
        "port",
        format!("Listening on port {port}"),
        Some(true),
        format!("http://{host}:{port}"),
    ));

    let log_ok = writable(&paths::log_dir());
    out.push(if log_ok {
        check("logs", "Logs", Some(true), log_file().display().to_string())
    } else {
        check(
            "logs",
            "Logs",
            Some(false),
            format!("Can't write to {}.", paths::log_dir().display()),
        )
    });

    if gateway["enabled"] == json!(true) {
        let connected = gateway["connected"] == json!(true);
        out.push(if connected {
            check(
                "chat",
                "Telegram",
                Some(true),
                gateway["bot"].as_str().unwrap_or("connected").to_string(),
            )
        } else {
            check(
                "chat",
                "Telegram",
                Some(false),
                gateway["last_error"]
                    .as_str()
                    .unwrap_or("Not connected yet.")
                    .to_string(),
            )
            .with_fix("chat")
        });
        if stored.telegram.allowed_chats.is_empty() {
            out.push(
                check(
                    "paired",
                    "A phone is paired",
                    Some(false),
                    "Message your bot, then approve the code it gives you.",
                )
                .with_fix("chat"),
            );
        }
    }

    out
}

/// The paste-into-a-message block.
pub fn report(host: &str, port: u16, gateway: &Value, assistant: &Value) -> Value {
    let stored = settings_store::get();
    let providers: Vec<Value> = providers::probe_providers()
        .into_iter()
        .map(|p| json!({"id": p.id, "available": p.available, "path": p.path}))
        .collect();

    json!({
        "version": VERSION,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "host": host,
        "port": port,
        "auth_required": !settings_store::auth_token().is_empty(),
        "secrets": settings_store::secret_backing().as_str(),
        "config_dir": paths::config_dir().display().to_string(),
        "data_dir": paths::data_dir().display().to_string(),
        "log_file": log_file().display().to_string(),
        "folder": providers::agent_cwd().display().to_string(),
        "ask_before_acting": !providers::auto_approve_enabled(),
        "always_on": stored.always_on,
        "start_at_login": stored.start_at_login,
        "providers": providers,
        "node": crate::setup::node_status(),
        "assistant": assistant,
        "gateway": {
            "enabled": gateway["enabled"],
            "connected": gateway["connected"],
            "paired_chats": gateway["paired_chats"],
            "last_error": gateway["last_error"],
        },
        "checks": checks(host, port, gateway),
        "recent_log": log_tail(60),
    })
}

/// Everything Wired wrote to this machine, so "uninstall" can mean it.
pub fn removable() -> Vec<String> {
    [
        paths::settings_file(),
        paths::schedules_file(),
        paths::transcript_dir(),
        paths::log_dir(),
    ]
    .into_iter()
    .filter(|p| p.exists())
    .map(|p| p.display().to_string())
    .collect()
}

/// Delete Wired's own files. Never touches the agent's working folder, the
/// CLIs, or their credentials — those were not ours to install.
pub fn forget_everything() -> Result<Vec<String>, String> {
    let mut removed = Vec::new();
    for path in [paths::settings_file(), paths::schedules_file()] {
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            removed.push(path.display().to_string());
        }
    }
    for dir in [paths::transcript_dir(), paths::log_dir()] {
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            removed.push(dir.display().to_string());
        }
    }
    crate::secrets::delete("auth-token");
    crate::secrets::delete("telegram-bot-token");
    Ok(removed)
}
