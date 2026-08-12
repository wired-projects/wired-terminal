//! 24/7 personal-assistant supervisor.
//!
//! Keeps an agent CLI session alive in the shared PTY and restarts
//! it when the process exits (so you can command it all day via REST).

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};

use crate::config::int_env;
use crate::providers::{default_assistant_provider, resolve_cmd, ASSISTANT_PROVIDERS};
use crate::pty::PtyManager;
use crate::settings_store;

#[derive(Debug, Clone, Serialize)]
pub struct AssistantConfig {
    pub provider: String,
    pub keep_alive: bool,
    /// Off by default: booting the API should not spawn an agent unless asked.
    pub auto_start: bool,
    pub cols: u16,
    pub rows: u16,
    pub restart_delay: Duration,
    pub max_restarts_per_hour: usize,
}

impl Default for AssistantConfig {
    fn default() -> Self {
        Self {
            provider: default_assistant_provider(),
            keep_alive: true,
            auto_start: false,
            cols: 100,
            rows: 32,
            restart_delay: Duration::from_secs(2),
            max_restarts_per_hour: 30,
        }
    }
}

/// Environment first, then `settings.json`, then the defaults — see
/// `settings_store` for why that order.
pub fn load_config_from_env() -> AssistantConfig {
    let mut cfg = AssistantConfig::default();
    let stored = settings_store::get();

    let provider =
        settings_store::string_or("WIRED_ASSISTANT_PROVIDER", stored.assistant.clone(), "")
            .trim()
            .to_ascii_lowercase();
    cfg.provider = if ASSISTANT_PROVIDERS.contains(&provider.as_str()) || provider == "shell" {
        provider
    } else {
        default_assistant_provider()
    };

    cfg.keep_alive = settings_store::flag_or("WIRED_ASSISTANT_KEEP_ALIVE", stored.always_on, true);
    cfg.auto_start =
        settings_store::flag_or("WIRED_ASSISTANT_AUTO_START", stored.auto_start, false);
    cfg.cols = int_env("WIRED_ASSISTANT_COLS", cfg.cols as i64).clamp(20, 500) as u16;
    cfg.rows = int_env("WIRED_ASSISTANT_ROWS", cfg.rows as i64).clamp(8, 200) as u16;
    cfg
}

struct Shared {
    config: Mutex<AssistantConfig>,
    runtime: Mutex<Runtime>,
    /// Wakes the watcher for shutdown instead of making it poll a flag.
    wake: Condvar,
}

struct Runtime {
    enabled: bool,
    stop: bool,
    watcher_running: bool,
    restarts: Vec<Instant>,
    last_error: Option<String>,
    started_at: Option<f64>,
}

#[derive(Clone)]
pub struct Assistant {
    manager: PtyManager,
    shared: Arc<Shared>,
}

impl Assistant {
    pub fn new(manager: PtyManager, config: AssistantConfig) -> Self {
        Self {
            manager,
            shared: Arc::new(Shared {
                config: Mutex::new(config),
                runtime: Mutex::new(Runtime {
                    enabled: false,
                    stop: false,
                    watcher_running: false,
                    restarts: Vec::new(),
                    last_error: None,
                    started_at: None,
                }),
                wake: Condvar::new(),
            }),
        }
    }

    pub fn config(&self) -> AssistantConfig {
        self.shared.config.lock().unwrap().clone()
    }

    pub fn status(&self) -> Value {
        let cfg = self.shared.config.lock().unwrap().clone();
        let rt = self.shared.runtime.lock().unwrap();
        json!({
            "enabled": rt.enabled,
            "keep_alive": cfg.keep_alive,
            "auto_start": cfg.auto_start,
            "provider": cfg.provider,
            "session_running": self.manager.running(),
            "session_provider": self.manager.provider(),
            "generation": self.manager.generation(),
            "started_at": rt.started_at,
            "last_error": rt.last_error,
            "restarts_last_hour": rt.restarts.iter().filter(|t| t.elapsed() < Duration::from_secs(3600)).count(),
        })
    }

    pub fn configure(
        &self,
        provider: Option<&str>,
        keep_alive: Option<bool>,
        auto_start: Option<bool>,
        cols: Option<u16>,
        rows: Option<u16>,
    ) -> Result<(), String> {
        let mut cfg = self.shared.config.lock().unwrap();
        if let Some(provider) = provider {
            let p = provider.trim().to_ascii_lowercase();
            if !ASSISTANT_PROVIDERS.contains(&p.as_str()) && p != "shell" {
                return Err(format!("provider must be one of {ASSISTANT_PROVIDERS:?}"));
            }
            if resolve_cmd(&p).is_none() {
                return Err(format!("CLI for provider '{p}' not found on PATH"));
            }
            cfg.provider = p;
        }
        if let Some(v) = keep_alive {
            cfg.keep_alive = v;
        }
        if let Some(v) = auto_start {
            cfg.auto_start = v;
        }
        if let Some(v) = cols {
            cfg.cols = v.clamp(20, 500);
        }
        if let Some(v) = rows {
            cfg.rows = v.clamp(8, 200);
        }
        Ok(())
    }

    /// Start supervisor + ensure an agent session is running. Blocking.
    pub fn enable(&self) -> Result<Value, String> {
        {
            let mut rt = self.shared.runtime.lock().unwrap();
            rt.enabled = true;
            rt.stop = false;
            rt.started_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs_f64());
            rt.last_error = None;
        }

        let result = self.ensure_session()?;

        let keep_alive = self.shared.config.lock().unwrap().keep_alive;
        let should_spawn = {
            let mut rt = self.shared.runtime.lock().unwrap();
            if keep_alive && !rt.watcher_running {
                rt.watcher_running = true;
                true
            } else {
                false
            }
        };
        if should_spawn {
            let me = self.clone();
            std::thread::Builder::new()
                .name("assistant-supervisor".into())
                .spawn(move || me.watch_loop())
                .map_err(|e| format!("could not start the supervisor: {e}"))?;
        }
        Ok(result)
    }

    pub fn disable(&self, kill_session: bool) {
        {
            let mut rt = self.shared.runtime.lock().unwrap();
            rt.enabled = false;
            rt.stop = true;
        }
        self.shared.wake.notify_all();
        if kill_session {
            self.manager.kill();
        }
    }

    /// Start preferred provider if not already running (or wrong provider).
    pub fn ensure_session(&self) -> Result<Value, String> {
        let cfg = self.shared.config.lock().unwrap().clone();

        if resolve_cmd(&cfg.provider).is_none() {
            let err = format!("Provider '{}' CLI not found", cfg.provider);
            self.shared.runtime.lock().unwrap().last_error = Some(err.clone());
            return Err(err);
        }

        if self.manager.running() && self.manager.provider().as_deref() == Some(&cfg.provider) {
            return Ok(json!({
                "status": "already_running",
                "provider": cfg.provider,
                "generation": self.manager.generation(),
            }));
        }

        match self.manager.start(&cfg.provider, cfg.cols, cfg.rows) {
            Ok(()) => {
                self.shared.runtime.lock().unwrap().last_error = None;
                Ok(json!({
                    "status": "started",
                    "provider": cfg.provider,
                    "generation": self.manager.generation(),
                }))
            }
            Err(e) => {
                self.shared.runtime.lock().unwrap().last_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Sleep unless someone asks us to stop. Returns true if we should quit.
    fn rest(&self, dur: Duration) -> bool {
        let rt = self.shared.runtime.lock().unwrap();
        let (rt, _) = self.shared.wake.wait_timeout(rt, dur).unwrap();
        rt.stop
    }

    fn watch_loop(self) {
        let provider = self.shared.config.lock().unwrap().provider.clone();
        tracing::info!(provider, "assistant supervisor watching");

        loop {
            {
                let rt = self.shared.runtime.lock().unwrap();
                if rt.stop {
                    break;
                }
            }
            let cfg = self.shared.config.lock().unwrap().clone();
            let enabled = self.shared.runtime.lock().unwrap().enabled;

            if !enabled || !cfg.keep_alive || self.manager.running() {
                if self.rest(Duration::from_secs(1)) {
                    break;
                }
                continue;
            }

            // Session died — restart if under rate limit.
            let over_limit = {
                let mut rt = self.shared.runtime.lock().unwrap();
                rt.restarts
                    .retain(|t| t.elapsed() < Duration::from_secs(3600));
                rt.restarts.len() >= cfg.max_restarts_per_hour
            };
            if over_limit {
                let msg = "restart rate limit hit; supervisor paused";
                self.shared.runtime.lock().unwrap().last_error = Some(msg.to_string());
                tracing::error!("{msg}");
                if self.rest(Duration::from_secs(30)) {
                    break;
                }
                continue;
            }

            tracing::warn!(delay = ?cfg.restart_delay, "agent session down; restarting");
            if self.rest(cfg.restart_delay) {
                break;
            }
            if !self.shared.runtime.lock().unwrap().enabled {
                break;
            }

            match self.manager.start(&cfg.provider, cfg.cols, cfg.rows) {
                Ok(()) => {
                    let mut rt = self.shared.runtime.lock().unwrap();
                    rt.restarts.push(Instant::now());
                    rt.last_error = None;
                    tracing::info!(
                        provider = cfg.provider,
                        generation = self.manager.generation(),
                        "restarted"
                    );
                }
                Err(e) => {
                    self.shared.runtime.lock().unwrap().last_error = Some(e.clone());
                    tracing::error!("failed to restart assistant: {e}");
                    if self.rest(Duration::from_secs(5)) {
                        break;
                    }
                }
            }
        }

        self.shared.runtime.lock().unwrap().watcher_running = false;
        tracing::info!("assistant supervisor stopped");
    }
}
