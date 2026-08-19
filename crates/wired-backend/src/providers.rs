//! Resolve the agent CLI binaries — Claude Code, Grok, Codex, Gemini — for
//! Wired assistant sessions.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// One agent CLI Wired can host.
///
/// Everything that differs between them lives in this row and nowhere else:
/// four CLIs is where a `match` per question (which flag? which binary? which
/// credential file?) stops being readable and starts being four places to
/// forget when a fifth arrives.
struct Agent {
    id: &'static str,
    label: &'static str,
    /// The binary to look for on `PATH`.
    bin: &'static str,
    /// Install locations that are not on `PATH` and not the shared npm/brew
    /// ones — a CLI that keeps its own prefix under `$HOME`.
    extra_paths: &'static [&'static str],
    /// Flags that pre-answer this CLI's approval prompts. Required for
    /// unattended operation and deliberately opt-out
    /// (`WIRED_AGENT_AUTO_APPROVE=0`) rather than hard-coded: with them on,
    /// anything that can reach the API can make the agent act on this machine
    /// without a human confirming.
    auto_approve: &'static [&'static str],
    /// Subcommand that runs the interactive sign-in, when the CLI has one.
    /// Empty means the bare CLI asks by itself on first run.
    login_args: &'static [&'static str],
    /// Files under `$HOME` whose existence means "signed in", best effort.
    credentials: &'static [&'static str],
    /// npm package that installs the CLI, for the setup wizard.
    package: &'static str,
}

/// The agent CLIs, in the order they are offered and auto-selected.
///
/// Claude and Grok lead because they are the two this shipped with: appending
/// rather than inserting keeps the "first CLI found" default landing where it
/// already did for anyone who has both.
const AGENTS: &[Agent] = &[
    Agent {
        id: "claude",
        label: "Claude Code",
        bin: "claude",
        extra_paths: &[".claude/local/claude"],
        auto_approve: &["--dangerously-skip-permissions"],
        login_args: &[],
        credentials: &[
            ".claude/.credentials.json",
            ".config/claude/.credentials.json",
        ],
        package: "@anthropic-ai/claude-code",
    },
    Agent {
        id: "grok",
        label: "Grok CLI",
        bin: "grok",
        extra_paths: &[".grok/bin/grok"],
        auto_approve: &["--always-approve"],
        login_args: &[],
        credentials: &[".grok/user-settings.json", ".grok/config.json"],
        package: "@vibe-kit/grok-cli",
    },
    Agent {
        id: "codex",
        label: "Codex CLI",
        bin: "codex",
        extra_paths: &[".codex/bin/codex"],
        // Codex sandboxes by default and asks before each escalation. Bypassing
        // both is the only setting that will not stall an unattended run —
        // which is why it is behind the same opt-out as the others.
        auto_approve: &["--dangerously-bypass-approvals-and-sandbox"],
        // Unlike the others, Codex will not offer to sign you in from the
        // session TUI — `codex login` is the flow, and it prints a URL the
        // terminal panel can show.
        login_args: &["login"],
        credentials: &[".codex/auth.json"],
        package: "@openai/codex",
    },
    Agent {
        id: "gemini",
        label: "Gemini CLI",
        bin: "gemini",
        extra_paths: &[".gemini/bin/gemini"],
        // `--yolo` pre-answers the tool prompts but not the "do you trust the
        // files in this folder?" modal Gemini opens on a folder it has not seen
        // before — which is the agent's own working folder, and which would sit
        // there unanswered with nobody at the terminal.
        auto_approve: &["--yolo", "--skip-trust"],
        login_args: &[],
        credentials: &[".gemini/oauth_creds.json", ".gemini/google_accounts.json"],
        package: "@google/gemini-cli",
    },
];

pub const ASSISTANT_PROVIDERS: &[&str] = &["claude", "grok", "codex", "gemini"];

fn agent(provider: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|a| a.id == provider)
}

/// The npm package that installs a CLI, for the setup wizard.
pub fn package_for(provider: &str) -> Option<&'static str> {
    agent(provider).map(|a| a.package)
}

/// Should the CLI act without confirming?
///
/// Yes, unless `WIRED_AGENT_AUTO_APPROVE=0`. The agent is unattended by
/// design — a prompt nobody is sitting in front of just wedges the session.
/// A stored Settings toggle used to flip this; that control is gone.
pub fn auto_approve_enabled() -> bool {
    crate::config::flag("WIRED_AGENT_AUTO_APPROVE", true)
}

fn auto_approve_args(agent: &Agent) -> Vec<String> {
    if auto_approve_enabled() {
        agent.auto_approve.iter().map(|f| f.to_string()).collect()
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub available: bool,
    pub path: Option<String>,
    pub label: String,
    #[serde(default)]
    pub detail: String,
    #[serde(skip)]
    pub argv: Vec<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Where the agent should be working — and therefore what it can reach.
///
/// This is the scope the setup wizard asks about in plain words, so the chosen
/// folder has to outrank whatever directory a GUI launch happened to inherit.
/// The last resort is still the home directory, because a GUI launch inherits
/// `/`, which is a useless place to run an assistant.
pub fn agent_cwd() -> PathBuf {
    if let Some(dir) = std::env::var_os("WIRED_AGENT_CWD") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return path;
        }
    }
    if let Some(folder) = crate::settings_store::get().folder {
        let path = PathBuf::from(folder.trim());
        if path.is_dir() {
            return path;
        }
    }
    match std::env::current_dir() {
        Ok(dir) if dir != Path::new("/") => dir,
        _ => home(),
    }
}

/// Windows has no execute bit and npm installs a shim, not a binary: `claude`
/// on disk is `claude.cmd`. Looking only for the bare name is why a Windows
/// build would report every provider missing.
#[cfg(windows)]
const EXTENSIONS: [&str; 4] = ["", ".exe", ".cmd", ".bat"];
#[cfg(not(windows))]
const EXTENSIONS: [&str; 1] = [""];

fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| {
            EXTENSIONS
                .iter()
                .map(move |ext| dir.join(format!("{name}{ext}")))
        })
        .find(|candidate| is_executable(candidate))
        .map(|p| p.to_string_lossy().into_owned())
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn which_or_fallbacks(name: &str, fallbacks: &[PathBuf]) -> Option<String> {
    if let Some(found) = which(name) {
        return Some(found);
    }
    fallbacks
        .iter()
        .find(|p| is_executable(p))
        .map(|p| p.to_string_lossy().into_owned())
}

/// Where a CLI ends up when it is not on `PATH`: its own prefix first, then
/// the shared ones every one of these installers writes to.
fn agent_path(agent: &Agent) -> Option<String> {
    let h = home();
    let bin = agent.bin;
    let fallbacks: Vec<PathBuf> = agent
        .extra_paths
        .iter()
        .map(|rel| h.join(rel))
        .chain([
            h.join(format!(".local/bin/{bin}")),
            // Where the setup wizard installs it: a prefix the user owns, so
            // the install never needs a password.
            h.join(format!(".npm-global/bin/{bin}")),
            PathBuf::from(format!("/opt/homebrew/bin/{bin}")),
            PathBuf::from(format!("/usr/local/bin/{bin}")),
        ])
        .collect();
    which_or_fallbacks(bin, &fallbacks)
}

fn resolve_agent(agent: &Agent) -> Option<Vec<String>> {
    let mut argv = vec![agent_path(agent)?];
    argv.extend(auto_approve_args(agent));
    Some(argv)
}

/// The CLI with no approval flags — what the interactive sign-in runs.
pub fn resolve_login_cmd(provider: &str) -> Option<Vec<String>> {
    let agent = agent(provider.trim().to_ascii_lowercase().as_str())?;
    let mut argv = vec![agent_path(agent)?];
    argv.extend(agent.login_args.iter().map(|a| a.to_string()));
    Some(argv)
}

pub fn resolve_shell() -> Vec<String> {
    #[cfg(windows)]
    {
        let shell = which("powershell")
            .or_else(|| which("pwsh"))
            .or_else(|| which("cmd"))
            .unwrap_or_else(|| "cmd.exe".to_string());
        vec![shell]
    }
    #[cfg(not(windows))]
    {
        let sh = which("zsh")
            .or_else(|| which("bash"))
            .unwrap_or_else(|| "/bin/sh".to_string());
        vec![sh]
    }
}

pub fn resolve_cmd(provider: &str) -> Option<Vec<String>> {
    let provider = provider.trim().to_ascii_lowercase();
    if provider == "shell" {
        return Some(resolve_shell());
    }
    resolve_agent(agent(&provider)?)
}

pub fn probe_providers() -> Vec<ProviderInfo> {
    let specs = AGENTS
        .iter()
        .map(|a| (a.id, a.label, resolve_agent(a)))
        .chain([("shell", "System Shell", Some(resolve_shell()))]);

    specs
        .map(|(id, label, resolved)| match resolved {
            Some(argv) => ProviderInfo {
                id: id.to_string(),
                available: true,
                path: Some(argv[0].clone()),
                label: label.to_string(),
                detail: "ready".to_string(),
                argv,
            },
            // The old text here named a PATH and a shell command. The app can
            // do this itself now, so say what will happen instead — the button
            // is in the setup wizard.
            None => ProviderInfo {
                id: id.to_string(),
                available: false,
                path: None,
                label: label.to_string(),
                detail: "Not installed yet — Wired can install it for you".to_string(),
                argv: Vec::new(),
            },
        })
        .collect()
}

/// Best-effort answer to "is this CLI signed in?".
///
/// `None` means we cannot tell, and the UI says so rather than guessing: these
/// CLIs may keep their credentials in the OS keychain, where looking would
/// raise a password prompt for a question nobody asked.
pub fn signed_in(provider: &str) -> Option<bool> {
    // Anything that is not an agent CLI — the shell — needs no sign-in.
    let Some(agent) = agent(provider) else {
        return Some(true);
    };
    let h = home();
    agent
        .credentials
        .iter()
        .any(|rel| h.join(rel).exists())
        .then_some(true)
}

/// Prefer env, then the assistant chosen in Settings, else the first available
/// agent CLI in `ASSISTANT_PROVIDERS` order.
pub fn default_assistant_provider() -> String {
    let chosen = crate::settings_store::string_or(
        "WIRED_ASSISTANT_PROVIDER",
        crate::settings_store::get().assistant,
        "",
    )
    .trim()
    .to_ascii_lowercase();
    if ASSISTANT_PROVIDERS.contains(&chosen.as_str()) && resolve_cmd(&chosen).is_some() {
        return chosen;
    }
    for provider in ASSISTANT_PROVIDERS {
        if resolve_cmd(provider).is_some() {
            return provider.to_string();
        }
    }
    "claude".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ASSISTANT_PROVIDERS` is what every caller validates against, and the
    /// table is what actually gets launched. A CLI added to one and not the
    /// other is either invisible or unrunnable, so tie them together here
    /// rather than trusting whoever adds the fifth to notice.
    #[test]
    fn the_provider_list_and_the_table_agree() {
        let from_table: Vec<&str> = AGENTS.iter().map(|a| a.id).collect();
        assert_eq!(from_table, ASSISTANT_PROVIDERS);
    }

    #[test]
    fn shell_is_not_an_assistant_but_still_resolves() {
        assert!(!ASSISTANT_PROVIDERS.contains(&"shell"));
        assert!(resolve_cmd("shell").is_some());
        assert!(resolve_cmd("nonesuch").is_none());
        // Nothing to sign into, so it never reads as "signed out".
        assert_eq!(signed_in("shell"), Some(true));
    }

    /// Provider ids arrive from HTTP, chat commands and the CLI, all of which
    /// let a human type them.
    #[test]
    fn provider_ids_are_matched_loosely() {
        assert_eq!(resolve_cmd(" SHELL "), resolve_cmd("shell"));
        assert!(resolve_login_cmd("nonesuch").is_none());
        // The shell has no sign-in to run.
        assert!(resolve_login_cmd("shell").is_none());
    }

    #[test]
    fn approvals_are_on_unless_the_environment_says_otherwise() {
        if std::env::var_os("WIRED_AGENT_AUTO_APPROVE").is_none() {
            assert!(
                auto_approve_enabled(),
                "the agent should act without asking by default"
            );
        }
    }

    #[test]
    fn every_agent_can_be_installed_and_probed() {
        for agent in AGENTS {
            assert!(
                package_for(agent.id).is_some(),
                "{} has no package",
                agent.id
            );
            assert!(
                !agent.auto_approve.is_empty(),
                "{} cannot run alone",
                agent.id
            );
            assert!(!agent.credentials.is_empty(), "{} has no sign-in", agent.id);
        }
        let probed: Vec<String> = probe_providers().into_iter().map(|p| p.id).collect();
        for id in ASSISTANT_PROVIDERS {
            assert!(probed.iter().any(|p| p == id), "{id} missing from probe");
        }
        assert!(probed.iter().any(|p| p == "shell"));
    }
}
