//! Where Wired keeps its files.
//!
//! A packaged app has no project directory to write into, so settings, the
//! transcript store and the log all live in the platform's own locations. Every
//! one of them can be redirected with an environment variable, which is what
//! makes the developer path and the test suite able to stay out of a real
//! user's home directory.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Matches `tauri.conf.json`'s identifier, so the desktop app and the server
/// binary agree on one directory rather than each inventing its own.
pub const APP_ID: &str = "com.wired.terminal";
/// XDG prefers a readable name over a reverse-DNS one. Unused on macOS and
/// Windows, which both want the identifier.
#[allow(dead_code)]
const APP_DIR: &str = "wired-terminal";

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_dir(name: &str) -> Option<PathBuf> {
    let raw = std::env::var_os(name)?;
    let path = PathBuf::from(raw);
    (!path.as_os_str().is_empty()).then_some(path)
}

/// Settings and credentials — small, hand-editable, backed up by the OS.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = env_dir("WIRED_CONFIG_DIR") {
        return dir;
    }
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support").join(APP_ID)
    }
    #[cfg(target_os = "windows")]
    {
        env_dir("APPDATA")
            .unwrap_or_else(|| home().join("AppData/Roaming"))
            .join(APP_ID)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        env_dir("XDG_CONFIG_HOME")
            .unwrap_or_else(|| home().join(".config"))
            .join(APP_DIR)
    }
}

/// Transcripts and logs — potentially large, regenerable.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = env_dir("WIRED_DATA_DIR") {
        return dir;
    }
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support").join(APP_ID)
    }
    #[cfg(target_os = "windows")]
    {
        env_dir("LOCALAPPDATA")
            .unwrap_or_else(|| home().join("AppData/Local"))
            .join(APP_ID)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        env_dir("XDG_DATA_HOME")
            .unwrap_or_else(|| home().join(".local/share"))
            .join(APP_DIR)
    }
}

/// `Open logs folder` points here, so it has to be somewhere a file manager
/// will actually reveal — not a temp directory and not inside the bundle.
pub fn log_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if env_dir("WIRED_DATA_DIR").is_none() {
            return home().join("Library/Logs").join(APP_ID);
        }
    }
    data_dir().join("logs")
}

pub fn transcript_dir() -> PathBuf {
    data_dir().join("transcript")
}

pub fn settings_file() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn schedules_file() -> PathBuf {
    config_dir().join("schedules.json")
}

/// The default working folder offered during setup: a fresh directory rather
/// than the whole home folder, because that scope is what the agent may touch.
pub fn default_agent_folder() -> PathBuf {
    home().join("Wired")
}

pub fn ensure_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Write a file only the owner can read.
///
/// The settings file holds a bot token and the API token; on a shared machine a
/// world-readable copy of either is a working key to the agent. Permissions are
/// set before the contents are written, so there is no readable window.
pub fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    // One scratch file per call, not per path: two saves landing together —
    // resetting the chat bridge writes twice in a row — would otherwise share a
    // `settings.tmp`, and the loser of the race renames a file the winner has
    // already moved away.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp, contents)?;
    }

    // Rename last: a crash mid-write leaves the previous settings intact rather
    // than a truncated file that fails to parse on the next launch.
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}
