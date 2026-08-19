//! Request bodies for the Wired API.
//!
//! Ranges are clamped rather than rejected: these are convenience bounds on a
//! terminal geometry, not a security boundary, and a script that asks for 900
//! columns is better served than 422'd. Lengths *are* rejected — an oversized
//! body is a mistake worth naming.

use serde::Deserialize;

pub const MAX_MESSAGE_CHARS: usize = 100_000;

fn default_true() -> bool {
    true
}
fn default_idle() -> f64 {
    1.5
}
fn default_cols_80() -> u16 {
    80
}
fn default_rows_24() -> u16 {
    24
}
fn default_cols_100() -> u16 {
    100
}
fn default_rows_32() -> u16 {
    32
}

#[derive(Debug, Deserialize)]
pub struct CommandRequest {
    pub command: String,
    #[serde(default)]
    pub submit: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct WriteRequest {
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub hex: Option<String>,
    #[serde(default = "default_true")]
    pub interpret_escapes: bool,
}

#[derive(Debug, Deserialize)]
pub struct StartRequest {
    pub provider: String,
    #[serde(default = "default_cols_80")]
    pub cols: u16,
    #[serde(default = "default_rows_24")]
    pub rows: u16,
}

#[derive(Debug, Deserialize)]
pub struct ResizeRequest {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Deserialize)]
pub struct KeyRequest {
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub struct AgentMessageRequest {
    /// Message / task for the live agent CLI.
    pub text: String,
    /// Press Enter (CR) after typing.
    #[serde(default = "default_true")]
    pub submit: bool,
    /// Treat newlines as soft line-breaks (LF), then optional CR submit.
    #[serde(default = "default_true")]
    pub multiline: bool,
    /// Auto-start keep-alive assistant if no session is running.
    #[serde(default = "default_true")]
    pub ensure_session: bool,
    /// After send, wait up to N seconds collecting terminal output (0 = don't wait).
    #[serde(default)]
    pub wait_seconds: f64,
    /// When waiting: return early after this many quiet seconds.
    #[serde(default = "default_idle")]
    pub idle_seconds: f64,
    /// Return cleaned human-readable text (strip TUI chrome / ANSI).
    #[serde(default = "default_true")]
    pub plain: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct AssistantConfigRequest {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub keep_alive: Option<bool>,
    #[serde(default)]
    pub auto_start: Option<bool>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

#[derive(Debug, Deserialize)]
pub struct AgentStartRequest {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default = "default_cols_100")]
    pub cols: u16,
    #[serde(default = "default_rows_32")]
    pub rows: u16,
    #[serde(default = "default_true")]
    pub keep_alive: bool,
}

impl Default for AgentStartRequest {
    fn default() -> Self {
        Self {
            provider: None,
            cols: default_cols_100(),
            rows: default_rows_32(),
            keep_alive: true,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct OutputQuery {
    #[serde(default)]
    pub since: u64,
    #[serde(default = "default_true")]
    pub plain: bool,
    #[serde(default)]
    pub wait: f64,
    #[serde(default = "default_idle")]
    pub idle: f64,
    #[serde(default)]
    pub full: bool,
}

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    #[serde(default)]
    pub since: u64,
    /// Keep TUI chrome (debug).
    #[serde(default)]
    pub chrome: bool,
}

#[derive(Debug, Deserialize)]
pub struct ApproveRequest {
    /// `true` answers the agent's approval dialog with yes.
    pub allow: bool,
}

/// What a non-coder actually changes, plus the ones the wizard sets.
/// Every field is optional: the Settings screen sends only what moved.
#[derive(Debug, Default, Deserialize)]
pub struct SettingsRequest {
    #[serde(default)]
    pub assistant: Option<String>,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub always_on: Option<bool>,
    #[serde(default)]
    pub ask_before_acting: Option<bool>,
    #[serde(default)]
    pub auto_start: Option<bool>,
    #[serde(default)]
    pub start_at_login: Option<bool>,
    #[serde(default)]
    pub notifications: Option<bool>,
    #[serde(default)]
    pub port: Option<u16>,
    /// Empty string clears it and reopens the API to loopback callers.
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub onboarded: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderRequest {
    pub provider: String,
}

#[derive(Debug, Deserialize)]
pub struct FolderRequest {
    pub folder: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct GatewayRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
    /// The token from BotFather. Stored in the OS keychain when there is one.
    #[serde(default)]
    pub bot_token: Option<String>,
    #[serde(default)]
    pub muted: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PairingRequest {
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct UnpairRequest {
    pub chat: i64,
}

#[derive(Debug, Deserialize)]
pub struct ScheduleIdRequest {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub day: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default = "default_history_limit")]
    pub limit: usize,
}

fn default_history_limit() -> usize {
    1000
}
