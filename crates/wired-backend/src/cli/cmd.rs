//! The commands themselves.
//!
//! Every one of these is a thin reading of an endpoint that already exists —
//! the CLI adds no behaviour the API does not have, which is what keeps it
//! honest when the same thing is done from the app, from chat, or from `curl`.

use std::time::Duration;

use serde_json::{json, Value};

use super::args::{Pair, RemoteCmd, ScheduleCmd, Telegram};
use super::client::{Api, Result};
use super::profile::{Config, Remote, Target};
use super::service::Supervisor;
use super::ui::{human_duration, Mark, Ui};

/// Exit codes: 0 fine, 1 the command failed, 2 the thing it asked about is
/// unhealthy. `wired doctor` in a cron job wants to tell those two apart.
pub const EXIT_OK: i32 = 0;
pub const EXIT_UNHEALTHY: i32 = 2;

fn str_at<'a>(value: &'a Value, path: &[&str]) -> &'a str {
    let mut cursor = value;
    for key in path {
        cursor = &cursor[key];
    }
    cursor.as_str().unwrap_or("")
}

/// Ask before doing something that cannot be undone.
///
/// A pipe or a cron line has no way to answer, so there the flag is the only
/// consent there is — refusing is safer than assuming yes on an empty stdin.
fn confirm(ui: &Ui, already_agreed: bool, question: &str) -> Result<bool> {
    use std::io::{IsTerminal, Write};

    if already_agreed {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(format!("{question}\nRe-run with --yes to confirm."));
    }

    print!("{} {} ", ui.yellow(question), ui.dim("[y/N]"));
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| format!("could not read the answer: {e}"))?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

fn bool_at(value: &Value, path: &[&str]) -> bool {
    let mut cursor = value;
    for key in path {
        cursor = &cursor[key];
    }
    cursor.as_bool().unwrap_or(false)
}

// ── status ──────────────────────────────────────────────────────────────

pub async fn status(ui: &Ui, target: &Target, sup: &Supervisor, json_out: bool) -> Result<i32> {
    let state = sup.state();

    // The API may well be down — that is a normal thing for `status` to report,
    // not an error that should stop it printing what it does know.
    let health = match Api::connect(target).await {
        Ok(api) => api.get("/api/health").await.map_err(Some),
        Err(e) => Err(Some(e)),
    };

    if json_out {
        ui.json(&json!({
            "target": target.describe(),
            "service": {
                "manager": state.manager,
                "running": state.running,
                "detail": state.detail,
                "pid": state.pid,
                "uptime_seconds": state.uptime,
            },
            "health": health.clone().unwrap_or(Value::Null),
        }));
        return Ok(if state.running || !sup.is_managed() {
            EXIT_OK
        } else {
            EXIT_UNHEALTHY
        });
    }

    let mut detail = String::new();
    if let Some(pid) = state.pid {
        detail.push_str(&format!("pid {pid}"));
    }
    if let Some(uptime) = state.uptime {
        if !detail.is_empty() {
            detail.push_str("   ");
        }
        detail.push_str(&format!("up {}", human_duration(uptime)));
    }

    ui.row(
        &sup.label(),
        match (sup.is_managed(), state.running) {
            (false, _) => Mark::Unknown,
            (true, true) => Mark::Good,
            (true, false) => Mark::Bad,
        },
        &state.detail,
        &detail,
    );

    let health = match health {
        Ok(health) => health,
        Err(reason) => {
            ui.row("api", Mark::Bad, "not answering", &target.describe());
            if let Some(reason) = reason {
                ui.note(&reason);
            }
            return Ok(EXIT_UNHEALTHY);
        }
    };

    // ── the agent
    let assistant = &health["assistant"];
    let running = assistant["session_running"].as_bool().unwrap_or(false);
    let provider = match str_at(assistant, &["session_provider"]) {
        "" => str_at(assistant, &["provider"]),
        p => p,
    };
    let waiting = !health["pending_prompt"].is_null();
    let (mark, note) = match (running, waiting) {
        (false, _) => (Mark::Bad, "stopped".to_string()),
        (true, true) => (
            Mark::Unknown,
            "waiting for you — `wired approve`".to_string(),
        ),
        (true, false) => (Mark::Good, "running".to_string()),
    };
    let label = if provider.is_empty() { "—" } else { provider };
    ui.row("agent", mark, label, &note);

    // ── the way in
    let security = &health["security"];
    let mut api_note = Vec::new();
    if bool_at(security, &["auth_required"]) {
        api_note.push("token required");
    }
    if bool_at(security, &["loopback_only"]) {
        api_note.push("loopback only");
    }
    ui.row("api", Mark::Good, &target.describe(), &api_note.join(", "));

    let chat = &health["chat"];
    let pending = chat["pending_pairings"].as_u64().unwrap_or(0);
    if bool_at(chat, &["enabled"]) {
        let connected = bool_at(chat, &["connected"]);
        let mut note = String::new();
        if pending > 0 {
            note = format!("{pending} waiting to pair — `wired pair`");
        }
        ui.row(
            "telegram",
            if connected { Mark::Good } else { Mark::Bad },
            if connected {
                "connected"
            } else {
                "disconnected"
            },
            &note,
        );
    } else {
        ui.row("telegram", Mark::None, "off", "");
    }

    ui.field("folder", str_at(&health, &["folder"]));

    if !bool_at(security, &["agent_auto_approve"]) {
        ui.note("ask-before-acting is on: the agent pauses for approval");
    }
    if let Some(error) = assistant["last_error"].as_str() {
        ui.warn(error);
    }

    // The agent is what you asked about; the service only counts against it when
    // this machine is the one supposed to be running it.
    Ok(if running && (state.running || !sup.is_managed()) {
        EXIT_OK
    } else {
        EXIT_UNHEALTHY
    })
}

// ── the service ─────────────────────────────────────────────────────────

pub async fn start(
    ui: &Ui,
    target: &Target,
    sup: &Supervisor,
    provider: Option<String>,
) -> Result<i32> {
    if !sup.state().running {
        sup.start()?;
        println!("{} {}", ui.green("started"), sup.describe());
    }

    let api = Api::connect(target).await?;
    if !api.wait_alive(Duration::from_secs(20)).await {
        return Err(format!(
            "the service started but {} is not answering — try `wired logs`",
            api.base()
        ));
    }

    let mut body = json!({"keep_alive": true});
    if let Some(provider) = provider {
        body["provider"] = json!(provider);
    }
    let result = api.post("/api/agent/start", body).await?;
    let started = str_at(&result, &["assistant", "session_provider"]);
    println!(
        "{} {}",
        ui.green("agent running"),
        if started.is_empty() { "—" } else { started }
    );
    Ok(EXIT_OK)
}

pub async fn stop(ui: &Ui, target: &Target, sup: &Supervisor, agent_only: bool) -> Result<i32> {
    if agent_only {
        let api = Api::connect(target).await?;
        api.post("/api/agent/stop", json!({})).await?;
        println!("{}", ui.green("agent session stopped"));
        return Ok(EXIT_OK);
    }
    sup.stop()?;
    println!("{} {}", ui.green("stopped"), sup.describe());
    Ok(EXIT_OK)
}

pub async fn restart(ui: &Ui, target: &Target, sup: &Supervisor) -> Result<i32> {
    sup.restart()?;
    let api = Api::connect(target).await?;
    if api.wait_alive(Duration::from_secs(30)).await {
        println!("{} {}", ui.green("restarted"), sup.describe());
        Ok(EXIT_OK)
    } else {
        Err(format!(
            "restarted, but {} did not answer within 30s — try `wired logs`",
            api.base()
        ))
    }
}

// ── the agent ───────────────────────────────────────────────────────────

pub async fn ask(ui: &Ui, target: &Target, text: &str, wait: f64, json_out: bool) -> Result<i32> {
    let api = Api::connect(target).await?;
    let result = api
        .post(
            "/api/agent/message",
            json!({
                "text": text,
                "submit": true,
                "ensure_session": true,
                "wait_seconds": wait,
                "plain": true,
            }),
        )
        .await?;

    if json_out {
        ui.json(&result);
        return Ok(EXIT_OK);
    }

    if wait <= 0.0 {
        println!("{}", ui.dim("sent — follow it with `wired watch`"));
        return Ok(EXIT_OK);
    }

    match result["text"].as_str().map(str::trim).unwrap_or("") {
        "" => {
            ui.warn("no output within the wait — it may still be working; try `wired watch`");
            Ok(EXIT_UNHEALTHY)
        }
        reply => {
            println!("{reply}");
            Ok(EXIT_OK)
        }
    }
}

pub async fn watch(ui: &Ui, target: &Target) -> Result<i32> {
    let api = Api::connect(target).await?;
    eprintln!("{}", ui.dim("watching — ctrl-c to detach"));

    api.stream_sse("/api/agent/output/stream", |event| {
        let text = event.data.trim_end();
        match event.kind.as_str() {
            // The API already prefixes these with ❯ and a blank line.
            "user" => println!("{}", ui.magenta(text)),
            "prompt" => println!("{}", ui.yellow(text)),
            "notice" => println!("{}", ui.yellow(text)),
            "system" => println!("{}", ui.blue(text)),
            "session" | "status" => println!("{}", ui.dim(text)),
            _ => println!("{text}"),
        }
    })
    .await?;
    Ok(EXIT_OK)
}

pub async fn approve(ui: &Ui, target: &Target, allow: bool) -> Result<i32> {
    let api = Api::connect(target).await?;
    api.post("/api/agent/approve", json!({ "allow": allow }))
        .await?;
    println!(
        "{}",
        if allow {
            ui.green("approved")
        } else {
            ui.yellow("denied")
        }
    );
    Ok(EXIT_OK)
}

// ── setup ───────────────────────────────────────────────────────────────

/// `wired update` — ask the manifest, then reinstall from source if asked to.
///
/// The check goes through the target's API rather than being done here, so
/// `--remote pilot update --check` reports that server's version and not this
/// laptop's. Applying an update is local-only: it rebuilds and restarts a
/// service, which over an SSH tunnel would be a surprise rather than a
/// convenience.
pub async fn update(ui: &Ui, target: &Target, check_only: bool, yes: bool) -> Result<i32> {
    let api = Api::connect(target).await?;
    let status = api.get("/api/update").await?;

    let current = str_at(&status, &["current"]);
    let latest = status["latest"].as_str();
    let available = bool_at(&status, &["available"]);

    ui.heading("Version");
    ui.field("running", current);
    match latest {
        Some(latest) => ui.field("published", latest),
        None => ui.field("published", "unknown"),
    }
    if let Some(date) = status["pub_date"].as_str() {
        ui.field("released", date);
    }
    if let Some(reason) = status["error"].as_str() {
        ui.note(reason);
    }

    if !available {
        if latest.is_some() {
            ui.note("Up to date.");
        }
        return Ok(EXIT_OK);
    }

    if let Some(notes) = status["notes"].as_str().filter(|n| !n.is_empty()) {
        ui.field("notes", notes);
    }
    ui.warn(&format!(
        "{} is out; this is {current}.",
        latest.unwrap_or("a newer version")
    ));

    if check_only {
        // Non-zero so a cron line can act on it, the same way `doctor` does.
        return Ok(EXIT_UNHEALTHY);
    }

    // An update replaces the binaries on disk and restarts the unit, and both of
    // those are local acts — this command over a tunnel would be reading one
    // machine's version and writing another's.
    if !matches!(target, Target::Local { .. }) {
        return Err(
            "an update replaces binaries and restarts the service, so run it on that machine:\n               ssh <host> sudo wired update"
                .into(),
        );
    }

    // Two ways to become the new version: swap the published binaries, or — for
    // a desktop install, which is a signed `.app` rather than two binaries, and
    // for an architecture nothing is published for — say where to get it.
    if let Some(url) = status["server_download"].as_str() {
        // Before the question, not after it. /opt is root-owned on a normal
        // install, so the common case is that this cannot work at all, and
        // "answer yes, then be told to use sudo" wastes the one interaction.
        writable_install_dir()?;

        if !confirm(ui, yes, "Install the new binaries and restart the service?")? {
            ui.note("Left alone.");
            return Ok(EXIT_OK);
        }
        let installed = install_server_binaries(ui, url, latest.unwrap_or("")).await?;

        let sup = Supervisor::for_target(target);
        if sup.is_managed() {
            ui.note("Restarting the service.");
            sup.restart()?;
        } else {
            ui.note("Nothing supervises this install, so the new binary starts on your next run.");
        }
        ui.note(&format!("Updated to {installed}."));
        return Ok(EXIT_OK);
    }

    // Nothing published for this platform. There used to be a rebuild path
    // here — update the checkout, re-run the installer, compile — and it is gone
    // along with the installer's compiler: `install-ubuntu.sh` no longer builds
    // anything, so re-running it would either download the same binary this
    // command already could not find, or stop and say so. Offering a rebuild
    // that cannot happen is worse than naming the one thing that works.
    let download = status["download"]
        .as_str()
        .unwrap_or("https://terminal.wired.dev/#install");
    ui.note(&format!(
        "No binaries are published for this platform ({} {}).",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    ui.note("Build one from a checkout, then install it:");
    ui.note("  cargo build --release --manifest-path crates/wired-backend/Cargo.toml");
    ui.note("  sudo bash scripts/install-ubuntu.sh --binary <path to wired-backend>");
    ui.note(&format!("Or take the desktop build: {download}"));
    Ok(EXIT_OK)
}

/// Refuse early when the binaries cannot be replaced anyway.
///
/// `install_server_binaries` reports the same thing, but only once it is already
/// downloading — which on a root-owned `/opt` means every non-root run asked a
/// question it could never act on.
fn writable_install_dir() -> Result<()> {
    let exe =
        std::env::current_exe().map_err(|e| format!("could not find my own location: {e}"))?;
    let dir = exe
        .parent()
        .ok_or("could not find the directory I am installed in")?;

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| "the install path has a NUL in it".to_string())?;
        // access(2) rather than a probe file: asking is cheaper than writing,
        // and leaves nothing behind when the answer is no.
        if unsafe { libc::access(c.as_ptr(), libc::W_OK) } != 0 {
            return Err(format!(
                "{} is not writable by this user: sudo wired update",
                dir.display()
            ));
        }
    }
    Ok(())
}

/// Replace the running `wired-backend` and `wired` with the published pair.
///
/// The work happens in a directory beside the binaries it replaces, so the swap
/// is a `rename` within one filesystem: atomic, and never a half-written file
/// for systemd to start. Unix allows renaming over a running executable, which
/// is what lets this replace the very CLI that is running it — the old inode
/// stays alive until the process and the service exit.
///
/// Returns what the new binary reports as its version.
async fn install_server_binaries(ui: &Ui, url: &str, expected: &str) -> Result<String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("could not find my own location: {e}"))?;
    let bin_dir = exe
        .parent()
        .ok_or("could not find the directory I am installed in")?;

    // Writability is the real requirement, not root: /opt needs sudo, but an
    // install under a home directory does not, and demanding root there would
    // be a lie. Finding out here also beats a permission error three steps into
    // a tar.
    let work = bin_dir.join(".wired-update");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            format!(
                "{} is not writable by this user: sudo wired update",
                bin_dir.display()
            )
        } else {
            format!("could not write beside the binaries: {e}")
        }
    })?;

    // Anything that fails from here leaves nothing behind but the temp dir.
    let result = fetch_and_swap(ui, url, expected, bin_dir, &work).await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

async fn fetch_and_swap(
    ui: &Ui,
    url: &str,
    expected: &str,
    bin_dir: &std::path::Path,
    work: &std::path::Path,
) -> Result<String> {
    ui.note("Downloading the new binaries.");
    let archive = work.join("server.tar.gz");
    let client = reqwest::Client::builder()
        // Minutes, not seconds: this is ~10MB over whatever the box has.
        .timeout(Duration::from_secs(180))
        .user_agent(concat!("wired/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("could not build an HTTP client: {e}"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("could not reach {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("could not read {url}: {e}"))?;
    std::fs::write(&archive, &bytes).map_err(|e| format!("could not save the download: {e}"))?;

    let untar = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(work)
        .status()
        .map_err(|e| format!("could not run tar: {e}"))?;
    if !untar.success() {
        return Err("the download did not unpack — a partial or corrupt file".into());
    }

    let names = ["wired-backend", "wired"];
    for name in names {
        if !work.join(name).is_file() {
            return Err(format!("the archive did not contain {name}"));
        }
    }

    // The last point where backing out is free. This catches both a binary that
    // cannot run here (an older glibc than the build's) and a manifest pointing
    // at the wrong tarball, before either becomes what systemd starts.
    let probe = std::process::Command::new(work.join("wired"))
        .arg("--version")
        .output()
        .map_err(|e| format!("the downloaded binary would not run: {e}"))?;
    if !probe.status.success() {
        return Err("the downloaded binary would not run on this system".into());
    }
    let reported = String::from_utf8_lossy(&probe.stdout).trim().to_string();
    if !expected.is_empty() && !reported.contains(expected) {
        return Err(format!(
            "the download reports itself as {reported:?}, but the manifest promised {expected}"
        ));
    }

    // Old versions go aside rather than away, so a failure halfway through two
    // renames can be put back instead of leaving a mismatched pair installed.
    let backup = work.join("previous");
    std::fs::create_dir_all(&backup).map_err(|e| format!("could not stage a rollback: {e}"))?;
    let mut moved: Vec<&str> = Vec::new();
    for name in names {
        let live = bin_dir.join(name);
        if live.exists() {
            std::fs::rename(&live, backup.join(name))
                .map_err(|e| format!("could not move the running {name} aside: {e}"))?;
        }
        if let Err(e) = std::fs::rename(work.join(name), &live) {
            // Put back whatever was already replaced, including this one.
            for done in moved.iter().copied().chain(std::iter::once(name)) {
                let saved = backup.join(done);
                if saved.exists() {
                    let _ = std::fs::rename(saved, bin_dir.join(done));
                }
            }
            return Err(format!("could not install the new {name}: {e}"));
        }
        moved.push(name);
    }

    Ok(reported)
}

/// The guided first run.
///
/// Everything here exists as its own command already — `doctor`, `start`,
/// `telegram`, `pair`, `approve`. What this adds is the order, and the waiting:
/// the steps depend on each other, and two of them used to end with "now go and
/// do something else, then run another command". Pairing in particular was
/// message-the-bot-then-poll, so this waits for the request and offers to
/// approve it, which is the difference between five commands and one.
///
/// It asks before every change. `--yes` takes the steps that cannot surprise
/// anyone and skips the rest, saying which — a provisioning script wants an
/// honest report, not a wizard blocked on a prompt it cannot answer.
pub async fn setup(
    ui: &Ui,
    target: &Target,
    yes: bool,
    want_telegram: bool,
    json_out: bool,
) -> Result<i32> {
    let api = Api::connect(target).await?;
    let report = api.get("/api/diagnostics").await?;

    if json_out {
        // Nothing interactive can happen in a pipe, so this is `doctor` with a
        // different name rather than a wizard pretending to run.
        ui.json(&report);
        return Ok(EXIT_OK);
    }

    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
    let mut skipped: Vec<&str> = Vec::new();

    // ── 1. the agent CLI ────────────────────────────────────────────────
    ui.heading("The agent CLI");
    let checks = report["checks"].as_array().cloned().unwrap_or_default();
    let find = |id: &str| checks.iter().find(|c| str_at(c, &["id"]) == id).cloned();

    let cli_ok = find("cli").map(|c| bool_at(&c, &["ok"])).unwrap_or(false);
    match find("cli") {
        Some(c) => ui.row(
            "cli",
            Mark::from_ok(c["ok"].as_bool()),
            str_at(&c, &["label"]),
            str_at(&c, &["detail"]),
        ),
        None => ui.row("cli", Mark::Unknown, "not reported", ""),
    }
    if !cli_ok {
        let providers = report["providers"]
            .as_array()
            .map(|p| {
                p.iter()
                    .map(|v| str_at(v, &["id"]).to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        ui.warn("No agent CLI found, so there is nothing for Wired to keep alive.");
        ui.note("Install one, then run this again:");
        ui.note("  npm install -g @anthropic-ai/claude-code");
        if !providers.is_empty() {
            ui.note(&format!("Wired can drive: {providers}"));
        }
        return Ok(EXIT_UNHEALTHY);
    }

    // `login` is deliberately tri-state: the backend cannot tell from outside,
    // so the honest check is whether the agent answers, which happens below.
    if let Some(login) = find("login") {
        if login["ok"].as_bool() == Some(false) {
            ui.warn(str_at(&login, &["detail"]));
            ui.note("Sign in once, as the account the agent runs as:");
            ui.note("  sudo -u <user> -H claude      # then Ctrl-D");
        }
    }

    // ── 2. a running session ────────────────────────────────────────────
    ui.heading("The session");
    let assistant = &report["assistant"];
    let mut running = bool_at(assistant, &["session_running"]);
    if running {
        ui.row(
            "agent",
            Mark::Good,
            str_at(assistant, &["session_provider"]),
            "running",
        );
    } else {
        ui.row("agent", Mark::Bad, "—", "not running");
        if confirm(ui, yes, "Start the agent now?")? {
            let started = api
                .post("/api/agent/start", json!({ "keep_alive": true }))
                .await?;
            running = bool_at(&started, &["assistant", "session_running"]);
            ui.row(
                "agent",
                Mark::from_ok(Some(running)),
                str_at(&started, &["assistant", "session_provider"]),
                if running { "started" } else { "did not start" },
            );
        } else {
            skipped.push("starting the agent");
        }
    }

    // The only check that proves the CLI is signed in and working, which is why
    // it is offered rather than assumed: it costs a real round trip to a model.
    if running && interactive && !yes && confirm(ui, false, "Send it a test message?")? {
        ui.note("Asking it to say hello — up to 45s.");
        let reply = api
            .post(
                "/api/agent/message",
                json!({
                    "text": "Reply with exactly: wired is working",
                    "submit": true,
                    "ensure_session": true,
                    "wait_seconds": 45.0,
                    "plain": true,
                }),
            )
            .await?;
        let text = str_at(&reply, &["reply"]);
        if text.is_empty() {
            ui.warn("No reply yet. `wired watch` shows what it is doing.");
            ui.note("If it is asking to be signed in: sudo -u <user> -H claude");
        } else {
            ui.row("reply", Mark::Good, text.lines().next().unwrap_or(""), "");
        }
    } else if running {
        skipped.push("the test message");
    }

    // ── 3. Telegram, and the pairing that follows it ────────────────────
    if want_telegram {
        ui.heading("Your phone");
        let gateway = api.get("/api/gateway/status").await?;
        let configured = bool_at(&gateway, &["configured"]);
        let connected = bool_at(&gateway, &["connected"]);
        let paired = gateway["paired_chats"].as_u64().unwrap_or(0);

        if paired > 0 && connected {
            ui.row(
                "telegram",
                Mark::Good,
                "connected",
                &format!("{paired} paired"),
            );
        } else if !configured {
            ui.row("telegram", Mark::None, "no bot token yet", "");
            if interactive && !yes {
                ui.note("In Telegram: message @BotFather, send /newbot, answer its two questions.");
                if confirm(ui, false, "Paste the token it gave you now?")? {
                    let token = read_token_quietly(ui)?;
                    api.post(
                        "/api/gateway/configure",
                        json!({ "bot_token": token, "enabled": true }),
                    )
                    .await?;
                    let live = wait_for_connect(ui, &api).await?;
                    if live {
                        wait_for_pairing(ui, &api, yes).await?;
                    }
                } else {
                    skipped.push("Telegram");
                }
            } else {
                skipped.push("Telegram (needs a token typed in)");
            }
        } else if !connected {
            ui.row("telegram", Mark::Bad, "token set, not connected", "");
            if let Some(err) = gateway["last_error"].as_str().filter(|e| !e.is_empty()) {
                ui.warn(err);
            }
            ui.note("`wired telegram on` to re-send it, or `wired pair reset` to start over.");
        } else {
            ui.row("telegram", Mark::Good, "connected", "no phone paired yet");
            wait_for_pairing(ui, &api, yes).await?;
        }
    }

    // ── 4. what it is already waiting on ────────────────────────────────
    let health = api.get("/api/health").await?;
    if !health["pending_prompt"].is_null() {
        ui.heading("It is waiting for you");
        let prompt = &health["pending_prompt"];
        ui.note(str_at(prompt, &["question"]));
        if confirm(ui, yes, "Allow it?")? {
            api.post("/api/agent/approve", json!({ "allow": true }))
                .await?;
            println!("{}", ui.green("allowed"));
        } else {
            skipped.push("the pending approval (`wired approve`)");
        }
    }

    // ── what is left ────────────────────────────────────────────────────
    ui.heading("Where that leaves you");
    ui.field("folder", str_at(&report, &["folder"]));
    ui.field(
        "acting",
        if bool_at(&report, &["ask_before_acting"]) {
            "asks first"
        } else {
            "freely (auto-approve on)"
        },
    );
    for what in &skipped {
        ui.note(&format!("skipped: {what}"));
    }
    ui.note("`wired status` any time. `wired ask \"…\"` to give it work.");
    Ok(EXIT_OK)
}

/// Wait for Telegram to answer. Connecting is a round trip out of the box, so
/// the reply to `configure` is usually "not yet" rather than "no".
async fn wait_for_connect(ui: &Ui, api: &Api) -> Result<bool> {
    for _ in 0..12 {
        let status = api.get("/api/gateway/status").await?;
        if bool_at(&status, &["connected"]) {
            let bot = str_at(&status, &["bot"]);
            ui.row(
                "telegram",
                Mark::Good,
                &if bot.is_empty() {
                    "connected".to_string()
                } else {
                    format!("connected as @{bot}")
                },
                "",
            );
            return Ok(true);
        }
        if let Some(err) = status["last_error"].as_str().filter(|e| !e.is_empty()) {
            ui.warn(err);
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    ui.warn("Saved, but Telegram has not answered yet — `wired telegram` to check.");
    Ok(false)
}

/// Wait for a phone to ask to be let in, then offer to let it in.
///
/// This is the step that made pairing feel like homework: the code appears on
/// the phone, and until now you had to come back and run two more commands.
async fn wait_for_pairing(ui: &Ui, api: &Api, yes: bool) -> Result<()> {
    if yes {
        ui.note("skipped: pairing (needs the code from your phone)");
        return Ok(());
    }
    ui.note("Now message your bot from your phone — anything will do.");
    ui.note("Waiting up to two minutes; ctrl-c to stop and pair later with `wired pair`.");

    for _ in 0..60 {
        let pairings = api.get("/api/gateway/pairings").await?;
        let pending = pairings["pending"].as_array().cloned().unwrap_or_default();
        if let Some(first) = pending.first() {
            let display = str_at(first, &["display"]);
            let code = str_at(first, &["code"]);
            ui.row("request", Mark::Unknown, display, code);
            // Naming who is asking matters: approving is handing that chat the
            // ability to run commands as the service user.
            if confirm(ui, false, &format!("Let {display} in?"))? {
                api.post("/api/gateway/pairings/approve", json!({ "code": code }))
                    .await?;
                println!("{}", ui.green("paired"));
                ui.note("Send it another message — answers come back to your phone.");
            } else {
                api.post("/api/gateway/pairings/deny", json!({ "code": code }))
                    .await?;
                println!("{}", ui.yellow("denied"));
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    ui.note("Nothing asked to pair. Message the bot, then `wired pair`.");
    Ok(())
}

pub async fn doctor(ui: &Ui, target: &Target, show_log: bool, json_out: bool) -> Result<i32> {
    let api = Api::connect(target).await?;
    let report = api.get("/api/diagnostics").await?;

    if json_out {
        ui.json(&report);
        return Ok(EXIT_OK);
    }

    let checks = report["checks"].as_array().cloned().unwrap_or_default();
    let mut failed = false;
    ui.heading("Checks");
    for check in &checks {
        let ok = check["ok"].as_bool();
        failed |= ok == Some(false);
        ui.row(
            str_at(check, &["id"]),
            Mark::from_ok(ok),
            str_at(check, &["label"]),
            str_at(check, &["detail"]),
        );
    }

    ui.heading("This install");
    ui.field(
        "version",
        &format!(
            "{} ({}/{})",
            str_at(&report, &["version"]),
            str_at(&report, &["os"]),
            str_at(&report, &["arch"])
        ),
    );
    ui.field(
        "api",
        &format!(
            "{}:{}",
            str_at(&report, &["host"]),
            report["port"].as_u64().unwrap_or(0)
        ),
    );
    ui.field(
        "auth",
        if bool_at(&report, &["auth_required"]) {
            "token required"
        } else {
            "open (loopback)"
        },
    );
    ui.field("secrets", str_at(&report, &["secrets"]));
    ui.field("config", str_at(&report, &["config_dir"]));
    ui.field("data", str_at(&report, &["data_dir"]));
    ui.field("log", str_at(&report, &["log_file"]));

    if show_log {
        ui.heading("Recent log");
        for line in report["recent_log"].as_array().unwrap_or(&Vec::new()) {
            println!("  {}", line.as_str().unwrap_or_default());
        }
    }

    Ok(if failed { EXIT_UNHEALTHY } else { EXIT_OK })
}

/// Read a line without echoing it, so a token pasted at a prompt does not stay
/// on the screen. Falls back to a visible read when stdin is not a terminal,
/// which is what makes `echo $TOKEN | wired telegram on` work in a script.
fn read_token_quietly(ui: &Ui) -> Result<String> {
    use std::io::{BufRead, Write};

    let stdin = std::io::stdin();
    let interactive = std::io::IsTerminal::is_terminal(&stdin);
    if interactive {
        print!("{} ", ui.dim("Bot token from @BotFather:"));
        let _ = std::io::stdout().flush();
    }

    // Turning the echo off is worth the unsafe: the alternative is a bot token
    // sitting in the scrollback of a shared terminal.
    #[cfg(unix)]
    let restore = {
        use std::os::fd::AsRawFd as _;
        let fd = stdin.as_raw_fd();
        let mut term: libc::termios = unsafe { std::mem::zeroed() };
        if interactive && unsafe { libc::tcgetattr(fd, &mut term) } == 0 {
            let original = term;
            term.c_lflag &= !libc::ECHO;
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
            Some((fd, original))
        } else {
            None
        }
    };

    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);

    #[cfg(unix)]
    if let Some((fd, original)) = restore {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
        println!();
    }

    read.map_err(|e| format!("could not read the token: {e}"))?;
    let token = line.trim().to_string();
    if token.is_empty() {
        return Err("no token given".into());
    }
    Ok(token)
}

pub async fn telegram(ui: &Ui, target: &Target, cmd: &Telegram, json_out: bool) -> Result<i32> {
    // Prompt for and check the token before opening anything, so a typo is
    // answered as a typo rather than hidden behind whatever the network says —
    // and so a malformed token is never put on the wire at all.
    let token = match cmd {
        Telegram::On(given) => {
            let token = match given {
                Some(token) => token.trim().to_string(),
                None => read_token_quietly(ui)?,
            };
            // BotFather's tokens are `<digits>:<secret>`. Saying so now beats a
            // round trip that comes back as Telegram's own "Unauthorized".
            if !token.split_once(':').is_some_and(|(id, secret)| {
                !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) && secret.len() > 8
            }) {
                return Err(
                    "that does not look like a bot token — @BotFather gives you something like 8123456789:AAH…".into(),
                );
            }
            Some(token)
        }
        _ => None,
    };

    let api = Api::connect(target).await?;

    let show = |status: &Value| {
        let connected = bool_at(status, &["connected"]);
        let enabled = bool_at(status, &["enabled"]);
        let configured = bool_at(status, &["configured"]);
        let bot = str_at(status, &["bot"]);
        let (mark, detail) = match (configured, enabled, connected) {
            (false, _, _) => (Mark::None, "no bot token yet".to_string()),
            (_, false, _) => (Mark::None, "off".to_string()),
            (_, _, true) => (
                Mark::Good,
                if bot.is_empty() {
                    "connected".to_string()
                } else {
                    format!("connected as @{bot}")
                },
            ),
            (_, _, false) => (Mark::Bad, "not connected".to_string()),
        };
        ui.row("telegram", mark, &detail, "");
        if let Some(err) = status["last_error"].as_str().filter(|e| !e.is_empty()) {
            ui.warn(err);
        }
        let paired = status["paired_chats"].as_u64().unwrap_or(0);
        ui.field("paired chats", &paired.to_string());
    };

    match cmd {
        Telegram::Off => {
            // The token stays: switching the bridge off is not the same as
            // throwing away the bot, and `pair reset` is the one that forgets.
            let status = api
                .post("/api/gateway/configure", json!({ "enabled": false }))
                .await?;
            if json_out {
                ui.json(&status);
                return Ok(EXIT_OK);
            }
            println!("{}", ui.yellow("telegram off"));
            ui.note("The token is kept — `wired telegram on` reconnects.");
            Ok(EXIT_OK)
        }

        Telegram::Show => {
            let status = api.get("/api/gateway/status").await?;
            if json_out {
                ui.json(&status);
                return Ok(EXIT_OK);
            }
            show(&status);
            if !bool_at(&status, &["configured"]) {
                ui.note("No bot yet: message @BotFather in Telegram, /newbot, then");
                ui.note("  wired telegram on");
            } else if !bool_at(&status, &["enabled"]) {
                ui.note("Switch it back on with `wired telegram on`.");
            } else if status["paired_chats"].as_u64().unwrap_or(0) == 0 {
                ui.note("Message the bot from your phone, then `wired pair`.");
            }
            Ok(
                if bool_at(&status, &["connected"]) || !bool_at(&status, &["enabled"]) {
                    EXIT_OK
                } else {
                    EXIT_UNHEALTHY
                },
            )
        }

        Telegram::On(_) => {
            let token = token.expect("On always resolves a token above");
            let status = api
                .post(
                    "/api/gateway/configure",
                    json!({ "bot_token": token, "enabled": true }),
                )
                .await?;
            if json_out {
                ui.json(&status);
                return Ok(EXIT_OK);
            }

            // Connecting is a round trip to Telegram, so the answer to the
            // configure call is usually "not yet" rather than "no".
            let mut status = status;
            for _ in 0..10 {
                if bool_at(&status, &["connected"]) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(600)).await;
                status = api.get("/api/gateway/status").await?;
            }

            show(&status);
            if bool_at(&status, &["connected"]) {
                ui.note("Now message the bot from your phone — it answers with a code.");
                ui.note("Then `wired pair` to see it, and `wired pair approve <code>`.");
                Ok(EXIT_OK)
            } else {
                ui.note("Saved, but not connected yet. `wired telegram` to check again.");
                Ok(EXIT_UNHEALTHY)
            }
        }
    }
}

pub async fn pair(ui: &Ui, target: &Target, cmd: &Pair, json_out: bool) -> Result<i32> {
    let api = Api::connect(target).await?;
    match cmd {
        Pair::Approve(code) => {
            api.post("/api/gateway/pairings/approve", json!({ "code": code }))
                .await?;
            println!("{}", ui.green("paired"));
        }
        Pair::Deny(code) => {
            api.post("/api/gateway/pairings/deny", json!({ "code": code }))
                .await?;
            println!("{}", ui.yellow("denied"));
        }
        Pair::Unpair(chat) => {
            api.post("/api/gateway/unpair", json!({ "chat": chat }))
                .await?;
            println!("{} {chat}", ui.green("unpaired"));
        }
        Pair::Reset { yes } => {
            // Show what is about to be thrown away, not just a warning — the
            // bot token cannot be recovered from here, only from BotFather.
            let gateway = api.get("/api/gateway/status").await?;
            let bot = match str_at(&gateway, &["bot"]) {
                "" => "the bot".to_string(),
                name => name.to_string(),
            };
            let chats = gateway["paired_chats"].as_u64().unwrap_or(0);
            if !confirm(
                ui,
                *yes,
                &format!(
                    "Forget {bot}'s token and unpair {chats} chat{}? \
                     The token can only be recovered from BotFather.",
                    if chats == 1 { "" } else { "s" }
                ),
            )? {
                println!("{}", ui.dim("left alone"));
                return Ok(EXIT_OK);
            }
            api.post("/api/gateway/reset", json!({})).await?;
            println!(
                "{}",
                ui.green("chat bridge reset — token cleared, all chats unpaired")
            );
            ui.note("revoke the old token in BotFather (/revoke), then paste a new one");
        }
        Pair::List => {
            let gateway = api.get("/api/gateway/status").await?;
            if json_out {
                ui.json(&gateway);
                return Ok(EXIT_OK);
            }

            let connected = bool_at(&gateway, &["connected"]);
            ui.row(
                "telegram",
                if !bool_at(&gateway, &["enabled"]) {
                    Mark::None
                } else if connected {
                    Mark::Good
                } else {
                    Mark::Bad
                },
                match (bool_at(&gateway, &["enabled"]), connected) {
                    (false, _) => "off",
                    (true, true) => "connected",
                    (true, false) => "disconnected",
                },
                str_at(&gateway, &["bot"]),
            );
            ui.field(
                "paired chats",
                &gateway["paired_chats"].as_u64().unwrap_or(0).to_string(),
            );
            if let Some(error) = gateway["last_error"].as_str() {
                ui.warn(error);
            }

            let pending = gateway["pending"].as_array().cloned().unwrap_or_default();
            if pending.is_empty() {
                ui.note("no pairing requests waiting");
            } else {
                ui.heading("Waiting to pair");
                for request in &pending {
                    ui.row(
                        str_at(request, &["code"]),
                        Mark::Unknown,
                        str_at(request, &["display"]),
                        &format!(
                            "chat {} · expires in {}",
                            request["chat"].as_i64().unwrap_or(0),
                            human_duration(request["expires_in"].as_i64().unwrap_or(0))
                        ),
                    );
                }
                ui.note("`wired pair approve <code>` to let one in");
            }
        }
    }
    Ok(EXIT_OK)
}

pub async fn schedule(ui: &Ui, target: &Target, cmd: &ScheduleCmd, json_out: bool) -> Result<i32> {
    let api = Api::connect(target).await?;
    match cmd {
        ScheduleCmd::Run(id) => {
            api.post("/api/schedules/run", json!({ "id": id })).await?;
            println!("{} {id}", ui.green("running"));
        }
        ScheduleCmd::Delete(id) => {
            api.post("/api/schedules/delete", json!({ "id": id }))
                .await?;
            println!("{} {id}", ui.green("deleted"));
        }
        ScheduleCmd::List => {
            let listing = api.get("/api/schedules").await?;
            if json_out {
                ui.json(&listing);
                return Ok(EXIT_OK);
            }
            let schedules = listing["schedules"].as_array().cloned().unwrap_or_default();
            if schedules.is_empty() {
                ui.note("no scheduled tasks");
                return Ok(EXIT_OK);
            }
            for schedule in &schedules {
                let enabled = schedule["enabled"].as_bool().unwrap_or(false);
                ui.row(
                    str_at(schedule, &["id"]),
                    if enabled { Mark::Good } else { Mark::None },
                    str_at(schedule, &["name"]),
                    &format!(
                        "{} · next {}",
                        str_at(schedule, &["when_readable"]),
                        schedule["next_readable"].as_str().unwrap_or("—")
                    ),
                );
            }
        }
    }
    Ok(EXIT_OK)
}

// ── remotes ─────────────────────────────────────────────────────────────

pub fn remote(ui: &Ui, config: &mut Config, cmd: &RemoteCmd) -> Result<i32> {
    match cmd {
        RemoteCmd::List => {
            if config.remotes.is_empty() {
                ui.note("no remotes — `wired remote add pilot ubuntu@host`");
                return Ok(EXIT_OK);
            }
            for (name, remote) in &config.remotes {
                let is_default = config.default.as_deref() == Some(name.as_str());
                ui.row(
                    name,
                    if is_default { Mark::Good } else { Mark::None },
                    &remote.host,
                    &format!(
                        "api :{}{}{}",
                        remote.port,
                        match remote.ssh_port {
                            Some(p) => format!(" · ssh :{p}"),
                            None => String::new(),
                        },
                        if is_default { " · default" } else { "" }
                    ),
                );
            }
        }
        RemoteCmd::Add {
            name,
            host,
            port,
            ssh_port,
            token,
            unit,
        } => {
            config.remotes.insert(
                name.clone(),
                Remote {
                    host: host.clone(),
                    port: *port,
                    ssh_port: *ssh_port,
                    token: token.clone(),
                    unit: unit.clone(),
                },
            );
            config.save()?;
            println!("{} {name} → {host}", ui.green("added"));
            ui.note(&format!("try it: wired --remote {name} status"));
        }
        RemoteCmd::Remove(name) => {
            if config.remotes.remove(name).is_none() {
                return Err(format!("no remote called `{name}`"));
            }
            if config.default.as_deref() == Some(name.as_str()) {
                config.default = None;
            }
            config.save()?;
            println!("{} {name}", ui.green("removed"));
        }
        RemoteCmd::Default(name) => {
            if !config.remotes.contains_key(name) {
                return Err(format!("no remote called `{name}`"));
            }
            config.default = Some(name.clone());
            config.save()?;
            println!("{} {name}", ui.green("default is now"));
            ui.note("`wired --url http://127.0.0.1:8000 status` still reaches this machine");
        }
    }
    Ok(EXIT_OK)
}

/// `fetch_and_swap` replaces the binaries a live service runs, so the thing
/// worth testing is not the happy path but every way it can refuse: whatever it
/// rejects must leave the installed pair exactly as it was.
#[cfg(test)]
mod update_tests {
    use super::*;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    /// A one-shot HTTP server. Serves `body` for the first request and stops,
    /// which is all a single download needs.
    fn serve_once(status: &'static str, body: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                use std::io::Read as _;
                let mut scratch = [0u8; 2048];
                let _ = sock.read(&mut scratch);
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes());
                let _ = sock.write_all(&body);
                let _ = sock.flush();
            }
        });
        format!("http://127.0.0.1:{port}/server.tar.gz")
    }

    /// A stand-in binary: a shell script that reports the version it was given.
    fn fake_binary(path: &Path, version: &str, runs: bool) {
        let body = if runs {
            format!("#!/bin/sh\necho \"wired {version}\"\n")
        } else {
            // A binary the loader would refuse looks like this from the outside.
            "#!/bin/sh\necho 'GLIBC_2.35 not found' >&2\nexit 127\n".to_string()
        };
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    struct Fixture {
        _root: tempdir::TempDir,
        bin: PathBuf,
        work: PathBuf,
    }

    /// An install holding version 1.0.2 of both binaries.
    fn installed() -> Fixture {
        let root = tempdir::TempDir::new();
        let bin = root.path().join("bin");
        let work = bin.join(".wired-update");
        std::fs::create_dir_all(&work).unwrap();
        for name in ["wired", "wired-backend"] {
            fake_binary(&bin.join(name), "1.0.2", true);
        }
        Fixture {
            _root: root,
            bin,
            work,
        }
    }

    fn tarball(entries: &[(&str, &str, bool)]) -> Vec<u8> {
        let dir = tempdir::TempDir::new();
        for (name, version, runs) in entries {
            fake_binary(&dir.path().join(name), version, *runs);
        }
        let out = dir.path().join("t.tar.gz");
        let mut cmd = std::process::Command::new("tar");
        cmd.arg("-czf").arg(&out).arg("-C").arg(dir.path());
        for (name, _, _) in entries {
            cmd.arg(name);
        }
        assert!(cmd.status().unwrap().success());
        std::fs::read(&out).unwrap()
    }

    fn installed_version(bin: &Path, name: &str) -> String {
        let out = std::process::Command::new(bin.join(name))
            .arg("--version")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn run(fx: &Fixture, url: &str, expected: &str) -> Result<String> {
        let ui = Ui::new(true);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fetch_and_swap(&ui, url, expected, &fx.bin, &fx.work))
    }

    #[test]
    fn a_good_download_replaces_both_binaries() {
        let fx = installed();
        let url = serve_once(
            "200 OK",
            tarball(&[("wired", "1.0.3", true), ("wired-backend", "1.0.3", true)]),
        );
        let reported = run(&fx, &url, "1.0.3").unwrap();
        assert_eq!(reported, "wired 1.0.3");
        assert_eq!(installed_version(&fx.bin, "wired"), "wired 1.0.3");
        assert_eq!(installed_version(&fx.bin, "wired-backend"), "wired 1.0.3");
    }

    #[test]
    fn a_version_the_manifest_did_not_promise_is_refused() {
        let fx = installed();
        // The tarball is real and runs — it is simply not what was advertised,
        // which is how a stale alias or a mis-keyed manifest would look.
        let url = serve_once(
            "200 OK",
            tarball(&[("wired", "0.9.0", true), ("wired-backend", "0.9.0", true)]),
        );
        let err = run(&fx, &url, "1.0.3").unwrap_err();
        assert!(err.to_string().contains("manifest promised"), "{err}");
        assert_eq!(installed_version(&fx.bin, "wired"), "wired 1.0.2");
    }

    #[test]
    fn a_binary_that_cannot_run_here_is_refused() {
        let fx = installed();
        let url = serve_once(
            "200 OK",
            tarball(&[("wired", "1.0.3", false), ("wired-backend", "1.0.3", true)]),
        );
        let err = run(&fx, &url, "1.0.3").unwrap_err();
        assert!(err.to_string().contains("would not run"), "{err}");
        assert_eq!(installed_version(&fx.bin, "wired"), "wired 1.0.2");
    }

    #[test]
    fn an_archive_missing_a_binary_is_refused() {
        let fx = installed();
        let url = serve_once("200 OK", tarball(&[("wired", "1.0.3", true)]));
        let err = run(&fx, &url, "1.0.3").unwrap_err();
        assert!(err.to_string().contains("did not contain"), "{err}");
        assert_eq!(installed_version(&fx.bin, "wired"), "wired 1.0.2");
    }

    #[test]
    fn a_failed_download_leaves_the_install_alone() {
        let fx = installed();
        let url = serve_once("404 Not Found", b"nope".to_vec());
        let err = run(&fx, &url, "1.0.3").unwrap_err();
        assert!(err.to_string().contains("404"), "{err}");
        assert_eq!(installed_version(&fx.bin, "wired"), "wired 1.0.2");
    }

    #[test]
    fn something_that_is_not_an_archive_is_refused() {
        let fx = installed();
        let url = serve_once("200 OK", b"this is not a gzip stream".to_vec());
        let err = run(&fx, &url, "1.0.3").unwrap_err();
        assert!(err.to_string().contains("did not unpack"), "{err}");
        assert_eq!(installed_version(&fx.bin, "wired"), "wired 1.0.2");
    }

    /// No `tempfile` dependency for six tests; this is the whole of what they need.
    mod tempdir {
        use std::path::{Path, PathBuf};
        pub struct TempDir(PathBuf);
        impl TempDir {
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                // A counter, not a timestamp. These tests run as parallel
                // threads of one process, and the clock is not granular enough
                // to separate two of them — a collision meant two fixtures
                // shared a directory and one's Drop deleted the other's install,
                // which showed up as a version assertion failing about one run
                // in ten.
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let path = std::env::temp_dir()
                    .join(format!("wired-update-test-{}-{n}", std::process::id()));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
