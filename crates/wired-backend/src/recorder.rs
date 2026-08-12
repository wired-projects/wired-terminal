//! One transcript, many readers.
//!
//! `transcript.rs` turns a repainting TUI into an append-only conversation, but
//! it can only do that once: a `TranscriptTail` per SSE client would give each
//! of them a different idea of what had already been sent, and none of them
//! could write to a shared history. So a single background task owns the tail
//! and everything else subscribes:
//!
//!   • the SSE endpoint and `/reader`   — the live view
//!   • the chat gateway                 — the same conversation, in Telegram
//!   • the day store                    — "what did you do last night?"
//!   • the pending-approval flag        — the badge, and the inline buttons
//!
//! Persistence is newline-delimited JSON, one file per local day. It is the
//! least code that survives a restart, stays greppable, and can be deleted a
//! day at a time.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::paths;
use crate::pty::PtyManager;
use crate::transcript::{Kind, TranscriptTail};

/// Replay window for a client that connects mid-session. Two thousand rows is
/// what the panel itself keeps, so a reconnect looks like nothing happened.
const REPLAY: usize = 2000;
const DEFAULT_RETENTION_DAYS: i64 = 30;

/// How long the tail will sit on a screen that never falls quiet.
///
/// There is no scrollback — history is reconstructed from successive dumps of a
/// 32-row viewport — so anything that scrolls past between two samples is gone
/// for good. A working CLI repaints its spinner every second and so never goes
/// idle, which made this the *only* limit that applied, and three seconds of
/// output is comfortably more than a viewport: headings arrived on the phone
/// with the list underneath them missing.
const SAMPLE_CAP: Duration = Duration::from_millis(800);
/// Quiet long enough that the frame has settled. Shorter than the cap, so a
/// finished answer still lands promptly.
const SAMPLE_IDLE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    /// Something the agent said.
    Text,
    /// Something the user said.
    User,
    /// The agent is blocked, waiting for a yes or a no.
    Prompt,
    /// A standing warning the CLI pinned to its own screen.
    Notice,
    /// Session boundary — the CLI restarted.
    Session,
    /// Whether there is a session at all.
    Status,
    /// Wired itself talking: a scheduled run, a pairing request.
    System,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Text => "text",
            EventKind::User => "user",
            EventKind::Prompt => "prompt",
            EventKind::Notice => "notice",
            EventKind::Session => "session",
            EventKind::Status => "status",
            EventKind::System => "system",
        }
    }

    fn from_row(kind: Kind) -> Self {
        match kind {
            Kind::Text => EventKind::Text,
            Kind::User => EventKind::User,
            Kind::Prompt => EventKind::Prompt,
            Kind::Notice => EventKind::Notice,
        }
    }

    /// Rows worth keeping overnight. Session and status markers describe a
    /// process, not a conversation, and re-reading them tomorrow tells nobody
    /// anything.
    fn worth_storing(self) -> bool {
        matches!(
            self,
            EventKind::Text | EventKind::User | EventKind::Prompt | EventKind::System
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    /// Unix seconds. Float because the UI groups by minute and sorts by this.
    pub ts: f64,
    pub kind: EventKind,
    pub text: String,
    #[serde(default)]
    pub generation: u64,
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ── the day store ───────────────────────────────────────────────────────

/// Append-only NDJSON, one file per local day.
#[derive(Clone)]
pub struct DayStore {
    dir: PathBuf,
}

impl DayStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path_for(&self, day: &str) -> PathBuf {
        self.dir.join(format!("{day}.ndjson"))
    }

    fn today() -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    }

    /// A day name we are willing to turn into a path. Rejects `..` and anything
    /// else that is not exactly `YYYY-MM-DD`, because this value arrives from a
    /// query string.
    fn valid_day(day: &str) -> bool {
        day.len() == 10
            && day.as_bytes().iter().enumerate().all(|(i, b)| match i {
                4 | 7 => *b == b'-',
                _ => b.is_ascii_digit(),
            })
    }

    fn append(&self, event: &Event) {
        let Ok(line) = serde_json::to_string(event) else {
            return;
        };
        if paths::ensure_dir(&self.dir).is_err() {
            return;
        }
        let path = self.path_for(&Self::today());
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        // A history nobody can write is not worth stopping the assistant over:
        // log it once per failure and carry on serving the live transcript.
        match options.open(&path) {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{line}") {
                    tracing::warn!("could not append to {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("could not open {}: {e}", path.display()),
        }
    }

    /// Newest day first.
    pub fn days(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut days: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_suffix(".ndjson")
                    .filter(|d| Self::valid_day(d))
                    .map(str::to_string)
            })
            .collect();
        days.sort_unstable_by(|a, b| b.cmp(a));
        days
    }

    pub fn read_day(&self, day: &str, limit: usize) -> Vec<Event> {
        if !Self::valid_day(day) {
            return Vec::new();
        }
        let Ok(file) = std::fs::File::open(self.path_for(day)) else {
            return Vec::new();
        };
        let mut events: Vec<Event> = BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Event>(&line).ok())
            .collect();
        if events.len() > limit {
            events.drain(..events.len() - limit);
        }
        events
    }

    /// Case-insensitive substring search, newest day first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<(String, Event)> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for day in self.days() {
            for event in self.read_day(&day, usize::MAX) {
                if event.text.to_lowercase().contains(&needle) {
                    hits.push((day.clone(), event));
                    if hits.len() >= limit {
                        return hits;
                    }
                }
            }
        }
        hits
    }

    /// Drop day files older than the retention window.
    pub fn prune(&self, keep_days: i64) {
        if keep_days <= 0 {
            return;
        }
        let cutoff = chrono::Local::now().date_naive() - chrono::Duration::days(keep_days);
        for day in self.days() {
            let Ok(parsed) = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d") else {
                continue;
            };
            if parsed < cutoff {
                let _ = std::fs::remove_file(self.path_for(&day));
            }
        }
    }
}

// ── the recorder ────────────────────────────────────────────────────────

struct Inner {
    seq: Mutex<u64>,
    replay: Mutex<VecDeque<Event>>,
    /// The approval the agent is currently blocked on, if any. Cleared by
    /// answering it — see `routes::agent_approve`.
    pending: Mutex<Option<Event>>,
    store: Option<DayStore>,
    events: broadcast::Sender<Event>,
}

#[derive(Clone)]
pub struct Recorder {
    inner: Arc<Inner>,
}

impl Recorder {
    pub fn new(store: Option<DayStore>) -> Self {
        if let Some(store) = &store {
            let store = store.clone();
            let keep = crate::config::int_env("WIRED_TRANSCRIPT_DAYS", DEFAULT_RETENTION_DAYS);
            std::thread::spawn(move || store.prune(keep));
        }
        let (events, _) = broadcast::channel(1024);
        Self {
            inner: Arc::new(Inner {
                seq: Mutex::new(0),
                replay: Mutex::new(VecDeque::with_capacity(REPLAY)),
                pending: Mutex::new(None),
                store,
                events,
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn store(&self) -> Option<&DayStore> {
        self.inner.store.as_ref()
    }

    /// Record one row and hand it to every subscriber.
    pub fn push(&self, kind: EventKind, text: impl Into<String>, generation: u64) -> Event {
        let text = text.into();
        let event = {
            let mut seq = self.inner.seq.lock().unwrap();
            *seq += 1;
            Event {
                seq: *seq,
                ts: now_secs(),
                kind,
                text,
                generation,
            }
        };

        if kind == EventKind::Prompt {
            *self.inner.pending.lock().unwrap() = Some(event.clone());
        }

        {
            let mut replay = self.inner.replay.lock().unwrap();
            if replay.len() == REPLAY {
                replay.pop_front();
            }
            replay.push_back(event.clone());
        }

        if let Some(store) = &self.inner.store {
            if kind.worth_storing() {
                store.append(&event);
            }
        }

        // Err just means nobody is listening yet.
        let _ = self.inner.events.send(event.clone());
        event
    }

    /// Wired's own voice — scheduled runs, pairing requests, setup progress.
    pub fn system(&self, text: impl Into<String>) -> Event {
        self.push(EventKind::System, text, 0)
    }

    pub fn since(&self, seq: u64) -> Vec<Event> {
        self.inner
            .replay
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.seq > seq)
            .cloned()
            .collect()
    }

    pub fn pending_prompt(&self) -> Option<Event> {
        self.inner.pending.lock().unwrap().clone()
    }

    pub fn clear_pending(&self) {
        *self.inner.pending.lock().unwrap() = None;
    }

    /// Follow the PTY forever, turning screens into transcript rows.
    ///
    /// One of these per process. It is spawned by `build()` so that every
    /// consumer — HTTP, gateway, scheduler — sees the same conversation.
    pub fn watch(self, manager: PtyManager) {
        // `build()` is also reachable from a plain synchronous embedder; there
        // is nothing to tail without a runtime, and panicking would be a poor
        // way to say so.
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!("no async runtime: the transcript will not be recorded");
            return;
        }
        tokio::spawn(async move {
            let mut tail = TranscriptTail::default();
            let mut cursor = 0u64;
            let mut generation: Option<u64> = None;
            let mut announced_down = false;

            loop {
                let running = manager.running();
                let manager_for_read = manager.clone();
                let at = cursor;

                let snapshot = if running {
                    tokio::task::spawn_blocking(move || {
                        manager_for_read.wait_output(at, SAMPLE_CAP, SAMPLE_IDLE, true)
                    })
                    .await
                } else {
                    // Nothing to tail; check back rather than spin.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    tokio::task::spawn_blocking(move || {
                        manager_for_read.get_output_full(true, false)
                    })
                    .await
                };

                let Ok(snapshot) = snapshot else { break };

                let fresh = snapshot.seq > cursor;
                cursor = snapshot.seq;

                if generation != Some(snapshot.generation) {
                    if generation.is_some() {
                        tail.reset();
                        self.push(
                            EventKind::Session,
                            format!("new session (generation {})", snapshot.generation),
                            snapshot.generation,
                        );
                    }
                    generation = Some(snapshot.generation);
                }

                // `fresh` holds the bottom row back one frame: it may still be
                // mid-render, and half a sentence is worse than a late one.
                for row in tail.update(&snapshot.text, fresh, false) {
                    self.push(EventKind::from_row(row.kind), row.text, snapshot.generation);
                }

                if snapshot.running {
                    announced_down = false;
                } else if !announced_down {
                    announced_down = true;
                    self.push(
                        EventKind::Status,
                        "no session — start your assistant",
                        snapshot.generation,
                    );
                }
            }
        });
    }
}
