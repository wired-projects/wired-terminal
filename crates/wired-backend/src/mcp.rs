//! MCP server — the agent-facing surface.
//!
//! The REST API is for scripts and humans; this is the same control plane
//! shaped for a model. Claude Code, Claude Desktop, or anything else speaking
//! MCP can point at `/mcp` and drive the assistant with typed tools instead of
//! translating prose into curl.
//!
//! **The tool set is deliberately small and read-mostly.** Every destructive
//! operation the REST API exposes — starting and killing PTYs, raw writes,
//! stopping the supervisor — is absent here. A tool the model cannot call is
//! the cheapest guardrail available, and it matters more than usual: Wired
//! supervises an agent CLI, so an agent driving Wired can be driving *itself*.
//! See `routes::reject_self_call` for the loop guard that covers the rest.

use std::time::Duration;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler};
use serde::{Deserialize, Serialize};

use crate::keys::{encode_agent_message, resolve_key, SUBMIT_DELAY};
use crate::providers::{probe_providers, resolve_cmd, ASSISTANT_PROVIDERS};
use crate::routes::AppState;
use crate::settings_store;

/// Upper bound on how long one tool call may block.
///
/// An MCP client is waiting synchronously on this, and if the caller happens to
/// be the supervised agent it cannot produce the output it is waiting for — so
/// the wait must always terminate, well inside a client's own timeout.
const MAX_WAIT: f64 = 60.0;

#[derive(Clone)]
pub struct WiredMcp {
    state: AppState,
    // Read by the `#[tool_handler]`-generated ServerHandler impl, which
    // dead-code analysis doesn't follow through the derived Clone.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

// ── tool parameters ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendTaskParams {
    /// The task to give the assistant, written as you would type it to the CLI.
    pub text: String,
    /// Seconds to wait for the assistant to respond before returning, 0–60.
    /// Omit or use 0 to send without waiting, then poll `wired_read_transcript`.
    #[serde(default)]
    pub wait_seconds: Option<f64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadTranscriptParams {
    /// Return only the last N lines. Omit for the whole visible transcript.
    #[serde(default)]
    pub lines: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AnswerPromptParams {
    /// The key to press: `enter` to accept, `esc` to dismiss, `1`–`9` to choose
    /// a numbered option, or `ctrl+c` to interrupt.
    pub key: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetAssistantParams {
    /// Which agent CLI to use from the next session on: `claude`, `grok`,
    /// `codex` or `gemini`.
    pub provider: String,
}

// ── tool results ────────────────────────────────────────────────────────

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SendTaskResult {
    pub sent: bool,
    pub provider: Option<String>,
    /// What the assistant produced while we waited. Empty when `wait_seconds`
    /// was 0 or the assistant was still working when the wait elapsed.
    pub response: String,
    pub waited_seconds: f64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TranscriptResult {
    pub running: bool,
    pub provider: Option<String>,
    pub transcript: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StatusResult {
    /// True when an agent CLI is live and able to accept tasks.
    pub session_running: bool,
    pub provider: Option<String>,
    /// Which agent CLIs are installed on the host.
    pub available_providers: Vec<String>,
    /// True when the supervisor restarts the CLI if it exits.
    pub keep_alive: bool,
    #[schemars(schema_with = "plain_integer")]
    pub restarts_last_hour: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct AnswerPromptResult {
    pub sent: bool,
    pub key: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SetAssistantResult {
    /// The provider now saved as the preference.
    pub provider: String,
    /// What the preference was before this call.
    pub previous: Option<String>,
    /// True when a session is still running the old CLI, so the change has not
    /// taken effect yet. `next_step` says what closes the gap.
    pub restart_needed: bool,
    /// Plain sentence describing what happens next — relay it to the user.
    pub next_step: String,
}

/// A bare JSON integer.
///
/// schemars tags `u64` with `format: "uint64"`, which is legal JSON Schema but
/// not a format MCP clients know — Claude Code prints an "unknown format"
/// warning for every such field. The count needs no format to be understood.
fn plain_integer(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({ "type": "integer", "minimum": 0 })
}

fn no_session() -> ErrorData {
    ErrorData::invalid_request(
        "No agent session is running. Ask the operator to start one — this tool set \
         deliberately cannot start or stop sessions."
            .to_string(),
        None,
    )
}

#[tool_router]
impl WiredMcp {
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    /// Send a task to the running assistant.
    #[tool(
        name = "wired_send_task",
        description = "Give the running agent assistant a task, as if typed at its \
prompt. Call this when the user asks you to delegate work to their 24/7 assistant, to run \
something on the machine the assistant supervises, or to follow up on work it is already doing. \
Set wait_seconds (up to 60) to get the assistant's reply in the same call; leave it 0 to send \
and return immediately, then read the result later with wired_read_transcript. Requires a \
session to already be running — check wired_session_status first if unsure."
    )]
    async fn send_task(
        &self,
        Parameters(params): Parameters<SendTaskParams>,
    ) -> Result<Json<SendTaskResult>, ErrorData> {
        if params.text.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "text must not be empty".to_string(),
                None,
            ));
        }
        if !self.state.manager.running() {
            return Err(no_session());
        }

        let since = self.state.manager.current_seq();
        let (body, submit) = encode_agent_message(&params.text, true, true);

        self.state
            .manager
            .write(&body)
            .map_err(|e| ErrorData::internal_error(e, None))?;
        if !submit.is_empty() {
            tokio::time::sleep(SUBMIT_DELAY).await;
            self.state
                .manager
                .write(&submit)
                .map_err(|e| ErrorData::internal_error(e, None))?;
        }

        let wait = params.wait_seconds.unwrap_or(0.0).clamp(0.0, MAX_WAIT);
        let response = if wait > 0.0 {
            let manager = self.state.manager.clone();
            tokio::task::spawn_blocking(move || {
                manager.wait_output(
                    since,
                    Duration::from_secs_f64(wait),
                    Duration::from_millis(1500),
                    true,
                )
            })
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            .text
        } else {
            String::new()
        };

        Ok(Json(SendTaskResult {
            sent: true,
            provider: self.state.manager.provider(),
            response,
            waited_seconds: wait,
        }))
    }

    /// Read what the assistant has said.
    #[tool(
        name = "wired_read_transcript",
        description = "Read the assistant's current conversation — what it has been asked and what \
it has replied. Call this after wired_send_task when you did not wait for the reply, when the \
user asks what their assistant is doing or has done, or to check whether a long-running task \
has finished. Returns the readable transcript with the CLI's banners, spinners and status bars \
already stripped out."
    )]
    async fn read_transcript(
        &self,
        Parameters(params): Parameters<ReadTranscriptParams>,
    ) -> Result<Json<TranscriptResult>, ErrorData> {
        let manager = self.state.manager.clone();
        let out = tokio::task::spawn_blocking(move || manager.get_output_full(true, false))
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let transcript = match params.lines {
            Some(n) if n > 0 => {
                let all: Vec<&str> = out.text.lines().collect();
                all[all.len().saturating_sub(n)..].join("\n")
            }
            _ => out.text,
        };

        Ok(Json(TranscriptResult {
            running: out.running,
            provider: out.provider,
            transcript,
        }))
    }

    /// Report whether the assistant is up.
    #[tool(
        name = "wired_session_status",
        description = "Check whether the user's assistant is running, which agent CLI it is using, \
and whether keep-alive is on. Call this before sending a task if you are unsure a session \
exists, or when the user asks whether their assistant is up. Cheap and side-effect free — \
prefer it over guessing."
    )]
    async fn session_status(&self) -> Result<Json<StatusResult>, ErrorData> {
        let status = self.state.assistant.status();
        let available = probe_providers()
            .into_iter()
            .filter(|p| p.available)
            .map(|p| p.id)
            .collect();

        Ok(Json(StatusResult {
            session_running: self.state.manager.running(),
            provider: self.state.manager.provider(),
            available_providers: available,
            keep_alive: status["keep_alive"].as_bool().unwrap_or(false),
            restarts_last_hour: status["restarts_last_hour"].as_u64().unwrap_or(0),
        }))
    }

    /// Answer a pending approval prompt.
    #[tool(
        name = "wired_answer_prompt",
        description = "Answer a question the assistant is blocked on — an approval dialog or a \
numbered choice. Call this only when wired_read_transcript shows the assistant is waiting for \
input; sending keys at any other time types them into its composer. Use `enter` to accept, \
`esc` to dismiss, a digit to pick a numbered option, or `ctrl+c` to interrupt what it is doing."
    )]
    async fn answer_prompt(
        &self,
        Parameters(params): Parameters<AnswerPromptParams>,
    ) -> Result<Json<AnswerPromptResult>, ErrorData> {
        if !self.state.manager.running() {
            return Err(no_session());
        }
        // A bare digit is a menu choice, not a named key.
        let payload = match params.key.trim() {
            digit if digit.len() == 1 && digit.chars().all(|c| c.is_ascii_digit()) => {
                digit.as_bytes().to_vec()
            }
            named => resolve_key(named).map_err(|e| ErrorData::invalid_params(e, None))?,
        };

        self.state
            .manager
            .write(&payload)
            .map_err(|e| ErrorData::internal_error(e, None))?;

        Ok(Json(AnswerPromptResult {
            sent: true,
            key: params.key,
        }))
    }

    /// Change which agent CLI the assistant runs — the preference, not the
    /// running process.
    ///
    /// Switching means replacing one CLI with another, which ends the session
    /// and everything it was in the middle of. This tool deliberately stops at
    /// the setting: the caller may well *be* the session it would be killing,
    /// and the nonce guard that would catch that is a footgun guard rather than
    /// a boundary — omit the header and it is gone. So the restart stays with
    /// the operator, or with keep-alive once the old CLI exits on its own.
    #[tool(
        name = "wired_set_assistant",
        description = "Choose which agent CLI the user's assistant runs — `claude`, `grok`, `codex` \
or `gemini`. \
Call this when the user asks to switch or change their assistant. It saves the preference and \
stops there: the CLI already running is a different binary and keeps going until it exits, so \
read `restart_needed` and relay `next_step` rather than reporting the switch as done. Ending a \
session is deliberately not something this tool set can do."
    )]
    async fn set_assistant(
        &self,
        Parameters(params): Parameters<SetAssistantParams>,
    ) -> Result<Json<SetAssistantResult>, ErrorData> {
        let wanted = params.provider.trim().to_ascii_lowercase();
        if !ASSISTANT_PROVIDERS.contains(&wanted.as_str()) {
            return Err(ErrorData::invalid_params(
                format!("'{wanted}' is not an assistant. Choose one of {ASSISTANT_PROVIDERS:?}."),
                None,
            ));
        }
        // Saving a CLI that isn't there would leave the user with an assistant
        // that cannot start, and the failure would surface much later.
        if resolve_cmd(&wanted).is_none() {
            return Err(ErrorData::invalid_request(
                format!(
                    "{wanted} is not installed on this machine, so switching to it would leave \
                     the user without a working assistant. They can install it from Wired's \
                     setup screen."
                ),
                None,
            ));
        }

        let before = self.state.assistant.status();
        let previous = before["provider"].as_str().map(String::from);
        let keep_alive = before["keep_alive"].as_bool().unwrap_or(false);

        settings_store::update(|s| s.assistant = Some(wanted.clone()))
            .map_err(|e| ErrorData::internal_error(e, None))?;
        self.state
            .assistant
            .configure(Some(&wanted), None, None, None, None)
            .map_err(|e| ErrorData::internal_error(e, None))?;

        let live = self.state.manager.provider();
        let stale = self.state.manager.running() && live.as_deref() != Some(wanted.as_str());
        let running_now = live.unwrap_or_else(|| "the old CLI".to_string());

        let next_step = match (stale, keep_alive) {
            (false, _) if !self.state.manager.running() => {
                format!("No session is running, so the next one starts as {wanted}.")
            }
            (false, _) => format!("The running session is already {wanted}; nothing else to do."),
            (true, true) => format!(
                "The session running now is {running_now} and keeps that CLI until it exits — \
                 always-on then brings it back as {wanted}. To switch immediately, the operator \
                 restarts the assistant from Wired."
            ),
            (true, false) => format!(
                "The session running now is {running_now} and keeps that CLI until it exits. To \
                 switch, the operator restarts the assistant from Wired."
            ),
        };

        Ok(Json(SetAssistantResult {
            provider: wanted,
            previous,
            restart_needed: stale,
            next_step,
        }))
    }
}

#[tool_handler]
impl ServerHandler for WiredMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("wired-terminal", env!("CARGO_PKG_VERSION"))
                    .with_title("Wired Terminal")
                    .with_description("Drive the user's 24/7 agent CLI assistant."),
            )
            .with_instructions(
                "This machine runs a persistent agent CLI that the user treats as their \
                 always-on assistant. Send it work with wired_send_task and read what it did \
                 with wired_read_transcript.\n\n\
                 Starting, stopping and killing sessions are deliberately not exposed — if no \
                 session is running, say so and let the operator start one. wired_set_assistant \
                 changes which CLI runs next for the same reason: it saves the choice, and the \
                 session already running keeps its own CLI until it ends.",
            )
    }
}
