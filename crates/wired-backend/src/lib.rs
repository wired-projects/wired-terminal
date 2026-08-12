//! Wired backend — personal-assistant control plane for the agent CLIs.
//!
//! Architecture:
//!   • One shared PTY hosts claude|grok|codex|gemini|shell    (`pty`)
//!   • WebSocket streams raw PTY output to the desktop UI
//!   • REST injects multi-line tasks — the 24/7 remote control
//!   • One recorder turns the repainting screen into a transcript, and every
//!     consumer subscribes to it                             (`recorder`)
//!   • A chat bridge holds an outbound connection so a phone can reach it
//!     without a port, a tunnel or a token                   (`gateway`)
//!   • `Assistant` restarts the CLI if it dies               (`assistant`)
//!   • `Scheduler` fires tasks while nobody is watching      (`scheduler`)
//!
//! Two entry points share all of it: the `wired-backend` binary, and the
//! desktop app, which calls [`serve`] on its own runtime rather than shipping a
//! second executable.

pub mod agent_io;
pub mod assistant;
pub mod config;
pub mod diagnostics;
pub mod gateway;
pub mod keys;
pub mod mcp;
pub mod models;
pub mod paths;
pub mod providers;
pub mod pty;
pub mod recorder;
pub mod routes;
pub mod schedule;
pub mod scheduler;
pub mod secrets;
pub mod security;
pub mod settings_store;
pub mod setup;
pub mod terminal_clean;
pub mod transcript;
pub mod vt_screen;

use std::net::SocketAddr;

use assistant::{load_config_from_env, Assistant};
use config::{ConfigError, Settings};
use gateway::Gateway;
use providers::auto_approve_enabled;
use pty::PtyManager;
use recorder::{DayStore, Recorder};
use routes::AppState;
use scheduler::Scheduler;
use setup::Installer;

/// How far past the requested port to look before giving up and taking any free
/// one. Twenty is enough to step over a handful of other dev servers.
const PORT_SEARCH: u16 = 20;

/// Say out loud what this process will let a caller do.
fn log_posture(settings: &Settings) {
    tracing::info!(
        "listening on {}:{} ({})",
        settings.host,
        settings.port,
        if settings.is_loopback() {
            "loopback"
        } else {
            "NETWORK-REACHABLE"
        }
    );
    tracing::info!(
        "auth: {}",
        if settings.auth_required() {
            "bearer token required"
        } else {
            "none (open)"
        }
    );
    tracing::info!("logs: {}", diagnostics::log_file().display());
    if settings.allow_any_origin {
        tracing::warn!("CORS: any origin allowed (WIRED_ALLOWED_ORIGINS=*)");
    }
    if auto_approve_enabled() {
        tracing::warn!(
            "agent auto-approve is ON — the CLI will act without confirming. \
             Set WIRED_AGENT_AUTO_APPROVE=0 to require approvals."
        );
    }
    if !settings.is_loopback() && !settings.auth_required() {
        tracing::error!("exposed to the network with no token — WIRED_ALLOW_INSECURE was set");
    }
}

/// Build the state and router without binding a socket — for tests and for
/// embedding the API in another server.
pub fn build(settings: Settings) -> (AppState, axum::Router) {
    let cfg = load_config_from_env();
    let manager = PtyManager::new(cfg.cols, cfg.rows);
    let assistant = Assistant::new(manager.clone(), cfg);
    // `Settings::default()` leaves persistence off, so tests and embedders
    // never write a transcript or a schedule into a real user's directories.
    let persist = settings.persist;
    let store = persist.then(|| DayStore::new(paths::transcript_dir()));
    let recorder = Recorder::new(store);

    let state = AppState {
        settings,
        manager: manager.clone(),
        assistant,
        recorder: recorder.clone(),
        gateway: Gateway::new(),
        scheduler: Scheduler::new(persist),
        installer: Installer::new(),
    };

    // One recorder per process: it owns the only `TranscriptTail`, and every
    // reader — SSE, the chat bridge, the day store — sees the same rows.
    recorder.watch(manager);

    let router = routes::router(state.clone());
    (state, router)
}

/// Claim a socket, stepping past a busy port when that is allowed.
///
/// A non-coder cannot be asked which other program is on 8000, so the app moves
/// instead and tells its own window where it went. The server binary keeps the
/// old behaviour: its port is somebody's firewall rule.
pub async fn bind(
    mut settings: Settings,
) -> Result<(tokio::net::TcpListener, Settings), Box<dyn std::error::Error + Send + Sync>> {
    let wanted = settings.port;
    let mut last_error = None;

    let candidates: Vec<u16> = if settings.port_fallback {
        (wanted..wanted.saturating_add(PORT_SEARCH))
            .chain(std::iter::once(0))
            .collect()
    } else {
        vec![wanted]
    };

    for port in candidates {
        let addr: SocketAddr = format!("{}:{}", settings.host, port).parse()?;
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                let actual = listener.local_addr()?.port();
                if actual != wanted {
                    let holder = diagnostics::port_holder(wanted)
                        .map(|who| format!(" ({who} is using it)"))
                        .unwrap_or_default();
                    tracing::warn!("port {wanted} was taken{holder}; using {actual} instead");
                }
                settings.port = actual;
                return Ok((listener, settings));
            }
            Err(e) => last_error = Some(e),
        }
    }

    let error = last_error.expect("at least one candidate port");
    if let Some(who) = diagnostics::port_holder(wanted) {
        return Err(format!("Port {wanted} is already in use by {who}.").into());
    }
    Err(Box::new(error))
}

/// Run the API on an already-bound socket until asked to stop.
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    settings: Settings,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    log_posture(&settings);
    let (state, router) = build(settings);

    if state.assistant.config().auto_start {
        let assistant = state.assistant.clone();
        match tokio::task::spawn_blocking(move || assistant.enable()).await {
            Ok(Ok(_)) => tracing::info!(
                provider = state.assistant.config().provider,
                "auto-started assistant"
            ),
            Ok(Err(e)) => tracing::warn!("auto-start failed: {e}"),
            Err(e) => tracing::warn!("auto-start panicked: {e}"),
        }
    }

    // Both read their configuration at start and are restarted by the routes
    // that change it, so neither needs the process to bounce.
    state.gateway.restart(state.hub());
    state.scheduler.clone().watch(state.hub());

    let manager = state.manager.clone();
    let assistant = state.assistant.clone();
    let gateway = state.gateway.clone();

    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.await;
            gateway.stop();
            // Take the agent down with us: an orphaned CLI on a dead PTY is
            // invisible and unkillable from the API that started it.
            assistant.disable(true);
            manager.kill();
        })
        .await?;
    Ok(())
}

/// Run the API until the process is asked to stop.
///
/// `shutdown` lets an embedder (the desktop app) stop the server without
/// killing the process; the binary passes Ctrl-C.
pub async fn serve(
    settings: Settings,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (listener, settings) = bind(settings).await?;
    serve_on(listener, settings, shutdown).await
}

/// Read settings from the environment and `settings.json`, refusing unsafe
/// combinations.
pub fn settings_from_env() -> Result<Settings, ConfigError> {
    config::load_settings()
}

/// Install the default log format, honouring `WIRED_LOG_LEVEL`.
///
/// Also writes to a file. A `.app` launch has no terminal, so stdout-only
/// logging meant that the one artefact worth asking a stuck user for did not
/// exist — see `diagnostics::log_file`.
pub fn init_logging() {
    use tracing_subscriber::fmt::writer::MakeWriterExt;

    let level = std::env::var("WIRED_LOG_LEVEL").unwrap_or_else(|_| "info".into());
    let filter = tracing_subscriber::EnvFilter::try_new(level.to_lowercase())
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let dir = paths::log_dir();
    if paths::ensure_dir(&dir).is_ok() {
        let file = tracing_appender::rolling::daily(&dir, "wired.log");
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            // Colour codes in a file are noise in the one place the text has to
            // be readable by whoever is helping.
            .with_ansi(false)
            .with_writer(std::io::stdout.and(file))
            .try_init();
        return;
    }

    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
