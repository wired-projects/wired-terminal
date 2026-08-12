//! Telegram, over an outbound long poll.
//!
//! The whole point of this transport is what it does *not* need: no open port,
//! no TLS certificate, no dynamic DNS, no token pasted into a URL bar, and no
//! tunnel to a laptop behind a home router. The process dials out to
//! api.telegram.org and holds the connection; everything else follows from that.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};

use super::{Inbound, InboundKind, Platform, ACTIONS, COMMANDS};
use crate::agent_io::{Choice, MenuOption};

/// Telegram hard-limits a message to 4096 characters; leave room for the
/// framing we add.
const MAX_CHARS: usize = 3800;
/// How long the server holds the poll open before answering with nothing.
const POLL_SECONDS: u64 = 25;

pub const ALLOW: &str = "wired:allow";
pub const DENY: &str = "wired:deny";
/// `wired:pick:3` — the digit of one row of a selection menu.
pub const PICK: &str = "wired:pick:";
/// `wired:cmd:/restart` — a control-menu button, carrying the command it runs.
pub const CMD: &str = "wired:cmd:";

/// A button caption has to survive being read on a phone, so keep the head of
/// the row and drop the description the CLI aligns after it.
const LABEL_CHARS: usize = 28;

fn caption(option: &MenuOption) -> String {
    // "Default (recommended) ✔  Sonnet 5 · Efficient…" → "Default (recommended) ✔"
    let head = option
        .label
        .split("  ")
        .next()
        .unwrap_or(&option.label)
        .trim();
    let head = if head.is_empty() { &option.label } else { head };
    let mut out: String = head.chars().take(LABEL_CHARS).collect();
    if head.chars().count() > LABEL_CHARS {
        out.push('…');
    }
    format!("{}. {out}", option.digit as char)
}

/// The control menu — `ACTIONS`, two buttons to a line.
fn action_keyboard() -> Value {
    ACTIONS
        .chunks(2)
        .map(|pair| {
            pair.iter()
                .map(|action| {
                    json!({
                        "text": action.label,
                        "callback_data": format!("{CMD}{}", action.command),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .into()
}

/// Allow/Don't for a plain approval; one button per row for a selection menu,
/// two to a line, with an escape hatch underneath. A menu the phone cannot
/// answer is a session the phone has wedged.
fn keyboard(options: &[MenuOption]) -> Value {
    if options.is_empty() {
        return json!([[
            {"text": "Allow", "callback_data": ALLOW},
            {"text": "Don't", "callback_data": DENY},
        ]]);
    }
    let mut rows: Vec<Value> = options
        .chunks(2)
        .map(|pair| {
            pair.iter()
                .map(|option| {
                    json!({
                        "text": caption(option),
                        "callback_data": format!("{PICK}{}", option.digit as char),
                    })
                })
                .collect::<Vec<_>>()
                .into()
        })
        .collect();
    rows.push(json!([{"text": "Cancel", "callback_data": DENY}]));
    rows.into()
}

pub struct Telegram {
    token: String,
    client: reqwest::Client,
    offset: AtomicI64,
}

impl Telegram {
    pub fn new(token: impl Into<String>, offset: i64) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            // Longer than the long poll itself, or every quiet minute looks
            // like a network failure and the loop restarts for nothing.
            .timeout(Duration::from_secs(POLL_SECONDS + 20))
            .build()
            .map_err(|e| format!("Could not create an HTTPS client: {e}"))?;
        Ok(Self {
            token: token.into(),
            client,
            offset: AtomicI64::new(offset),
        })
    }

    fn url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{method}", self.token)
    }

    async fn call(&self, method: &str, body: Value) -> Result<Value, String> {
        let response = self
            .client
            .post(self.url(method))
            .json(&body)
            .send()
            .await
            // `without_url` is not cosmetic: the URL carries the bot token, and
            // this string goes to the log, to `last_error`, and out of the API.
            .map_err(|e| format!("Telegram {method} failed: {}", e.without_url()))?;

        let status = response.status();
        let payload: Value = response.json().await.map_err(|e| {
            format!(
                "Telegram {method} returned unreadable JSON: {}",
                e.without_url()
            )
        })?;

        if payload["ok"] == json!(true) {
            return Ok(payload["result"].clone());
        }
        // Telegram's own description is the useful part — "Unauthorized" for a
        // bad token, "chat not found" for a bad chat id.
        Err(payload["description"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("Telegram {method} failed with HTTP {status}")))
    }

    async fn send(&self, chat: i64, text: &str, markup: Option<Value>) -> Result<(), String> {
        for chunk in split_message(text) {
            let mut body = json!({
                "chat_id": chat,
                "text": chunk,
                // No parse_mode: agent output is full of underscores, asterisks
                // and backticks, and a message Telegram refuses to parse is a
                // message the user never sees.
                "disable_web_page_preview": true,
            });
            if let Some(markup) = &markup {
                body["reply_markup"] = markup.clone();
            }
            self.call("sendMessage", body).await?;
        }
        Ok(())
    }
}

impl Platform for Telegram {
    fn id(&self) -> &'static str {
        "telegram"
    }

    /// Validate the token and return the bot's @username, which is what the
    /// setup screen tells the user to open on their phone.
    fn identify(&self) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            let me = self.call("getMe", json!({})).await?;
            Ok(me["username"]
                .as_str()
                .map(|u| format!("@{u}"))
                .unwrap_or_else(|| "the bot".to_string()))
        })
    }

    fn cursor(&self) -> i64 {
        self.offset.load(Ordering::Relaxed)
    }

    fn send_text<'a>(&'a self, chat: i64, text: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.send(chat, text, None).await })
    }

    fn send_prompt<'a>(
        &'a self,
        chat: i64,
        text: &'a str,
        options: &'a [MenuOption],
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let markup = json!({ "inline_keyboard": keyboard(options) });
            self.send(chat, text, Some(markup)).await
        })
    }

    fn send_actions<'a>(&'a self, chat: i64, text: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let markup = json!({ "inline_keyboard": action_keyboard() });
            self.send(chat, text, Some(markup)).await
        })
    }

    /// Publish the command list so typing `/` on the phone offers them with
    /// descriptions, instead of the user having to remember what exists.
    fn advertise(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let commands: Vec<Value> = COMMANDS
                .iter()
                .map(|(command, description)| json!({"command": command, "description": description}))
                .collect();
            self.call("setMyCommands", json!({ "commands": commands }))
                .await
                .map(|_| ())
        })
    }

    fn acknowledge<'a>(
        &'a self,
        token: &'a str,
        note: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            // Without this the button keeps spinning on the phone even though
            // the answer already reached the agent.
            self.call(
                "answerCallbackQuery",
                json!({"callback_query_id": token, "text": note}),
            )
            .await
            .map(|_| ())
        })
    }

    fn poll(&self) -> BoxFuture<'_, Result<Vec<Inbound>, String>> {
        Box::pin(async move {
            let updates = self
                .call(
                    "getUpdates",
                    json!({
                        "offset": self.offset.load(Ordering::Relaxed),
                        "timeout": POLL_SECONDS,
                        "allowed_updates": ["message", "callback_query"],
                    }),
                )
                .await?;

            let mut inbound = Vec::new();
            for update in updates.as_array().unwrap_or(&Vec::new()) {
                if let Some(id) = update["update_id"].as_i64() {
                    // Acknowledging past `id` is what stops Telegram resending
                    // it; do that even for updates we ignore.
                    self.offset.fetch_max(id + 1, Ordering::Relaxed);
                }
                if let Some(event) = parse_update(update) {
                    inbound.push(event);
                }
            }
            Ok(inbound)
        })
    }
}

fn describe(from: &Value) -> String {
    let name = [from["first_name"].as_str(), from["last_name"].as_str()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    match (name.trim(), from["username"].as_str()) {
        ("", Some(username)) => format!("@{username}"),
        (name, Some(username)) => format!("{name} (@{username})"),
        ("", None) => "someone".to_string(),
        (name, None) => name.to_string(),
    }
}

/// Anything unrecognised is a decline: a button from an older message whose
/// menu has moved on should not press something the sender cannot see.
fn parse_choice(data: &str) -> Choice {
    if data == ALLOW {
        return Choice::Allow;
    }
    if let Some(digit) = data.strip_prefix(PICK) {
        if let Some(byte) = digit.as_bytes().first().filter(|b| b.is_ascii_digit()) {
            return Choice::Pick(*byte);
        }
    }
    Choice::Deny
}

fn parse_update(update: &Value) -> Option<Inbound> {
    if let Some(query) = update.get("callback_query").filter(|q| !q.is_null()) {
        let chat = query["message"]["chat"]["id"].as_i64()?;
        let data = query["data"].as_str().unwrap_or_default();
        let token = query["id"].as_str().unwrap_or_default().to_string();
        let display = describe(&query["from"]);
        // Only commands this build advertises: callback data is round-tripped
        // through the sender's client, so it is input, not memory.
        if let Some(word) = data.strip_prefix(CMD) {
            if let Some(action) = ACTIONS.iter().find(|a| a.command == word) {
                return Some(Inbound {
                    chat,
                    display,
                    kind: InboundKind::Action {
                        command: action.command.to_string(),
                        token,
                    },
                });
            }
            return None;
        }
        return Some(Inbound {
            chat,
            display,
            kind: InboundKind::Answer {
                choice: parse_choice(data),
                token,
            },
        });
    }

    let message = update.get("message").filter(|m| !m.is_null())?;
    let text = message["text"].as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(Inbound {
        chat: message["chat"]["id"].as_i64()?,
        display: describe(&message["from"]),
        kind: InboundKind::Message(text.to_string()),
    })
}

/// Break a long reply on line boundaries where possible, so code and lists do
/// not get cut mid-token.
fn split_message(text: &str) -> Vec<String> {
    if text.chars().count() <= MAX_CHARS {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.split_inclusive('\n') {
        if current.chars().count() + line.chars().count() > MAX_CHARS && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        // A single line longer than the limit still has to be cut somewhere.
        if line.chars().count() > MAX_CHARS {
            let mut buffer = String::new();
            for ch in line.chars() {
                buffer.push(ch);
                if buffer.chars().count() == MAX_CHARS {
                    chunks.push(std::mem::take(&mut buffer));
                }
            }
            current = buffer;
        } else {
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::{caption, keyboard, parse_choice, ALLOW, DENY, PICK};
    use crate::agent_io::{Choice, MenuOption};

    fn option(digit: u8, label: &str) -> MenuOption {
        MenuOption {
            digit,
            label: label.to_string(),
        }
    }

    #[test]
    fn a_plain_approval_still_gets_allow_and_dont() {
        let rows = keyboard(&[]);
        assert_eq!(rows[0][0]["callback_data"], ALLOW);
        assert_eq!(rows[0][1]["callback_data"], DENY);
    }

    #[test]
    fn a_menu_gets_a_button_per_row_plus_cancel() {
        let options = [
            option(b'1', "Default (recommended) ✔  Sonnet 5 · Efficient"),
            option(b'2', "Sonnet                  Sonnet 5 · Efficient"),
            option(b'3', "Opus                    Opus 5 · ~2× usage"),
        ];
        let rows = keyboard(&options);
        // three options, two to a line, then the escape hatch
        assert_eq!(rows.as_array().unwrap().len(), 3);
        assert_eq!(rows[0][0]["callback_data"], format!("{PICK}1"));
        assert_eq!(rows[1][0]["callback_data"], format!("{PICK}3"));
        assert_eq!(rows[2][0]["callback_data"], DENY);
        assert_eq!(rows[2][0]["text"], "Cancel");
    }

    #[test]
    fn a_caption_keeps_the_name_and_drops_the_aligned_description() {
        assert_eq!(
            caption(&option(
                b'1',
                "Default (recommended) ✔  Sonnet 5 · Efficient"
            )),
            "1. Default (recommended) ✔"
        );
    }

    #[test]
    fn a_long_caption_is_cut_rather_than_sent_whole() {
        let long = caption(&option(b'4', &"x".repeat(80)));
        assert!(long.chars().count() < 40, "{long}");
        assert!(long.ends_with('…'));
    }

    #[test]
    fn a_pick_carries_its_digit() {
        assert_eq!(parse_choice(&format!("{PICK}4")), Choice::Pick(b'4'));
        assert_eq!(parse_choice(ALLOW), Choice::Allow);
    }

    /// A button from a message whose menu has moved on must not press
    /// something the sender can no longer see.
    #[test]
    fn anything_unrecognised_declines() {
        assert_eq!(parse_choice("wired:pick:banana"), Choice::Deny);
        assert_eq!(parse_choice(""), Choice::Deny);
        assert_eq!(parse_choice("something else"), Choice::Deny);
    }
}

#[cfg(test)]
mod menu_tests {
    use super::{action_keyboard, parse_update, CMD};
    use crate::gateway::{InboundKind, ACTIONS, COMMANDS};
    use serde_json::json;

    #[test]
    fn every_action_gets_a_button() {
        let rows = action_keyboard();
        let count: usize = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row.as_array().unwrap().len())
            .sum();
        assert_eq!(count, ACTIONS.len());
    }

    /// Both providers are reachable in one tap — the thing this menu exists for.
    #[test]
    fn claude_and_grok_are_both_on_it() {
        let data = action_keyboard().to_string();
        assert!(data.contains("wired:cmd:/claude"), "{data}");
        assert!(data.contains("wired:cmd:/grok"), "{data}");
        assert!(data.contains("wired:cmd:/restart"), "{data}");
    }

    fn callback(data: &str) -> Option<InboundKind> {
        parse_update(&json!({
            "callback_query": {
                "id": "1",
                "from": {"username": "someone"},
                "message": {"chat": {"id": 7}},
                "data": data,
            }
        }))
        .map(|inbound| inbound.kind)
    }

    #[test]
    fn a_button_runs_the_command_it_names() {
        match callback(&format!("{CMD}/restart")) {
            Some(InboundKind::Action { command, .. }) => assert_eq!(command, "/restart"),
            other => panic!("expected an action, got {other:?}"),
        }
    }

    /// Callback data comes back through the sender's client, so it is input.
    /// A command this build does not advertise must not be run.
    #[test]
    fn an_unadvertised_command_is_dropped() {
        assert!(callback(&format!("{CMD}/logout")).is_none());
        assert!(callback(&format!("{CMD}/rm -rf /")).is_none());
    }

    /// Every button has to reach a real handler, and every advertised command
    /// has to be one the bridge answers — otherwise the phone offers dead ends.
    #[test]
    fn the_slash_list_and_the_buttons_agree() {
        for action in ACTIONS {
            let word = action.command.trim_start_matches('/');
            assert!(
                COMMANDS.iter().any(|(name, _)| *name == word),
                "{} is a button with no entry in the / list",
                action.command
            );
        }
    }
}
