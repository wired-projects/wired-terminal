//! Is there a newer version, and where is it.
//!
//! One place for the question, because three things ask it: the desktop app on
//! launch, `wired update`, and anything polling `/api/update`. The answer comes
//! from the same `updates/latest.json` the release job writes and the Tauri
//! updater would read, so the site, the app and the CLI cannot disagree about
//! what the current version is.
//!
//! This module only answers the question; installing is the caller's job, and
//! the two callers do it differently. `wired update` swaps the published
//! `wired-backend` and `wired` binaries in place, which is why `server_download`
//! exists beside `download`. The desktop app cannot: replacing a signed `.app`
//! from inside itself is the Tauri updater's job, and that needs signed
//! artefacts and a public key compiled into the app.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config;
use crate::diagnostics::VERSION;

pub const DEFAULT_MANIFEST: &str = "https://wired-terminal-releases.wired.dev/updates/latest.json";

/// Where the manifest lives. `WIRED_UPDATE_MANIFEST` points a fork, a mirror or
/// a test at its own.
pub fn manifest_url() -> String {
    std::env::var("WIRED_UPDATE_MANIFEST").unwrap_or_else(|_| DEFAULT_MANIFEST.to_string())
}

/// Checking reaches the network, which is not always wanted: an air-gapped
/// box, a test, or someone who simply does not want the call made.
pub fn enabled() -> bool {
    config::flag("WIRED_UPDATE_CHECK", true)
}

/// The build this binary was made for, keyed the way `publish-r2.mjs` keys its
/// `downloads` map. `None` on anything the release workflow does not build.
pub fn target_id() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("macos-aarch64"),
        ("macos", "x86_64") => Some("macos-x86_64"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Download {
    url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    pub_date: Option<String>,
    #[serde(default)]
    downloads: std::collections::HashMap<String, Download>,
    /// The headless `wired-backend` + `wired` tarballs, which only some targets
    /// publish. Absent in every release before 1.0.3, hence `default`: an old
    /// manifest must still parse, it just offers nothing to install.
    #[serde(default)]
    server: std::collections::HashMap<String, Download>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// What is running now.
    pub current: String,
    /// What the manifest offers, when it could be read.
    pub latest: Option<String>,
    pub available: bool,
    pub notes: Option<String>,
    pub pub_date: Option<String>,
    /// The download for this platform, when the manifest lists one. A desktop
    /// bundle: something a person opens.
    pub download: Option<String>,
    /// The headless binaries for this platform, when the release published them.
    /// This is the one `wired update` can install by itself, with no installer
    /// re-run and no compile.
    pub server_download: Option<String>,
    /// False when the check was switched off or the manifest was unreachable.
    pub checked: bool,
    /// Why the check produced nothing. Not an error the caller must handle:
    /// being offline is a normal state, not a broken assistant.
    pub error: Option<String>,
}

impl Status {
    fn unchecked(reason: Option<String>) -> Self {
        Self {
            current: VERSION.to_string(),
            latest: None,
            available: false,
            notes: None,
            pub_date: None,
            download: None,
            server_download: None,
            checked: false,
            error: reason,
        }
    }
}

/// Compare dotted numeric versions: 1.10.0 is newer than 1.9.9, which a string
/// comparison gets wrong. Anything non-numeric (a `-rc1` suffix) sorts as 0, so
/// a pre-release never counts as newer than the release it precedes.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim()
            .trim_start_matches(['v', 'V'])
            .split('.')
            .map(|p| {
                p.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    }
    let (a, b) = (parts(candidate), parts(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

/// Read the manifest and say whether it offers something newer.
///
/// Never returns an error: a failed check is reported in the payload, because
/// no caller should have to decide what to do when a version check times out.
pub async fn check() -> Status {
    if !enabled() {
        return Status::unchecked(Some("update checks are off (WIRED_UPDATE_CHECK)".into()));
    }

    let url = manifest_url();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .user_agent(format!("wired-terminal/{VERSION}"))
        .build()
    {
        Ok(client) => client,
        Err(e) => return Status::unchecked(Some(format!("could not build a client: {e}"))),
    };

    let response = match client.get(&url).send().await {
        Ok(response) => response,
        Err(e) => return Status::unchecked(Some(format!("could not reach {url}: {e}"))),
    };

    // A 404 is the normal state before the first release, not a failure worth
    // shouting about.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Status::unchecked(Some("no release has been published yet".into()));
    }
    if !response.status().is_success() {
        return Status::unchecked(Some(format!("{url} answered {}", response.status())));
    }

    let manifest: Manifest = match response.json().await {
        Ok(manifest) => manifest,
        Err(e) => return Status::unchecked(Some(format!("could not read the manifest: {e}"))),
    };

    let download = target_id()
        .and_then(|id| manifest.downloads.get(id))
        .map(|d| d.url.clone());
    let server_download = target_id()
        .and_then(|id| manifest.server.get(id))
        .map(|d| d.url.clone());

    Status {
        current: VERSION.to_string(),
        available: is_newer(&manifest.version, VERSION),
        latest: Some(manifest.version),
        notes: manifest.notes,
        pub_date: manifest.pub_date,
        download,
        server_download,
        checked: true,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_compares_numerically_not_lexically() {
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(!is_newer("1.9.9", "1.10.0"));
    }

    #[test]
    fn equal_versions_are_not_newer() {
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("v1.0.0", "1.0.0"));
    }

    #[test]
    fn a_missing_component_reads_as_zero() {
        assert!(is_newer("1.1", "1.0.9"));
        assert!(!is_newer("1.0", "1.0.0"));
    }

    #[test]
    fn a_prerelease_suffix_does_not_beat_the_release() {
        assert!(!is_newer("1.0.0-rc1", "1.0.0"));
        assert!(is_newer("1.0.1-rc1", "1.0.0"));
    }
}
