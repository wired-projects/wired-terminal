//! The shared pseudo-terminal that hosts the agent CLI.
//!
//! One PTY at a time. A reader thread pumps its output into three consumers:
//!
//!   * a broadcast channel (the desktop UI's live terminal WebSocket)
//!   * a bounded ring buffer, so a poll/long-poll API can replay a delta
//!   * a `VtScreen`, because the CLIs are full-screen TUIs — the readable text
//!     endpoints need the *rendered screen*, not the byte stream that painted it
//!
//! `generation` increments on every start so a late chunk from a dead session
//! can never be attributed to its replacement.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast;

use crate::providers::{agent_cwd, resolve_cmd};
use crate::terminal_clean::clean_terminal_bytes;
use crate::vt_screen::VtScreen;

const READ_CHUNK: usize = 8192;
/// Bounded replay window. Worst case ~8 MB of raw PTY bytes; the readable
/// endpoints render from the VT screen instead, so this only serves the
/// WebSocket replay and the raw debug delta.
const RECENT_CHUNKS: usize = 1000;
pub const DEFAULT_MAX_CHARS: usize = 80_000;

#[derive(Debug, Clone, Serialize)]
pub struct Output {
    pub since: u64,
    pub seq: u64,
    pub generation: u64,
    pub running: bool,
    pub provider: Option<String>,
    pub bytes: usize,
    pub truncated: bool,
    pub text: String,
    pub chunks: usize,
    pub mode: &'static str,
}

pub struct Snapshot {
    pub running: bool,
    pub provider: Option<String>,
    pub generation: u64,
    /// WebSocket replay: (generation, raw bytes)
    pub recent: Vec<(u64, Vec<u8>)>,
    pub seq: u64,
}

struct Session {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

struct State {
    generation: u64,
    seq: u64,
    provider: Option<String>,
    /// Per-session secret handed to the child in its environment. Lets the
    /// MCP layer recognise a call coming from the agent we are supervising —
    /// see `routes::reject_self_call`.
    nonce: Option<String>,
    recent: VecDeque<(u64, u64, Vec<u8>)>,
    vt: VtScreen,
    cols: u16,
    rows: u16,
    session: Option<Session>,
}

struct Inner {
    state: Mutex<State>,
    /// Signalled on every chunk and on session death, so `wait_output` can
    /// block instead of polling.
    output: Condvar,
    events: broadcast::Sender<String>,
}

#[derive(Clone)]
pub struct PtyManager {
    inner: Arc<Inner>,
}

impl PtyManager {
    pub fn new(cols: u16, rows: u16) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    generation: 0,
                    seq: 0,
                    provider: None,
                    nonce: None,
                    recent: VecDeque::with_capacity(RECENT_CHUNKS),
                    vt: VtScreen::new(cols, rows),
                    cols,
                    rows,
                    session: None,
                }),
                output: Condvar::new(),
                events,
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.inner.events.subscribe()
    }

    // ── lifecycle ───────────────────────────────────────────────────────

    /// Replace any running session with a fresh PTY for `provider`.
    ///
    /// Blocking: forks a process and may reap the previous one. Call it from
    /// `spawn_blocking`, never straight off the async runtime.
    pub fn start(&self, provider: &str, cols: u16, rows: u16) -> Result<(), String> {
        let cmd = resolve_cmd(provider).ok_or_else(|| {
            format!(
                "Provider '{provider}' CLI not found. Install one of {} and ensure PATH.",
                crate::providers::ASSISTANT_PROVIDERS.join(", ")
            )
        })?;
        self.start_command(provider, cmd, cols, rows)
    }

    /// Start an explicit command under `provider`'s identity.
    ///
    /// The setup wizard uses this to run the CLI's interactive login with no
    /// auto-approve flags attached — the one step that genuinely cannot be
    /// scripted, run in a terminal the app already owns.
    pub fn start_command(
        &self,
        provider: &str,
        cmd: Vec<String>,
        cols: u16,
        rows: u16,
    ) -> Result<(), String> {
        if cmd.is_empty() {
            return Err("No command to run".to_string());
        }

        self.kill();

        let cols = cols.clamp(20, 500);
        let rows = rows.clamp(8, 200);

        let pty = portable_pty::native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("Could not open a PTY: {e}"))?;

        let mut builder = CommandBuilder::new(&cmd[0]);
        for arg in &cmd[1..] {
            builder.arg(arg);
        }
        builder.cwd(agent_cwd());
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");
        // The agent inherits this; an MCP client that presents it is this very
        // session, talking to the API that is running it.
        let nonce = new_nonce();
        builder.env("WIRED_SESSION_NONCE", &nonce);

        let child = pty
            .slave
            .spawn_command(builder)
            .map_err(|e| format!("Could not start {provider}: {e}"))?;
        // Dropping the slave here is what lets the master see EOF when the
        // child exits; holding it open would hang the reader thread forever.
        drop(pty.slave);

        let reader = pty
            .master
            .try_clone_reader()
            .map_err(|e| format!("Could not read the PTY: {e}"))?;
        let writer = pty
            .master
            .take_writer()
            .map_err(|e| format!("Could not write to the PTY: {e}"))?;

        let gen = {
            let mut state = self.inner.state.lock().unwrap();
            state.generation += 1;
            state.provider = Some(provider.to_string());
            state.nonce = Some(nonce);
            state.recent.clear();
            state.cols = cols;
            state.rows = rows;
            state.vt.reset(cols, rows);
            state.session = Some(Session {
                master: pty.master,
                writer,
                child,
            });
            state.generation
        };

        let inner = Arc::clone(&self.inner);
        std::thread::Builder::new()
            .name(format!("pty-read-{gen}"))
            .spawn(move || read_loop(inner, gen, reader))
            .map_err(|e| format!("Could not start the reader thread: {e}"))?;

        tracing::info!(provider, generation = gen, "PTY started");
        Ok(())
    }

    /// Stop the session. Safe to call when nothing is running.
    pub fn kill(&self) {
        let session = {
            let mut state = self.inner.state.lock().unwrap();
            state.provider = None;
            state.nonce = None;
            state.recent.clear();
            state.session.take()
        };
        self.inner.output.notify_all();

        let Some(mut session) = session else {
            return;
        };

        // Ask first: the CLIs flush state on SIGTERM. Signal the whole group —
        // a PTY child leads its own session and may have spawned helpers.
        let pid = session.child.process_id();
        #[cfg(unix)]
        if let Some(pid) = pid {
            unsafe {
                let pgid = libc::getpgid(pid as i32);
                if pgid > 0 {
                    libc::kill(-pgid, libc::SIGTERM);
                } else {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
        }

        if !wait_for_exit(&mut session.child, Duration::from_millis(1500)) {
            let _ = session.child.kill();
            if !wait_for_exit(&mut session.child, Duration::from_millis(2000)) {
                tracing::warn!(?pid, "PTY child still alive after kill; abandoning");
            }
        }

        // Closing the master last: the reader thread sees EOF and retires.
        drop(session.writer);
        drop(session.master);
    }

    // ── io ──────────────────────────────────────────────────────────────

    pub fn write(&self, data: &[u8]) -> Result<(), String> {
        let mut state = self.inner.state.lock().unwrap();
        let session = state
            .session
            .as_mut()
            .ok_or_else(|| "No active PTY session".to_string())?;
        session
            .writer
            .write_all(data)
            .and_then(|_| session.writer.flush())
            .map_err(|e| format!("PTY write failed: {e}"))
    }

    pub fn set_size(&self, cols: u16, rows: u16) {
        let cols = cols.clamp(20, 500);
        let rows = rows.clamp(8, 200);
        let mut state = self.inner.state.lock().unwrap();
        state.cols = cols;
        state.rows = rows;
        state.vt.resize(cols, rows);
        if let Some(session) = state.session.as_ref() {
            let _ = session.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    // ── reads ───────────────────────────────────────────────────────────

    pub fn running(&self) -> bool {
        self.inner.state.lock().unwrap().session.is_some()
    }

    pub fn provider(&self) -> Option<String> {
        self.inner.state.lock().unwrap().provider.clone()
    }

    /// The live session's nonce, if a session is running.
    pub fn session_nonce(&self) -> Option<String> {
        self.inner.state.lock().unwrap().nonce.clone()
    }

    pub fn generation(&self) -> u64 {
        self.inner.state.lock().unwrap().generation
    }

    pub fn current_seq(&self) -> u64 {
        self.inner.state.lock().unwrap().seq
    }

    pub fn screen_text(&self) -> String {
        self.inner.state.lock().unwrap().vt.dump(true)
    }

    /// Readable terminal snapshot.
    ///
    /// `plain` renders the virtual screen (correct for the agent CLI TUIs);
    /// otherwise the raw bytes since `since` are returned, for debugging.
    pub fn get_output(&self, since: u64, plain: bool, keep_chrome: bool) -> Output {
        let (raw, seq, generation, running, provider, screen) = {
            let state = self.inner.state.lock().unwrap();
            let raw: Vec<u8> = state
                .recent
                .iter()
                .filter(|(s, _, _)| *s > since)
                .flat_map(|(_, _, b)| b.iter().copied())
                .collect();
            let chunks = state.recent.iter().filter(|(s, _, _)| *s > since).count();
            (
                (raw, chunks),
                state.seq,
                state.generation,
                state.session.is_some(),
                state.provider.clone(),
                if plain {
                    Some(state.vt.dump(true))
                } else {
                    None
                },
            )
        };
        let (raw, chunks) = raw;

        let mut text = if !plain {
            String::from_utf8_lossy(&raw).into_owned()
        } else if keep_chrome {
            // Lighter strip (debug) from the raw delta rather than the screen.
            if raw.is_empty() {
                screen.clone().unwrap_or_default()
            } else {
                clean_terminal_bytes(&raw, true)
            }
        } else {
            screen.clone().unwrap_or_default()
        };

        let truncated = text.chars().count() > DEFAULT_MAX_CHARS;
        if truncated {
            let skip = text.chars().count() - DEFAULT_MAX_CHARS;
            text = text.chars().skip(skip).collect();
        }

        Output {
            since,
            seq,
            generation,
            running,
            provider,
            bytes: if plain { text.len() } else { raw.len() },
            truncated,
            text,
            chunks,
            mode: if plain { "vt-screen" } else { "raw" },
        }
    }

    pub fn get_output_full(&self, plain: bool, keep_chrome: bool) -> Output {
        self.get_output(0, plain, keep_chrome)
    }

    /// Block until the PTY produces output and then falls quiet.
    ///
    /// Returns as soon as `idle` passes with no new data (the agent has
    /// finished a thought), when the session ends, or at `timeout`. Blocking —
    /// run it on `spawn_blocking`.
    pub fn wait_output(
        &self,
        since: u64,
        timeout: Duration,
        idle: Duration,
        plain: bool,
    ) -> Output {
        let deadline = Instant::now() + timeout;
        let mut last_activity = Instant::now();
        let mut saw_data = false;
        let mut cursor = since;

        let mut state = self.inner.state.lock().unwrap();
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline - now;

            let newest = state
                .recent
                .iter()
                .filter(|(s, _, _)| *s > cursor)
                .map(|(s, _, _)| *s)
                .next_back();
            if let Some(seq) = newest {
                cursor = seq;
                saw_data = true;
                last_activity = Instant::now();
            }

            let running = state.session.is_some();
            if saw_data && last_activity.elapsed() >= idle {
                break;
            }
            if !running {
                break;
            }

            let mut wait_for = remaining.min(Duration::from_millis(250));
            if saw_data {
                wait_for = wait_for.min(idle.saturating_sub(last_activity.elapsed()));
            }
            let wait_for = wait_for.max(Duration::from_millis(50));
            let (guard, _) = self.inner.output.wait_timeout(state, wait_for).unwrap();
            state = guard;
        }
        drop(state);

        self.get_output(since, plain, false)
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.inner.state.lock().unwrap();
        Snapshot {
            running: state.session.is_some(),
            provider: state.provider.clone(),
            generation: state.generation,
            recent: state
                .recent
                .iter()
                .map(|(_, g, b)| (*g, b.clone()))
                .collect(),
            seq: state.seq,
        }
    }
}

fn new_nonce() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..24)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

fn wait_for_exit(child: &mut Box<dyn Child + Send + Sync>, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Err(_) => return true, // already reaped
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read_loop(inner: Arc<Inner>, gen: u64, mut reader: Box<dyn Read + Send>) {
    let mut buf = vec![0u8; READ_CHUNK];

    loop {
        // A read on the master blocks until the child writes or exits, so the
        // generation check happens on the way out rather than in a poll loop.
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let chunk = buf[..n].to_vec();

        {
            let mut state = inner.state.lock().unwrap();
            if state.generation != gen {
                break;
            }
            state.seq += 1;
            let seq = state.seq;
            if state.recent.len() == RECENT_CHUNKS {
                state.recent.pop_front();
            }
            state.recent.push_back((seq, gen, chunk.clone()));
            state.vt.feed(&chunk);
        }
        inner.output.notify_all();

        let _ = inner.events.send(
            json!({
                "type": "data",
                "generation": gen,
                "chunk": base64::engine::general_purpose::STANDARD.encode(&chunk),
            })
            .to_string(),
        );
    }

    let _ = inner
        .events
        .send(json!({"type": "exit", "generation": gen}).to_string());

    {
        let mut state = inner.state.lock().unwrap();
        if state.generation == gen {
            state.session = None;
        }
    }
    inner.output.notify_all();
    tracing::info!(generation = gen, "PTY exit");
}
