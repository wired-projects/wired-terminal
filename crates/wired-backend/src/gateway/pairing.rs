//! Deciding whether a stranger who messaged the bot may drive your computer.
//!
//! The parameters below are load-bearing, not decoration. This is the only gate
//! between an inbound chat message and a shell running as you, and it replaces
//! the previous answer to "how do I authorise a new device", which was to
//! generate 24 random bytes in a terminal.
//!
//!   • 8 characters from an alphabet with no 0/O/1/I — read aloud, retyped on a
//!     phone, and still unambiguous
//!   • one hour to use it, then it is gone
//!   • at most three requests pending at once, so the list cannot be flooded
//!   • one request per sender per ten minutes
//!   • five wrong codes and approval locks for fifteen minutes
//!   • codes are never written to the log, and the API never returns them to
//!     anyone but the owner's own session
//!
//! Nothing here is persisted: a code that outlives a restart is a code that
//! outlives its hour.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// No 0/O, no 1/I/l — the characters people transcribe wrongly.
const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 8;

const TTL: Duration = Duration::from_secs(60 * 60);
const MAX_PENDING: usize = 3;
const PER_SENDER_COOLDOWN: Duration = Duration::from_secs(10 * 60);
const MAX_FAILED_ATTEMPTS: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
pub struct Request {
    pub platform: &'static str,
    pub chat: i64,
    /// How the owner will recognise the sender: "Sam (@samw)".
    pub display: String,
    code: String,
    created: Instant,
}

impl Request {
    /// Deliberately hides the code: this type ends up in `tracing` fields and
    /// error strings, and a code in a log file is a code on disk forever.
    pub fn redacted(&self) -> Value {
        json!({
            "platform": self.platform,
            "chat": self.chat,
            "display": self.display,
            "expires_in": TTL.saturating_sub(self.created.elapsed()).as_secs(),
        })
    }

    /// The owner's own view, which is the one place a code may appear.
    pub fn for_owner(&self) -> Value {
        let mut value = self.redacted();
        value["code"] = json!(self.code);
        value
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("platform", &self.platform)
            .field("chat", &self.chat)
            .field("code", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
pub enum Outcome {
    /// A code to read back to the sender. Asking twice within the hour returns
    /// the same one rather than minting another.
    Issued(String),
    /// Asking again too soon, or the queue is full.
    Refused(&'static str),
}

#[derive(Default)]
pub struct Pairings {
    pending: Vec<Request>,
    /// Last time each sender asked, so a bot cannot mint codes in a loop.
    last_request: HashMap<(&'static str, i64), Instant>,
    failed: u32,
    locked_until: Option<Instant>,
}

fn new_code() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..CODE_LEN)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

impl Pairings {
    fn expire(&mut self) {
        self.pending.retain(|r| r.created.elapsed() < TTL);
        self.last_request
            .retain(|_, at| at.elapsed() < PER_SENDER_COOLDOWN);
        if self
            .locked_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.locked_until = None;
            self.failed = 0;
        }
    }

    /// An unknown sender said something. Give them a code, or a reason.
    pub fn request(&mut self, platform: &'static str, chat: i64, display: String) -> Outcome {
        self.expire();

        if let Some(existing) = self
            .pending
            .iter()
            .find(|r| r.platform == platform && r.chat == chat)
        {
            return Outcome::Issued(existing.code.clone());
        }
        if self
            .last_request
            .get(&(platform, chat))
            .is_some_and(|at| at.elapsed() < PER_SENDER_COOLDOWN)
        {
            return Outcome::Refused("You already asked recently. Try again in a few minutes.");
        }
        if self.pending.len() >= MAX_PENDING {
            return Outcome::Refused(
                "There are already several requests waiting to be approved. Try again later.",
            );
        }

        let code = new_code();
        self.pending.push(Request {
            platform,
            chat,
            display,
            code: code.clone(),
            created: Instant::now(),
        });
        self.last_request.insert((platform, chat), Instant::now());
        // The code itself is never logged — only that one was issued.
        tracing::info!(platform, chat, "pairing code issued");
        Outcome::Issued(code)
    }

    /// Codes for the owner's screen, newest last.
    pub fn list(&mut self) -> Vec<Value> {
        self.expire();
        self.pending.iter().map(Request::for_owner).collect()
    }

    pub fn is_locked(&mut self) -> Option<u64> {
        self.expire();
        self.locked_until
            .map(|until| until.saturating_duration_since(Instant::now()).as_secs())
    }

    /// The owner approved a code. Returns the request it belonged to.
    pub fn approve(&mut self, code: &str) -> Result<Request, String> {
        self.expire();
        if let Some(seconds) = self.is_locked() {
            return Err(format!(
                "Too many wrong codes. Try again in {} minutes.",
                seconds.div_ceil(60)
            ));
        }

        let wanted = code.trim().to_ascii_uppercase();
        let found = self
            .pending
            .iter()
            .position(|r| r.code.eq_ignore_ascii_case(&wanted));

        match found {
            Some(index) => {
                self.failed = 0;
                let request = self.pending.remove(index);
                self.last_request.remove(&(request.platform, request.chat));
                tracing::info!(
                    platform = request.platform,
                    chat = request.chat,
                    "pairing approved"
                );
                Ok(request)
            }
            None => {
                self.failed += 1;
                if self.failed >= MAX_FAILED_ATTEMPTS {
                    self.locked_until = Some(Instant::now() + LOCKOUT);
                    tracing::warn!("pairing locked after {MAX_FAILED_ATTEMPTS} wrong codes");
                }
                Err("That code does not match a waiting request.".to_string())
            }
        }
    }

    /// Throw a request away without approving it.
    pub fn deny(&mut self, code: &str) -> Result<Request, String> {
        self.expire();
        let wanted = code.trim().to_ascii_uppercase();
        let index = self
            .pending
            .iter()
            .position(|r| r.code.eq_ignore_ascii_case(&wanted))
            .ok_or_else(|| "That code does not match a waiting request.".to_string())?;
        Ok(self.pending.remove(index))
    }

    pub fn clear(&mut self) {
        self.pending.clear();
    }
}
