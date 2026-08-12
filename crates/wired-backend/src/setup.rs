//! Getting from "nothing installed" to "signed in and working", with no
//! commands typed.
//!
//! Two things stood between a non-coder and a running assistant: an agent CLI
//! that had to be installed from a terminal, and a login that "is interactive
//! and cannot be scripted". The first is a subprocess. The second turns out not
//! to need scripting at all — this app already owns a PTY, so the interactive
//! login can simply happen inside it.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::paths;
use crate::providers;

/// npm chatters; keep enough to diagnose a failure, not enough to scroll.
const LOG_LINES: usize = 200;

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// A prefix the user owns, so a global install never asks for a password.
/// `providers.rs` already looks here, and the desktop shell puts it on PATH.
fn npm_prefix() -> PathBuf {
    home().join(".npm-global")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Is there a Node to install the CLIs with?
///
/// Checked *before* the install button is pressed, because "npm: command not
/// found" halfway through a progress bar is exactly the dead end this whole
/// wizard exists to remove.
pub fn node_status() -> Value {
    let node = which("node");
    let npm = which("npm");
    let version = node.as_ref().and_then(|path| {
        Command::new(path)
            .arg("--version")
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    });
    let major = version.as_deref().and_then(|v| {
        v.trim_start_matches('v')
            .split('.')
            .next()?
            .parse::<u32>()
            .ok()
    });

    json!({
        "found": node.is_some() && npm.is_some(),
        "node": node.map(|p| p.display().to_string()),
        "npm": npm.map(|p| p.display().to_string()),
        "version": version,
        "supported": major.is_some_and(|m| m >= 18),
        "download": "https://nodejs.org/en/download",
    })
}

// ── installing a CLI ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Running,
    Done,
    Failed,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Running => "running",
            Phase::Done => "done",
            Phase::Failed => "failed",
        }
    }
}

struct Progress {
    phase: Phase,
    provider: String,
    /// One sentence, suitable for a progress line.
    message: String,
    log: VecDeque<String>,
}

/// Runs one install at a time and reports it in words.
#[derive(Clone)]
pub struct Installer {
    progress: Arc<Mutex<Progress>>,
}

impl Default for Installer {
    fn default() -> Self {
        Self::new()
    }
}

impl Installer {
    pub fn new() -> Self {
        Self {
            progress: Arc::new(Mutex::new(Progress {
                phase: Phase::Idle,
                provider: String::new(),
                message: String::new(),
                log: VecDeque::with_capacity(LOG_LINES),
            })),
        }
    }

    pub fn status(&self) -> Value {
        let progress = self.progress.lock().unwrap();
        json!({
            "status": progress.phase.as_str(),
            "provider": progress.provider,
            "message": progress.message,
            "log": progress.log.iter().cloned().collect::<Vec<_>>(),
        })
    }

    fn say(&self, message: impl Into<String>) {
        self.progress.lock().unwrap().message = message.into();
    }

    fn log_line(&self, line: String) {
        let mut progress = self.progress.lock().unwrap();
        if progress.log.len() == LOG_LINES {
            progress.log.pop_front();
        }
        progress.log.push_back(line);
    }

    /// Begin installing a provider's CLI. Returns immediately; poll `status`.
    pub fn start(&self, provider: &str) -> Result<(), String> {
        let package = providers::package_for(provider)
            .ok_or_else(|| format!("There is nothing to install for '{provider}'."))?;

        {
            let mut progress = self.progress.lock().unwrap();
            if progress.phase == Phase::Running {
                return Err("An install is already running.".into());
            }
            progress.phase = Phase::Running;
            progress.provider = provider.to_string();
            progress.message = "Starting…".into();
            progress.log.clear();
        }

        let npm = match which("npm") {
            Some(npm) => npm,
            None => {
                self.finish(
                    Phase::Failed,
                    "Node.js isn't installed yet. Install it first.",
                );
                return Err("Node.js isn't installed yet.".into());
            }
        };

        let me = self.clone();
        let provider = provider.to_string();
        std::thread::Builder::new()
            .name("wired-install".into())
            .spawn(move || me.run(npm, &provider, package))
            .map_err(|e| format!("Could not start the installer: {e}"))?;
        Ok(())
    }

    fn finish(&self, phase: Phase, message: impl Into<String>) {
        let mut progress = self.progress.lock().unwrap();
        progress.phase = phase;
        progress.message = message.into();
    }

    fn run(self, npm: PathBuf, provider: &str, package: &str) {
        let prefix = npm_prefix();
        let _ = paths::ensure_dir(&prefix);
        self.say(format!("Downloading {package}…"));

        let child = Command::new(&npm)
            .args([
                "install",
                "--global",
                "--prefix",
                &prefix.display().to_string(),
                // npm's progress bar is drawn with control codes that make no
                // sense outside a terminal.
                "--no-fund",
                "--no-audit",
                "--loglevel",
                "http",
                package,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match child {
            Ok(child) => child,
            Err(e) => {
                self.finish(Phase::Failed, format!("Could not run npm: {e}"));
                return;
            }
        };

        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let me = self.clone();
            // Both streams are drained concurrently: a full pipe on either one
            // deadlocks the child.
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if let Some(progress) = readable(&line) {
                        me.say(progress);
                    }
                    me.log_line(line);
                }
            });
        }

        let status = child.wait();
        // Give the reader threads a moment to drain what is already buffered.
        std::thread::sleep(std::time::Duration::from_millis(200));

        match status {
            Ok(status) if status.success() => {
                if providers::resolve_cmd(provider).is_some() {
                    self.finish(Phase::Done, "Installed.");
                } else {
                    // Installed, but somewhere this process cannot see.
                    self.finish(
                        Phase::Failed,
                        format!(
                            "Installed, but the {provider} command isn't where Wired looks. \
                             Restart Wired and try again."
                        ),
                    );
                }
            }
            Ok(_) => self.finish(Phase::Failed, self.diagnose()),
            Err(e) => self.finish(Phase::Failed, format!("The install stopped: {e}")),
        }
    }

    /// Turn npm's exit into something worth reading.
    fn diagnose(&self) -> String {
        let log = self.progress.lock().unwrap().log.clone();
        let joined = log.iter().cloned().collect::<Vec<_>>().join("\n");
        if joined.contains("EACCES") || joined.contains("permission denied") {
            "The install was blocked by file permissions.".into()
        } else if joined.contains("ENOTFOUND") || joined.contains("ETIMEDOUT") {
            "Couldn't reach the internet to download it.".into()
        } else if joined.contains("EBADENGINE") {
            "This needs a newer version of Node.js.".into()
        } else {
            log.back()
                .cloned()
                .unwrap_or_else(|| "The install didn't finish.".into())
        }
    }
}

/// npm's own lines, when one of them is worth showing.
fn readable(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("http fetch") {
        return Some("Downloading…".into());
    }
    if lower.starts_with("added ") || lower.contains("changed ") {
        return Some("Almost done…".into());
    }
    None
}

// ── signing in ──────────────────────────────────────────────────────────

/// Everything the wizard needs to decide which step to show.
pub fn state(installer: &Installer) -> Value {
    let stored = crate::settings_store::get();
    let providers: Vec<Value> = providers::probe_providers()
        .into_iter()
        .filter(|p| p.id != "shell")
        .map(|p| {
            json!({
                "id": p.id,
                "label": p.label,
                "available": p.available,
                "path": p.path,
                "detail": p.detail,
                "signed_in": providers::signed_in(&p.id),
                "installable": providers::package_for(&p.id).is_some(),
            })
        })
        .collect();

    json!({
        "onboarded": stored.onboarded,
        "node": node_status(),
        "providers": providers,
        "install": installer.status(),
        "folder": {
            "current": providers::agent_cwd().display().to_string(),
            "chosen": stored.folder,
            "suggested": paths::default_agent_folder().display().to_string(),
        },
        "ask_before_acting": !providers::auto_approve_enabled(),
    })
}

/// Create the folder the assistant will work in, and remember it.
pub fn choose_folder(folder: &str) -> Result<PathBuf, String> {
    let folder = folder.trim();
    if folder.is_empty() {
        return Err("Choose a folder for your assistant to work in.".into());
    }
    let path = PathBuf::from(folder);
    if !path.is_absolute() {
        return Err("That needs to be a full path.".into());
    }
    paths::ensure_dir(&path).map_err(|e| format!("Could not use {}: {e}", path.display()))?;
    // Prove it before promising it: a folder the agent cannot write to fails
    // later, in the middle of a task, with a message from the CLI.
    let probe = path.join(".wired-write-test");
    std::fs::write(&probe, b"ok")
        .map_err(|e| format!("Wired can't write to {}: {e}", path.display()))?;
    let _ = std::fs::remove_file(&probe);

    let saved = path.display().to_string();
    crate::settings_store::update(|s| s.folder = Some(saved))?;
    Ok(path)
}
