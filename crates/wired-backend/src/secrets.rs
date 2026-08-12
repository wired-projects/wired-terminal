//! Credential storage, preferring the OS keychain.
//!
//! Two secrets matter here: the API token, which is a password to a shell on
//! this machine, and the Telegram bot token, which is a password to the chat
//! that drives it. Neither belongs in a build-time constant or a world-readable
//! file.
//!
//! The keychain is reached through the tools every desktop already ships
//! (`security` on macOS, `secret-tool` on Linux) rather than a native binding:
//! the binding pulls in D-Bus on Linux and a Windows crate that does not do
//! retrieval, for a feature that has to degrade gracefully anyway. When no
//! keychain answers, the secret stays in the 0600 settings file — which is
//! where an SSH private key lives too.

use std::process::{Command, Stdio};

const SERVICE: &str = "com.wired.terminal";

/// Did the value come from a keychain, or from the settings file?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backing {
    Keychain,
    File,
}

impl Backing {
    pub fn as_str(self) -> &'static str {
        match self {
            Backing::Keychain => "keychain",
            Backing::File => "file",
        }
    }
}

/// Only the Linux backend has to look for its tool: `security` ships with
/// macOS, and Windows has no CLI that reads a secret back out.
#[allow(dead_code)]
fn tool_exists(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// Keychain access can prompt, and a prompt in a headless process hangs
/// forever. Tests and servers opt out with one variable.
fn keychain_enabled() -> bool {
    crate::config::flag("WIRED_USE_KEYCHAIN", true)
}

#[cfg(target_os = "macos")]
fn backend() -> Option<&'static str> {
    keychain_enabled().then_some("security")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn backend() -> Option<&'static str> {
    (keychain_enabled() && tool_exists("secret-tool")).then_some("secret-tool")
}

#[cfg(not(unix))]
fn backend() -> Option<&'static str> {
    None
}

pub fn available() -> bool {
    backend().is_some()
}

/// Read a secret. `None` means "not in the keychain" — the caller falls back to
/// whatever the settings file holds.
pub fn get(key: &str) -> Option<String> {
    match backend()? {
        "security" => {
            let out = Command::new("security")
                .args(["find-generic-password", "-s", SERVICE, "-a", key, "-w"])
                .stderr(Stdio::null())
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        }
        "secret-tool" => {
            let out = Command::new("secret-tool")
                .args(["lookup", "service", SERVICE, "account", key])
                .stderr(Stdio::null())
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        }
        _ => None,
    }
}

/// Store a secret. Returns `false` when no keychain took it, which is the
/// signal to keep the value in the settings file instead of losing it.
pub fn set(key: &str, value: &str) -> bool {
    let Some(backend) = backend() else {
        return false;
    };
    match backend {
        "security" => Command::new("security")
            .args([
                "add-generic-password",
                "-s",
                SERVICE,
                "-a",
                key,
                "-w",
                value,
                // Replace rather than fail on an existing entry.
                "-U",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
        "secret-tool" => {
            use std::io::Write;
            let Ok(mut child) = Command::new("secret-tool")
                .args([
                    "store",
                    "--label",
                    "Wired Terminal",
                    "service",
                    SERVICE,
                    "account",
                    key,
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                return false;
            };
            if let Some(stdin) = child.stdin.as_mut() {
                if stdin.write_all(value.as_bytes()).is_err() {
                    return false;
                }
            }
            child.wait().map(|s| s.success()).unwrap_or(false)
        }
        _ => false,
    }
}

pub fn delete(key: &str) {
    let Some(backend) = backend() else { return };
    let _ = match backend {
        "security" => Command::new("security")
            .args(["delete-generic-password", "-s", SERVICE, "-a", key])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
        "secret-tool" => Command::new("secret-tool")
            .args(["clear", "service", SERVICE, "account", key])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
        _ => return,
    };
}
