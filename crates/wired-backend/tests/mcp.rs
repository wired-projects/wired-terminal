//! MCP endpoint tests — spoken over raw JSON-RPC, the way a client does.
//!
//! These deliberately avoid the rmcp client SDK: what matters is that a real
//! MCP client on the wire can initialize, list the tools, and call them, so the
//! test drives the same HTTP surface Claude Code would.

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::{json, Value};
use wired_backend::config::Settings;
use wired_backend::providers::ASSISTANT_PROVIDERS;
use wired_backend::routes::AppState;

const MCP_ACCEPT: &str = "application/json, text/event-stream";

/// Keep the suite out of the developer's real configuration.
///
/// `wired_set_assistant` persists, and `settings.json` lives in a platform
/// directory — without this, running the tests would rewrite the machine's own
/// choice of assistant.
fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("wired-mcp-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("WIRED_CONFIG_DIR", &dir);
        std::env::set_var("WIRED_DATA_DIR", &dir);
        std::env::set_var("WIRED_USE_KEYCHAIN", "0");
    });
}

/// Returns the address plus the state the server is running on, so a test can
/// inspect what the server knows rather than guessing from the outside.
async fn spawn_server_with_state() -> (SocketAddr, AppState) {
    isolate();
    let (state, router) = wired_backend::build(Settings::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, state)
}

async fn spawn_server() -> SocketAddr {
    spawn_server_with_state().await.0
}

/// Responses come back as either JSON or a one-event SSE stream depending on
/// negotiation; unwrap both to the JSON-RPC payload.
fn parse_rpc(body: &str) -> Value {
    // The stream opens with an empty `data:` keep-alive frame before the
    // payload, so take the first frame that actually carries something.
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.trim();
            if rest.is_empty() {
                continue;
            }
            return serde_json::from_str(rest)
                .unwrap_or_else(|e| panic!("SSE data frame was not JSON ({e}): {rest}"));
        }
    }
    serde_json::from_str(body)
        .unwrap_or_else(|e| panic!("body was neither SSE nor JSON ({e}): {body}"))
}

struct McpClient {
    http: reqwest::Client,
    url: String,
    session: Option<String>,
}

impl McpClient {
    fn new(addr: SocketAddr) -> Self {
        Self {
            http: reqwest::Client::new(),
            url: format!("http://{addr}/mcp"),
            session: None,
        }
    }

    async fn send(&mut self, body: Value) -> (u16, Value) {
        let mut req = self
            .http
            .post(&self.url)
            .header("Accept", MCP_ACCEPT)
            .header("Content-Type", "application/json");
        if let Some(id) = &self.session {
            req = req.header("Mcp-Session-Id", id);
        }
        let response = req.json(&body).send().await.unwrap();
        let status = response.status().as_u16();

        if let Some(id) = response.headers().get("mcp-session-id") {
            self.session = Some(id.to_str().unwrap().to_string());
        }
        let text = response.text().await.unwrap();
        if status >= 400 {
            return (
                status,
                serde_json::from_str(&text).unwrap_or(json!({"raw": text})),
            );
        }
        // Notifications carry no id, so the server acknowledges with 202 and an
        // empty body — there is no JSON-RPC envelope to parse.
        if text.trim().is_empty() {
            return (status, Value::Null);
        }
        (status, parse_rpc(&text))
    }

    /// initialize + notifications/initialized — the MCP handshake.
    async fn handshake(&mut self) -> Value {
        let (status, result) = self
            .send(json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "wired-tests", "version": "1.0.0"},
                },
            }))
            .await;
        assert_eq!(status, 200, "initialize failed: {result}");

        let _ = self
            .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        result
    }

    async fn call_tool(&mut self, name: &str, args: Value) -> Value {
        let (status, body) = self
            .send(json!({
                "jsonrpc": "2.0", "id": 99, "method": "tools/call",
                "params": {"name": name, "arguments": args},
            }))
            .await;
        assert_eq!(status, 200, "tools/call {name} failed: {body}");
        body
    }
}

#[tokio::test]
async fn initialize_reports_server_identity_and_instructions() {
    let addr = spawn_server().await;
    let mut client = McpClient::new(addr);
    let result = client.handshake().await;

    let info = &result["result"];
    assert_eq!(info["serverInfo"]["name"], "wired-terminal");
    assert!(
        info["capabilities"]["tools"].is_object(),
        "tools capability missing: {info}"
    );
    let instructions = info["instructions"].as_str().unwrap_or_default();
    assert!(
        instructions.contains("wired_send_task"),
        "instructions should point at the tools: {instructions}"
    );
}

#[tokio::test]
async fn lists_exactly_the_intended_tools() {
    let addr = spawn_server().await;
    let mut client = McpClient::new(addr);
    client.handshake().await;

    let (status, body) = client
        .send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}))
        .await;
    assert_eq!(status, 200, "tools/list failed: {body}");

    let tools = body["result"]["tools"].as_array().expect("no tools array");
    let mut names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    names.sort_unstable();

    assert_eq!(
        names,
        vec![
            "wired_answer_prompt",
            "wired_read_transcript",
            "wired_send_task",
            "wired_session_status",
            "wired_set_assistant",
        ],
        "the tool surface is a deliberate subset — update the docs if you change it"
    );

    // No destructive verbs may leak in. This is the actual guardrail.
    for forbidden in ["start", "stop", "kill", "write", "resize"] {
        assert!(
            !names.iter().any(|n| n.contains(forbidden)),
            "tool surface must not expose '{forbidden}': {names:?}"
        );
    }

    // Descriptions carry the trigger conditions, not just the mechanics.
    for tool in tools {
        let description = tool["description"].as_str().unwrap_or_default();
        assert!(
            description.len() > 120,
            "{} needs a description that says when to call it",
            tool["name"]
        );
        assert!(
            description.contains("Call this") || description.contains("Check whether"),
            "{} should state its trigger condition",
            tool["name"]
        );
    }
}

#[tokio::test]
async fn status_works_without_a_session() {
    let addr = spawn_server().await;
    let mut client = McpClient::new(addr);
    client.handshake().await;

    let body = client.call_tool("wired_session_status", json!({})).await;
    let structured = &body["result"]["structuredContent"];
    assert_eq!(structured["session_running"], false);
    // The shell always resolves, so the probe is doing real work.
    let providers = structured["available_providers"].as_array().unwrap();
    assert!(
        providers.iter().any(|p| p == "shell"),
        "expected shell in {providers:?}"
    );
}

#[tokio::test]
async fn send_task_without_a_session_is_an_error_not_a_panic() {
    let addr = spawn_server().await;
    let mut client = McpClient::new(addr);
    client.handshake().await;

    let body = client
        .call_tool("wired_send_task", json!({"text": "hello"}))
        .await;
    // A tool-level failure is a JSON-RPC error, and it must explain the fix.
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("No agent session"),
        "unhelpful error: {body}"
    );
}

#[tokio::test]
async fn send_task_reaches_a_live_session() {
    let addr = spawn_server().await;

    // `shell` stands in for the agent CLI so this runs anywhere.
    let started = reqwest::Client::new()
        .post(format!("http://{addr}/api/pty/start"))
        .json(&json!({"provider": "shell", "cols": 100, "rows": 32}))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status().as_u16(), 200);
    tokio::time::sleep(Duration::from_millis(700)).await;

    let mut client = McpClient::new(addr);
    client.handshake().await;

    let body = client
        .call_tool(
            "wired_send_task",
            json!({"text": "echo MCP-REACHED-THE-PTY", "wait_seconds": 10}),
        )
        .await;
    let structured = &body["result"]["structuredContent"];
    assert_eq!(structured["sent"], true, "{body}");
    assert_eq!(structured["provider"], "shell");

    let response = structured["response"].as_str().unwrap_or_default();
    assert!(
        response.contains("MCP-REACHED-THE-PTY"),
        "the task never reached the PTY: {response:?}"
    );

    // And the transcript tool sees the same thing.
    let body = client
        .call_tool("wired_read_transcript", json!({"lines": 40}))
        .await;
    let transcript = body["result"]["structuredContent"]["transcript"]
        .as_str()
        .unwrap_or_default();
    assert!(
        transcript.contains("MCP-REACHED-THE-PTY"),
        "transcript missing the exchange: {transcript:?}"
    );

    let _ = reqwest::Client::new()
        .post(format!("http://{addr}/api/pty/kill"))
        .json(&json!({}))
        .send()
        .await;
}

#[tokio::test]
async fn setting_the_assistant_saves_the_choice_and_leaves_the_session_alone() {
    let (addr, state) = spawn_server_with_state().await;

    // `shell` stands in for the CLI that is already running and mid-task.
    let started = reqwest::Client::new()
        .post(format!("http://{addr}/api/pty/start"))
        .json(&json!({"provider": "shell", "cols": 100, "rows": 32}))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status().as_u16(), 200);

    let mut client = McpClient::new(addr);
    client.handshake().await;

    // A name that is not an assistant is refused, not saved.
    let body = client
        .call_tool("wired_set_assistant", json!({"provider": "gpt"}))
        .await;
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("not an assistant"),
        "unhelpful error: {body}"
    );

    let status = client.call_tool("wired_session_status", json!({})).await;
    let available = status["result"]["structuredContent"]["available_providers"]
        .as_array()
        .expect("no provider list")
        .clone();
    let installed = |id: &str| available.iter().any(|p| p == id);

    // Switching to a CLI that is not on the machine would leave the user with
    // an assistant that cannot start, so it is refused too.
    if let Some(missing) = ASSISTANT_PROVIDERS
        .iter()
        .copied()
        .find(|id| !installed(id))
    {
        let body = client
            .call_tool("wired_set_assistant", json!({"provider": missing}))
            .await;
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("not installed"),
            "switching to a missing {missing} should say so: {body}"
        );
    }

    if let Some(real) = ASSISTANT_PROVIDERS.iter().copied().find(|id| installed(id)) {
        let body = client
            .call_tool("wired_set_assistant", json!({"provider": real}))
            .await;
        let structured = &body["result"]["structuredContent"];
        assert_eq!(structured["provider"], real, "{body}");
        assert_eq!(
            structured["restart_needed"], true,
            "a session on another CLI means the switch has not landed: {body}"
        );
        assert!(
            structured["next_step"]
                .as_str()
                .unwrap_or_default()
                .contains("shell"),
            "next_step should name the CLI still running: {structured}"
        );

        // The whole point: the caller may be the session this would have
        // killed, so the running one is untouched and still answering.
        assert!(
            state.manager.running(),
            "changing the preference must not end the live session"
        );
        let status = client.call_tool("wired_session_status", json!({})).await;
        assert_eq!(
            status["result"]["structuredContent"]["provider"], "shell",
            "the live session must keep its own CLI"
        );
    }

    let _ = reqwest::Client::new()
        .post(format!("http://{addr}/api/pty/kill"))
        .json(&json!({}))
        .send()
        .await;
}

#[tokio::test]
async fn the_supervised_agent_cannot_drive_itself() {
    let (addr, state) = spawn_server_with_state().await;

    let started = reqwest::Client::new()
        .post(format!("http://{addr}/api/pty/start"))
        .json(&json!({"provider": "shell", "cols": 100, "rows": 32}))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status().as_u16(), 200);

    // The exact secret the PTY child was handed in its environment.
    let nonce = state
        .manager
        .session_nonce()
        .expect("a running session must have a nonce");

    let blocked = reqwest::Client::new()
        .post(format!("http://{addr}/mcp"))
        .header("Accept", MCP_ACCEPT)
        .header("X-Wired-Session-Nonce", &nonce)
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "the-agent-itself", "version": "1"}},
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        blocked.status().as_u16(),
        409,
        "the supervised session must not be able to drive itself"
    );
    let detail = blocked.text().await.unwrap();
    assert!(
        detail.contains("agent session you are running in"),
        "the refusal should explain why: {detail}"
    );

    // Any other client is unaffected.
    let allowed = reqwest::Client::new()
        .post(format!("http://{addr}/mcp"))
        .header("Accept", MCP_ACCEPT)
        .header("X-Wired-Session-Nonce", "some-other-session")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "t", "version": "1"}},
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status().as_u16(), 200);

    // And the nonce is retired with the session.
    let _ = reqwest::Client::new()
        .post(format!("http://{addr}/api/pty/kill"))
        .json(&json!({}))
        .send()
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(state.manager.session_nonce().is_none());
}
