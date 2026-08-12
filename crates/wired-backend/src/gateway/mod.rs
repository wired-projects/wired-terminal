//! The chat bridge — reaching your assistant from a phone you already own.
//!
//! Wired's answer to "how do I talk to it from the sofa" used to be an SSH
//! tunnel. This is the replacement: a process that dials *out* to a chat
//! service and holds the connection open. Nothing is exposed, nothing is
//! forwarded, and the phone side is an app the user already has.
//!
//! Only Telegram is implemented. The `Platform` trait exists so that Discord or
//! Signal is a file rather than a rewrite — the loop below, the pairing rules
//! and the transcript relay are all platform-agnostic already.

pub mod pairing;
pub mod telegram;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use crate::agent_io;
use crate::assistant::Assistant;
use crate::pty::PtyManager;
use crate::recorder::{EventKind, Recorder};
use crate::settings_store;

use pairing::{Outcome, Pairings};

/// How long a burst of transcript rows is allowed to keep growing before it is
/// sent. Long enough to arrive as a paragraph, short enough to feel live.
///
/// Has to clear the interval the recorder samples the screen on, or every
/// sample flushes and an answer arrives one line at a time — which is what a
/// phone showed until the spinner row stopped counting as content.
const BATCH_QUIET: Duration = Duration::from_millis(2500);
const BATCH_MAX_CHARS: usize = 3000;
/// A scheduled run with nothing to report says this and nothing else. Without
/// it, a daily monitor teaches the user to mute the bot.
pub const SILENT: &str = "[SILENT]";

// ── the platform contract ───────────────────────────────────────────────

#[derive(Debug)]
pub enum InboundKind {
    Message(String),
    /// An inline button on an approval request or a selection menu.
    Answer {
        choice: agent_io::Choice,
        token: String,
    },
    /// A button on the control menu. Carries the command it stands for, so
    /// there is exactly one implementation of "restart" and the button and the
    /// typed word cannot drift apart.
    Action {
        command: String,
        token: String,
    },
}

/// One button on the control menu.
pub struct Action {
    pub label: &'static str,
    pub command: &'static str,
}

/// The control menu, in the order it is drawn — two buttons to a line.
///
/// Product logic, not Telegram's: a platform renders these, it does not decide
/// what they are.
pub const ACTIONS: &[Action] = &[
    Action {
        label: "Use Claude",
        command: "/claude",
    },
    Action {
        label: "Use Grok",
        command: "/grok",
    },
    Action {
        label: "Use Codex",
        command: "/codex",
    },
    Action {
        label: "Use Gemini",
        command: "/gemini",
    },
    Action {
        label: "Restart",
        command: "/restart",
    },
    Action {
        label: "Stop",
        command: "/stop",
    },
    Action {
        label: "Status",
        command: "/status",
    },
    Action {
        label: "Back out",
        command: "/cancel",
    },
    Action {
        label: "Mute",
        command: "/mute",
    },
    Action {
        label: "Unmute",
        command: "/unmute",
    },
];

/// What the phone shows when you type `/`. Registered with the service on
/// connect so the commands are discoverable without reading any docs.
pub const COMMANDS: &[(&str, &str)] = &[
    ("menu", "Buttons for everything below"),
    ("status", "Is the assistant running?"),
    ("cancel", "Back out of a menu it is waiting on"),
    ("claude", "Switch the assistant to Claude Code"),
    ("grok", "Switch the assistant to Grok"),
    ("codex", "Switch the assistant to Codex"),
    ("gemini", "Switch the assistant to Gemini"),
    ("restart", "Restart the assistant session"),
    ("stop", "Stop the assistant"),
    ("mute", "Stop sending replies here"),
    ("unmute", "Start sending replies again"),
];

/// A command's answer: text, or text with the control menu under it.
enum Reply {
    Text(String),
    Menu(String),
}

impl From<String> for Reply {
    fn from(text: String) -> Self {
        Reply::Text(text)
    }
}

pub struct Inbound {
    pub chat: i64,
    pub display: String,
    pub kind: InboundKind,
}

/// Everything a chat service has to do to carry this product.
///
/// `BoxFuture` rather than `async fn` so the trait stays object-safe: the run
/// loop holds `Arc<dyn Platform>` and does not care which service answered.
pub trait Platform: Send + Sync + 'static {
    fn id(&self) -> &'static str;
    /// Prove the credentials work and report the name the user will see —
    /// "@my_wired_bot". Shown on the setup screen so the pairing step is
    /// something they can follow.
    fn identify(&self) -> BoxFuture<'_, Result<String, String>>;
    fn send_text<'a>(&'a self, chat: i64, text: &'a str) -> BoxFuture<'a, Result<(), String>>;
    /// Ask for a decision. `options` are the rows of a selection menu when the
    /// agent is sitting on one; empty means the plain allow/deny approval, which
    /// is the only shape a boxed dialog has.
    fn send_prompt<'a>(
        &'a self,
        chat: i64,
        text: &'a str,
        options: &'a [agent_io::MenuOption],
    ) -> BoxFuture<'a, Result<(), String>>;
    /// Send the control menu — one button per `ACTIONS` entry.
    fn send_actions<'a>(&'a self, chat: i64, text: &'a str) -> BoxFuture<'a, Result<(), String>>;
    /// Publish the command list, so typing `/` on the phone offers them.
    /// A no-op on services that have no such concept.
    fn advertise(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    /// Tell the service a button press was handled, so the phone stops spinning.
    fn acknowledge<'a>(
        &'a self,
        token: &'a str,
        note: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;
    fn poll(&self) -> BoxFuture<'_, Result<Vec<Inbound>, String>>;
    /// Resume point to persist, so a restart does not replay old messages.
    /// Zero for services that do not have one.
    fn cursor(&self) -> i64 {
        0
    }
}

/// The pieces of the backend the bridge drives. Passed in rather than reaching
/// for `AppState`, which would make the two types mutually recursive.
#[derive(Clone)]
pub struct Hub {
    pub manager: PtyManager,
    pub assistant: Assistant,
    pub recorder: Recorder,
    pub gateway: Gateway,
}

// ── the handle ──────────────────────────────────────────────────────────

struct Inner {
    pairings: Mutex<Pairings>,
    task: Mutex<Option<JoinHandle<()>>>,
    platform: Mutex<Option<Arc<dyn Platform>>>,
    connected: AtomicBool,
    muted: AtomicBool,
    /// Non-zero while a scheduled run is in flight — see `RelayHold`.
    holds: AtomicUsize,
    last_error: Mutex<Option<String>>,
    bot_name: Mutex<Option<String>>,
}

/// Suppresses live relaying while it is alive.
///
/// A scheduled run streams its whole working-out through the transcript; the
/// phone wants the conclusion, once. Dropping the guard restores the relay,
/// including on an early return or a panic.
pub struct RelayHold {
    inner: Arc<Inner>,
}

impl Drop for RelayHold {
    fn drop(&mut self) {
        self.inner.holds.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Control surface for the bridge: start it, stop it, approve a pairing, ask
/// how it is doing. Cheap to clone and safe to hold in `AppState`.
#[derive(Clone)]
pub struct Gateway {
    inner: Arc<Inner>,
}

impl Default for Gateway {
    fn default() -> Self {
        Self::new()
    }
}

impl Gateway {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                pairings: Mutex::new(Pairings::default()),
                task: Mutex::new(None),
                platform: Mutex::new(None),
                connected: AtomicBool::new(false),
                muted: AtomicBool::new(false),
                holds: AtomicUsize::new(0),
                last_error: Mutex::new(None),
                bot_name: Mutex::new(None),
            }),
        }
    }

    pub fn status(&self) -> Value {
        let stored = settings_store::get();
        let mut pairings = self.inner.pairings.lock().unwrap();
        json!({
            "platform": "telegram",
            "enabled": stored.telegram.enabled,
            "configured": !settings_store::telegram_bot_token().is_empty(),
            "connected": self.inner.connected.load(Ordering::Relaxed),
            "muted": self.inner.muted.load(Ordering::Relaxed),
            "bot": *self.inner.bot_name.lock().unwrap(),
            "paired_chats": stored.telegram.allowed_chats.len(),
            "pending": pairings.list(),
            "locked_for": pairings.is_locked(),
            "last_error": *self.inner.last_error.lock().unwrap(),
        })
    }

    fn fail(&self, message: impl Into<String>) {
        *self.inner.last_error.lock().unwrap() = Some(message.into());
        self.inner.connected.store(false, Ordering::Relaxed);
    }

    /// Stop any running bridge and start one from the current settings.
    ///
    /// Called at boot and after every settings change, so turning the bridge on
    /// or pasting a new token takes effect without restarting the process.
    pub fn restart(&self, hub: Hub) {
        self.stop();

        let stored = settings_store::get();
        let token = settings_store::telegram_bot_token();
        if !stored.telegram.enabled || token.is_empty() {
            return;
        }

        let platform = match telegram::Telegram::new(token, stored.telegram.update_offset) {
            Ok(platform) => Arc::new(platform) as Arc<dyn Platform>,
            Err(e) => {
                self.fail(e);
                return;
            }
        };
        *self.inner.platform.lock().unwrap() = Some(Arc::clone(&platform));
        *self.inner.last_error.lock().unwrap() = None;

        let gateway = self.clone();
        let handle = tokio::spawn(async move { run(hub, platform, gateway).await });
        *self.inner.task.lock().unwrap() = Some(handle);
    }

    pub fn stop(&self) {
        if let Some(task) = self.inner.task.lock().unwrap().take() {
            task.abort();
        }
        *self.inner.platform.lock().unwrap() = None;
        self.inner.connected.store(false, Ordering::Relaxed);
    }

    /// Forget the bot: its token, the phones paired to it, and the cursor into
    /// its update queue.
    ///
    /// Pasting a new token over an old one is not enough, because none of the
    /// rest belongs to the new bot. The approved chats were approved for a bot
    /// that is going away, and the offset is a position in a queue that no
    /// longer exists. Clearing the lot is what makes "start again with a
    /// different bot" a button rather than an instruction to delete
    /// `settings.json`.
    ///
    /// The pairing lockout deliberately survives: it is a brake on guessing
    /// codes, and a reset must not be the way around it.
    pub fn reset(&self) -> Result<(), String> {
        self.stop();
        settings_store::set_telegram_bot_token("")?;
        settings_store::update(|s| {
            s.telegram.enabled = false;
            s.telegram.allowed_chats.clear();
            s.telegram.owner_chat = None;
            s.telegram.update_offset = 0;
        })?;
        self.inner.pairings.lock().unwrap().clear();
        *self.inner.bot_name.lock().unwrap() = None;
        *self.inner.last_error.lock().unwrap() = None;
        self.inner.muted.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub fn set_muted(&self, muted: bool) {
        self.inner.muted.store(muted, Ordering::Relaxed);
    }

    pub fn hold_relay(&self) -> RelayHold {
        self.inner.holds.fetch_add(1, Ordering::Relaxed);
        RelayHold {
            inner: Arc::clone(&self.inner),
        }
    }

    fn relaying(&self) -> bool {
        !self.inner.muted.load(Ordering::Relaxed) && self.inner.holds.load(Ordering::Relaxed) == 0
    }

    pub fn pending_pairings(&self) -> Vec<Value> {
        self.inner.pairings.lock().unwrap().list()
    }

    /// Approve a pairing code and remember the chat it belonged to.
    pub fn approve_pairing(&self, code: &str) -> Result<Value, String> {
        let request = self.inner.pairings.lock().unwrap().approve(code)?;
        let chat = request.chat;
        settings_store::update(|s| {
            if !s.telegram.allowed_chats.contains(&chat) {
                s.telegram.allowed_chats.push(chat);
            }
            // The first approved chat becomes where unsolicited output goes:
            // scheduled results, approval requests, "I restarted".
            s.telegram.owner_chat.get_or_insert(chat);
        })?;

        let summary = request.redacted();
        if let Some(platform) = self.inner.platform.lock().unwrap().clone() {
            tokio::spawn(async move {
                let _ = platform
                    .send_text(
                        chat,
                        "You're paired. Send me anything and I'll pass it to your assistant.",
                    )
                    .await;
            });
        }
        Ok(summary)
    }

    pub fn deny_pairing(&self, code: &str) -> Result<Value, String> {
        let request = self.inner.pairings.lock().unwrap().deny(code)?;
        Ok(request.redacted())
    }

    /// Remove a paired chat. The bridge re-reads the list every poll, so this
    /// takes effect immediately.
    pub fn unpair(&self, chat: i64) -> Result<(), String> {
        settings_store::update(|s| {
            s.telegram.allowed_chats.retain(|c| *c != chat);
            if s.telegram.owner_chat == Some(chat) {
                s.telegram.owner_chat = s.telegram.allowed_chats.first().copied();
            }
        })
        .map(|_| ())
    }

    /// Push something to the owner's chat — used by the scheduler.
    pub async fn notify_owner(&self, text: &str) -> Result<(), String> {
        if text.trim().is_empty() || text.trim() == SILENT {
            return Ok(());
        }
        let platform = self
            .inner
            .platform
            .lock()
            .unwrap()
            .clone()
            .ok_or("The chat bridge is not running")?;
        let chat = settings_store::get()
            .telegram
            .owner_chat
            .ok_or("No chat has been paired yet")?;
        platform.send_text(chat, text).await
    }

    pub fn is_delivering(&self) -> bool {
        self.inner.connected.load(Ordering::Relaxed)
            && settings_store::get().telegram.owner_chat.is_some()
    }
}

// ── the loop ────────────────────────────────────────────────────────────

async fn run(hub: Hub, platform: Arc<dyn Platform>, gateway: Gateway) {
    match platform.identify().await {
        Ok(name) => {
            tracing::info!(bot = name, "chat bridge connected");
            *gateway.inner.bot_name.lock().unwrap() = Some(name);
            // Cosmetic, and not worth failing the connection over.
            if let Err(e) = platform.advertise().await {
                tracing::debug!(error = e, "could not publish the command list");
            }
        }
        // Keep going: a failed handshake is usually a network blip, and the
        // poll loop below has the backoff and the error reporting already.
        Err(e) => gateway.fail(e),
    }

    let relay = tokio::spawn(relay_transcript(
        hub.clone(),
        Arc::clone(&platform),
        gateway.clone(),
    ));
    receive(hub, platform, gateway.clone()).await;
    relay.abort();
    gateway.inner.connected.store(false, Ordering::Relaxed);
}

/// Inbound: chat → agent.
async fn receive(hub: Hub, platform: Arc<dyn Platform>, gateway: Gateway) {
    let mut backoff = Duration::from_secs(1);

    loop {
        match platform.poll().await {
            Ok(events) => {
                gateway.inner.connected.store(true, Ordering::Relaxed);
                *gateway.inner.last_error.lock().unwrap() = None;
                backoff = Duration::from_secs(1);

                if !events.is_empty() {
                    let cursor = platform.cursor();
                    if cursor != 0 && settings_store::get().telegram.update_offset != cursor {
                        let _ = settings_store::update(|s| s.telegram.update_offset = cursor);
                    }
                }
                for event in events {
                    handle_inbound(&hub, &platform, &gateway, event).await;
                }
            }
            Err(e) => {
                gateway.fail(e.clone());
                tracing::warn!("chat bridge poll failed: {e}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

async fn handle_inbound(
    hub: &Hub,
    platform: &Arc<dyn Platform>,
    gateway: &Gateway,
    event: Inbound,
) {
    let paired = settings_store::get()
        .telegram
        .allowed_chats
        .contains(&event.chat);

    if !paired {
        offer_pairing(hub, platform, gateway, &event).await;
        return;
    }

    match event.kind {
        InboundKind::Answer { choice, token } => {
            let note = match agent_io::decide(&hub.manager, choice).await {
                Ok(()) => {
                    hub.recorder.clear_pending();
                    match choice {
                        agent_io::Choice::Allow => "Allowed".to_string(),
                        agent_io::Choice::Deny => "Declined".to_string(),
                        agent_io::Choice::Pick(digit) => {
                            format!("Picked {}", digit as char)
                        }
                    }
                }
                Err(_) => "Your assistant isn't waiting on anything right now".to_string(),
            };
            let _ = platform.acknowledge(&token, &note).await;
        }
        InboundKind::Action {
            command: word,
            token,
        } => {
            // Buttons run the very same handler the typed word does.
            let reply = command(hub, gateway, &word).await;
            // The toast on the button. Telegram caps it at 200 characters and
            // rejects the whole call over that, which would leave the button
            // spinning after the action already ran.
            let note: String = match &reply {
                Some(Reply::Text(text)) => text
                    .lines()
                    .next()
                    .unwrap_or("Done")
                    .chars()
                    .take(180)
                    .collect(),
                _ => "Done".to_string(),
            };
            let _ = platform.acknowledge(&token, &note).await;
            if let Some(reply) = reply {
                deliver(platform, event.chat, reply).await;
            }
        }
        InboundKind::Message(text) => {
            if let Some(reply) = command(hub, gateway, &text).await {
                deliver(platform, event.chat, reply).await;
                return;
            }
            if let Err(e) = agent_io::say(&hub.manager, &hub.assistant, &text, true).await {
                let _ = platform
                    .send_text(event.chat, &format!("I couldn't pass that on: {e}"))
                    .await;
            }
        }
    }
}

async fn deliver(platform: &Arc<dyn Platform>, chat: i64, reply: Reply) {
    let _ = match reply {
        Reply::Text(text) => platform.send_text(chat, &text).await,
        Reply::Menu(text) => platform.send_actions(chat, &text).await,
    };
}

/// An unknown sender. Issue a code rather than a rejection, so the owner has
/// something to approve and the sender has something to read back.
async fn offer_pairing(
    hub: &Hub,
    platform: &Arc<dyn Platform>,
    gateway: &Gateway,
    event: &Inbound,
) {
    let outcome = gateway.inner.pairings.lock().unwrap().request(
        platform.id(),
        event.chat,
        event.display.clone(),
    );

    let reply = match outcome {
        Outcome::Issued(code) => {
            hub.recorder.system(format!(
                "{} wants to talk to your assistant. Approve the code {} in Settings.",
                event.display, code
            ));
            format!(
                "Before we start, the owner of this computer has to let you in.\n\n\
                 Your code is {code}\n\n\
                 Ask them to open Wired, go to Settings → Telegram, and approve it. \
                 The code works for one hour."
            )
        }
        Outcome::Refused(reason) => reason.to_string(),
    };
    let _ = platform.send_text(event.chat, &reply).await;
}

/// The CLI a `/claude`-style command names, if that is what this word is.
///
/// The slash is required rather than optional: every message a user sends
/// reaches the command handler, and "gemini" on its own is a perfectly ordinary
/// thing to say to an assistant — stripping an absent slash would swallow it.
fn assistant_switch(word: &str) -> Option<&str> {
    word.strip_prefix('/')
        .filter(|name| crate::providers::ASSISTANT_PROVIDERS.contains(name))
}

/// Chat commands. Returns `Some(reply)` when the message was a command and must
/// not reach the agent.
async fn command(hub: &Hub, gateway: &Gateway, text: &str) -> Option<Reply> {
    let word = text.split_whitespace().next()?.to_ascii_lowercase();
    // Telegram appends @botname when several bots share a group.
    let word = word.split('@').next().unwrap_or(&word);

    // Switching ends the running session — the CLI is a different binary and
    // cannot be swapped underneath itself. On a phone that is the expected
    // outcome, so do it rather than leaving a "restart needed" note nobody can
    // act on. (MCP deliberately stops at the preference; there the caller may
    // *be* the session.)
    //
    // Handled ahead of the table so there is one arm for every agent CLI, and a
    // new one becomes switchable from the phone the moment it joins the list.
    if let Some(wanted) = assistant_switch(word) {
        return Some(Reply::Text(switch_assistant(hub, wanted).await));
    }

    Some(match word {
        "/start" | "/help" | "/menu" => Reply::Menu(
            "Send me anything and I'll pass it to your assistant, and send its answers \
             back here.\n\nOr use these:"
                .to_string(),
        ),
        "/restart" => Reply::Text(restart_assistant(hub).await),
        // Escape, always available. Any modal can be dismissed from the phone,
        // which is what stops a picker wedging the session for whoever is not
        // sitting at a terminal.
        "/cancel" | "/esc" | "/escape" => {
            match agent_io::decide(&hub.manager, agent_io::Choice::Deny).await {
                Ok(()) => {
                    hub.recorder.clear_pending();
                    "Backed out.".to_string().into()
                }
                Err(e) => format!("Nothing to back out of: {e}").into(),
            }
        }
        // These end the session or the login, and neither can be undone from a
        // phone — the CLI would be sitting at a sign-in screen with nobody
        // able to type into it.
        "/logout" | "/exit" | "/quit" => format!(
            "`{word}` is a command the agent CLI itself handles, and running it from here would \
             leave the session somewhere you can't reach from Telegram. Do it over SSH if you \
             mean it."
        )
        .into(),
        "/status" => {
            let stored = settings_store::get();
            let running = hub.manager.running();
            let provider = hub
                .manager
                .provider()
                .unwrap_or_else(|| stored.assistant.unwrap_or_else(|| "claude".into()));
            let folder = crate::providers::agent_cwd().display().to_string();
            // Naming the pin here too, so the answer to "why is it claude
            // again?" is on the same screen as the question.
            let provider = match pin_that_overrides(&provider, assistant_pin().as_deref()) {
                Some(pinned) => format!("{provider} (pinned to {pinned} on restart)"),
                None => provider,
            };
            format!(
                "{}\nAssistant: {provider}\nFolder: {folder}\nReplies here: {}",
                if running {
                    "Running."
                } else {
                    "Not running — send me anything and I'll start it."
                },
                if gateway.inner.muted.load(Ordering::Relaxed) {
                    "muted"
                } else {
                    "on"
                }
            )
            .into()
        }
        "/stop" => {
            let assistant = hub.assistant.clone();
            let _ = tokio::task::spawn_blocking(move || assistant.disable(true)).await;
            "Stopped. Send me anything to start it again."
                .to_string()
                .into()
        }
        "/mute" => {
            gateway.set_muted(true);
            "Muted. I'll still pass on what you send me."
                .to_string()
                .into()
        }
        "/unmute" => {
            gateway.set_muted(false);
            "Unmuted.".to_string().into()
        }
        _ => return None,
    })
}

/// The provider a `WIRED_ASSISTANT_PROVIDER` pin will impose at the next start,
/// when that is not the one being asked for.
///
/// `default_assistant_provider` reads the environment before `settings.json`, on
/// purpose — an operator's pin is meant to win. The failure mode is that saving
/// a different choice still *looks* like it worked, and the revert only shows up
/// after a restart, days later and somewhere else. Taking the value as an
/// argument keeps this testable without writing to the process environment.
fn pin_that_overrides(wanted: &str, pinned: Option<&str>) -> Option<String> {
    let pinned = pinned?.trim().to_ascii_lowercase();
    (!pinned.is_empty() && pinned != wanted.trim().to_ascii_lowercase()).then_some(pinned)
}

fn assistant_pin() -> Option<String> {
    std::env::var("WIRED_ASSISTANT_PROVIDER").ok()
}

/// Save which CLI to run and restart the session onto it.
///
/// Refuses a CLI that is not installed: saving one would leave the phone with
/// an assistant that cannot start, and the failure would surface later and
/// somewhere else.
async fn switch_assistant(hub: &Hub, wanted: &str) -> String {
    let running = hub.manager.provider();
    if running.as_deref() == Some(wanted) {
        return format!("Already running {wanted}.");
    }
    if crate::providers::resolve_cmd(wanted).is_none() {
        return format!(
            "{wanted} isn't installed on this machine, so I've left the assistant as it is."
        );
    }
    if let Err(e) = settings_store::update(|s| s.assistant = Some(wanted.to_string())) {
        return format!("Couldn't save that: {e}");
    }
    if let Err(e) = hub
        .assistant
        .configure(Some(wanted), None, None, None, None)
    {
        return format!("Couldn't switch: {e}");
    }
    let outcome = match restart_session(hub).await {
        Ok(()) => format!("Switched to {wanted} and restarted. The session starts fresh."),
        Err(e) => format!("Saved {wanted}, but the restart failed: {e}"),
    };

    match pin_that_overrides(wanted, assistant_pin().as_deref()) {
        None => outcome,
        Some(pinned) => format!(
            "{outcome}\n\nOne catch: this server sets WIRED_ASSISTANT_PROVIDER={pinned}, and that \
             beats the saved choice — the next restart will put {pinned} back. Remove that line \
             from the service's environment to make {wanted} stick."
        ),
    }
}

async fn restart_assistant(hub: &Hub) -> String {
    match restart_session(hub).await {
        Ok(()) => "Restarted. The session starts fresh.".to_string(),
        Err(e) => format!("Couldn't restart: {e}"),
    }
}

/// Kill the CLI and bring it back. Blocking work stays off the runtime — the
/// same reason `agent_io::ensure_session` does.
async fn restart_session(hub: &Hub) -> Result<(), String> {
    let assistant = hub.assistant.clone();
    tokio::task::spawn_blocking(move || {
        assistant.disable(true);
        assistant.enable().map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Outbound: transcript → chat.
///
/// The same filtered rows the desktop transcript shows, batched into paragraphs
/// so a working agent does not arrive as forty notifications.
async fn relay_transcript(hub: Hub, platform: Arc<dyn Platform>, gateway: Gateway) {
    let mut events = hub.recorder.subscribe();
    let mut buffer = String::new();

    loop {
        let received = if buffer.is_empty() {
            events.recv().await.map(Some)
        } else {
            match tokio::time::timeout(BATCH_QUIET, events.recv()).await {
                Ok(result) => result.map(Some),
                // Quiet long enough — the agent has finished a thought.
                Err(_) => Ok(None),
            }
        };

        let event = match received {
            Ok(Some(event)) => Some(event),
            Ok(None) => None,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        };

        let Some(event) = event else {
            flush(&buffer, &platform, &gateway).await;
            buffer.clear();
            continue;
        };

        match event.kind {
            // The sender does not need their own message read back to them,
            // and session bookkeeping is not conversation.
            EventKind::User | EventKind::Status | EventKind::Session => continue,
            EventKind::Prompt => {
                flush(&buffer, &platform, &gateway).await;
                buffer.clear();
                // Never held back: an unanswered approval leaves the agent
                // blocked, and the guard exists to reduce noise, not to hide
                // the one message that needs an answer.
                if !gateway.inner.muted.load(Ordering::Relaxed) {
                    // Read the rows off the live screen rather than the event:
                    // the transcript arrives a line at a time, and a menu is
                    // only answerable as a whole.
                    let options = agent_io::menu_options(&hub.manager.screen_text());
                    for chat in chats() {
                        let _ = platform
                            .send_prompt(
                                chat,
                                &format!("Your assistant is asking:\n\n{}", event.text),
                                &options,
                            )
                            .await;
                    }
                }
            }
            EventKind::Text | EventKind::Notice | EventKind::System => {
                if event.text.trim() == SILENT {
                    continue;
                }
                if !buffer.is_empty() {
                    buffer.push('\n');
                }
                buffer.push_str(&event.text);
                if buffer.chars().count() >= BATCH_MAX_CHARS {
                    flush(&buffer, &platform, &gateway).await;
                    buffer.clear();
                }
            }
        }
    }
}

fn chats() -> Vec<i64> {
    settings_store::get().telegram.allowed_chats
}

async fn flush(buffer: &str, platform: &Arc<dyn Platform>, gateway: &Gateway) {
    let text = buffer.trim();
    if text.is_empty() || !gateway.relaying() {
        return;
    }
    for chat in chats() {
        if let Err(e) = platform.send_text(chat, text).await {
            tracing::warn!("chat delivery failed: {e}");
            gateway.fail(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{assistant_switch, pin_that_overrides, ACTIONS, COMMANDS};
    use crate::providers::ASSISTANT_PROVIDERS;

    #[test]
    fn a_slashed_cli_name_switches_the_assistant() {
        for provider in ASSISTANT_PROVIDERS {
            assert_eq!(assistant_switch(&format!("/{provider}")), Some(*provider));
        }
    }

    /// The command handler sees every message, not just the slashed ones, so a
    /// bare CLI name has to fall through to the agent — "gemini is down again"
    /// is a sentence, not a request to switch.
    #[test]
    fn a_bare_cli_name_is_a_message_not_a_command() {
        for provider in ASSISTANT_PROVIDERS {
            assert_eq!(assistant_switch(provider), None);
        }
        assert_eq!(assistant_switch("/restart"), None);
        assert_eq!(assistant_switch("/gpt"), None);
    }

    /// The phone is the only place some users ever switch assistants from, and
    /// a CLI missing from the menu is a CLI they cannot reach. Both lists are
    /// hand-written product copy, so check them against the real one.
    #[test]
    fn every_assistant_can_be_chosen_from_chat() {
        for provider in ASSISTANT_PROVIDERS {
            let command = format!("/{provider}");
            assert!(
                ACTIONS.iter().any(|a| a.command == command),
                "no menu button for {provider}"
            );
            assert!(
                COMMANDS.iter().any(|(name, _)| name == provider),
                "{provider} is not advertised to the chat service"
            );
        }
    }

    #[test]
    fn no_pin_means_nothing_to_warn_about() {
        assert_eq!(pin_that_overrides("grok", None), None);
        assert_eq!(pin_that_overrides("grok", Some("")), None);
        assert_eq!(pin_that_overrides("grok", Some("   ")), None);
    }

    #[test]
    fn a_pin_matching_the_choice_is_not_a_conflict() {
        assert_eq!(pin_that_overrides("grok", Some("grok")), None);
        // The environment is read verbatim; a stray case or space is still a
        // pin on the same provider, not a different one.
        assert_eq!(pin_that_overrides("grok", Some("GROK")), None);
        assert_eq!(pin_that_overrides("grok", Some(" grok ")), None);
    }

    #[test]
    fn a_pin_on_a_different_provider_is_reported() {
        assert_eq!(
            pin_that_overrides("grok", Some("claude")),
            Some("claude".to_string())
        );
        assert_eq!(
            pin_that_overrides("claude", Some("Grok")),
            Some("grok".to_string())
        );
    }
}
