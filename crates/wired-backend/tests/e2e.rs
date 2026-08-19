//! End-to-end tests against a real server on a real socket.
//!
//! These boot the actual router, fork a real PTY and talk over HTTP and a
//! WebSocket — the parts unit tests cannot reach. `shell` stands in for the
//! agent CLIs so the suite runs anywhere, with no login and no network.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use futures_util::StreamExt;

use serde_json::{json, Value};
use wired_backend::config::Settings;
use wired_backend::providers::ASSISTANT_PROVIDERS;

/// Keep the suite out of the developer's real configuration.
///
/// `settings.json`, the transcript store and the schedule file all live in
/// platform directories; a test that wrote to them would edit the machine it is
/// running on. `Settings::default()` already leaves persistence off — this is
/// the belt to that pair of braces, and it also stops the store from picking up
/// whatever the developer has configured for themselves.
fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("wired-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("WIRED_CONFIG_DIR", &dir);
        std::env::set_var("WIRED_DATA_DIR", &dir);
        // Reading a keychain can raise a password prompt, which in a headless
        // test run is a hang rather than a failure.
        std::env::set_var("WIRED_USE_KEYCHAIN", "0");
    });
}

/// Boot a server on an ephemeral port and hand back its address.
async fn spawn_server() -> SocketAddr {
    isolate();
    let settings = Settings::default();
    let (_state, router) = wired_backend::build(settings);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    // The socket is already bound and listening, so one yield is enough.
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    let response = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
    let status = response.status().as_u16();
    (status, response.text().await.unwrap())
}

async fn post(addr: SocketAddr, path: &str, body: Value) -> (u16, String) {
    let response = reqwest::Client::new()
        .post(format!("http://{addr}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.text().await.unwrap())
}

#[tokio::test]
async fn healthz_is_public() {
    let addr = spawn_server().await;
    let (status, body) = get(addr, "/healthz").await;
    assert_eq!(status, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["service"], "wired-terminal");
}

#[tokio::test]
async fn providers_are_probed() {
    let addr = spawn_server().await;
    let (status, body) = get(addr, "/api/providers").await;
    assert_eq!(status, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    // The shell always exists; the agent CLIs may or may not be installed here.
    assert_eq!(json["shell"]["available"], true);
    for provider in ASSISTANT_PROVIDERS {
        assert!(
            json[provider].is_object(),
            "{provider} was not probed: {body}"
        );
    }
}

#[tokio::test]
async fn reader_page_is_served() {
    let addr = spawn_server().await;
    let (status, body) = get(addr, "/reader").await;
    assert_eq!(status, 200);
    assert!(body.contains("EventSource"), "reader.html was not served");
}

#[tokio::test]
async fn api_rejects_a_foreign_origin() {
    let addr = spawn_server().await;
    let response = reqwest::Client::new()
        .get(format!("http://{addr}/api/agent/status"))
        .header("Origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 403);
}

#[tokio::test]
async fn websocket_streams_injected_command() {
    let addr = spawn_server().await;

    let (status, _) = post(
        addr,
        "/api/pty/start",
        json!({"provider": "shell", "cols": 100, "rows": 32}),
    )
    .await;
    assert_eq!(status, 200);

    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("websocket upgrade");

    // First frame is always the session snapshot.
    let init = socket.next().await.unwrap().unwrap();
    let init: Value = serde_json::from_str(init.to_text().unwrap()).unwrap();
    assert_eq!(init["type"], "init");
    assert_eq!(init["running"], true);
    assert_eq!(init["provider"], "shell");

    let marker = "WIRED-E2E-MARKER";
    let (status, _) = post(
        addr,
        "/api/command",
        json!({"command": format!("echo {marker}")}),
    )
    .await;
    assert_eq!(status, 200);

    // The shell echoes the typed line and then its output; either arrival of
    // the marker proves the PTY → WebSocket path works.
    let saw_marker = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(msg)) = socket.next().await {
            let Ok(text) = msg.to_text() else { continue };
            let Ok(frame) = serde_json::from_str::<Value>(text) else {
                continue;
            };
            if frame["type"] != "data" {
                continue;
            }
            let chunk = frame["chunk"].as_str().unwrap_or_default();
            let raw = base64::engine::general_purpose::STANDARD
                .decode(chunk)
                .unwrap_or_default();
            if String::from_utf8_lossy(&raw).contains(marker) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    assert!(saw_marker, "never saw {marker} on the websocket");

    let _ = socket.close(None).await;
    let (status, _) = post(addr, "/api/pty/kill", json!({})).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn approve_without_a_session_is_a_conflict() {
    // The buttons in the app, in /reader and in Telegram all land here; when
    // nothing is waiting, they must say so rather than write into the void.
    let addr = spawn_server().await;
    let (status, body) = post(addr, "/api/agent/approve", json!({"allow": true})).await;
    assert_eq!(status, 409);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert!(json["detail"]
        .as_str()
        .unwrap()
        .contains("No active agent session"));
}

#[tokio::test]
async fn settings_describe_what_the_screen_can_change() {
    let addr = spawn_server().await;
    let (status, body) = get(addr, "/api/settings").await;
    assert_eq!(status, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    for field in ["assistant", "folder", "always_on"] {
        assert!(!json[field].is_null(), "{field} missing from /api/settings");
    }
    // A setting the environment has taken over is reported, not silently
    // overridden by a control that appears to work.
    assert!(json["env_overrides"].is_array());
}

#[tokio::test]
async fn switching_assistant_asks_for_a_restart() {
    // Picking a different assistant cannot reach the CLI already running — it is
    // a different binary — and the supervisor leaves a live session alone. The
    // save has to say so, or the switch looks applied when it is not.
    let addr = spawn_server().await;

    let (status, _) = post(
        addr,
        "/api/pty/start",
        json!({"provider": "shell", "cols": 100, "rows": 32}),
    )
    .await;
    assert_eq!(status, 200);

    // Saving the provider that is already running is not a switch.
    let (status, body) = post(addr, "/api/settings", json!({"assistant": "shell"})).await;
    assert_eq!(status, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["restart_assistant"], false);

    // An actual switch has to bring the session back up as the new CLI. Only
    // provable where that CLI exists, which is not every machine.
    let (_, body) = get(addr, "/api/providers").await;
    let providers: Value = serde_json::from_str(&body).unwrap();
    if let Some(installed) = ASSISTANT_PROVIDERS
        .iter()
        .copied()
        .find(|id| providers[id]["available"] == true)
    {
        let (status, body) = post(addr, "/api/settings", json!({"assistant": installed})).await;
        assert_eq!(status, 200);
        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            json["restart_assistant"], true,
            "switching from shell to {installed} did not ask for a restart"
        );
        assert_eq!(json["settings"]["assistant"], installed);
    }

    let (status, _) = post(addr, "/api/pty/kill", json!({})).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn setup_state_drives_the_wizard() {
    let addr = spawn_server().await;
    let (status, body) = get(addr, "/api/setup/state").await;
    assert_eq!(status, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert!(json["node"]["found"].is_boolean());
    // Every agent CLI is offered by the wizard; the shell is not one to install.
    assert!(json["providers"]
        .as_array()
        .is_some_and(|p| p.len() == ASSISTANT_PROVIDERS.len()));
    assert_eq!(json["install"]["status"], "idle");
    assert!(json["folder"]["suggested"]
        .as_str()
        .is_some_and(|s| s.ends_with("Wired")));
}

#[tokio::test]
async fn chat_bridge_is_off_until_it_is_configured() {
    let addr = spawn_server().await;
    let (status, body) = get(addr, "/api/gateway/status").await;
    assert_eq!(status, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["enabled"], false);
    assert_eq!(json["configured"], false);
    assert_eq!(json["connected"], false);
    assert_eq!(json["paired_chats"], 0);
}

#[tokio::test]
async fn resetting_the_chat_bridge_leaves_it_unconfigured() {
    let addr = spawn_server().await;
    let (status, body) = post(addr, "/api/gateway/reset", json!({})).await;
    assert_eq!(status, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["gateway"]["configured"], false);
    assert_eq!(json["gateway"]["enabled"], false);
    assert_eq!(json["gateway"]["paired_chats"], 0);
    assert_eq!(json["gateway"]["bot"], Value::Null);
}

#[tokio::test]
async fn a_schedule_it_cannot_read_gets_an_example_back() {
    let addr = spawn_server().await;
    let (status, body) = post(
        addr,
        "/api/schedules",
        json!({"task": "check the tree", "when": "whenever I feel like it"}),
    )
    .await;
    assert_eq!(status, 400);
    let json: Value = serde_json::from_str(&body).unwrap();
    let detail = json["detail"].as_str().unwrap();
    assert!(detail.contains("every morning at 8"), "got: {detail}");
}

#[tokio::test]
async fn a_schedule_says_back_when_it_will_run() {
    let addr = spawn_server().await;
    let (status, body) = post(
        addr,
        "/api/schedules",
        json!({"task": "tell me what changed", "when": "every morning at 8"}),
    )
    .await;
    assert_eq!(status, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["schedule"]["when_readable"], "every day at 08:00");
    // Named from the task when the user did not name it.
    assert_eq!(json["schedule"]["name"], "tell me what changed");
    assert!(json["schedule"]["next_readable"].as_str().is_some());
}

#[tokio::test]
async fn diagnostics_names_every_link_in_the_chain() {
    let addr = spawn_server().await;
    let (status, body) = get(addr, "/api/diagnostics").await;
    assert_eq!(status, 200);
    let json: Value = serde_json::from_str(&body).unwrap();
    let ids: Vec<&str> = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    for expected in ["cli", "login", "folder", "port", "logs"] {
        assert!(
            ids.contains(&expected),
            "missing check '{expected}' in {ids:?}"
        );
    }
    assert!(json["version"].as_str().is_some());
}

#[tokio::test]
async fn erasing_everything_needs_the_word() {
    let addr = spawn_server().await;
    let (status, _) = post(addr, "/api/diagnostics/reset", json!({})).await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn message_without_a_session_is_a_conflict() {
    let addr = spawn_server().await;
    let (status, body) = post(
        addr,
        "/api/agent/message",
        json!({"text": "hello", "ensure_session": false}),
    )
    .await;
    assert_eq!(status, 409);
    // The frontend unwraps `detail`; keep that shape.
    let json: Value = serde_json::from_str(&body).unwrap();
    assert!(json["detail"]
        .as_str()
        .unwrap()
        .contains("No active agent session"));
}

/// A transport failure must not put the bot token in the error string.
///
/// `Client::post(url).send()` returns an error whose `Display` appends the
/// request URL, and the Telegram URL carries the token — so that string would
/// otherwise reach the journal, `gateway.last_error`, and the API. This pins
/// the `without_url()` behaviour `gateway/telegram.rs` depends on rather than
/// the call site, which cannot be reached without a reachable api.telegram.org.
#[tokio::test]
async fn a_failed_telegram_call_does_not_leak_the_token() {
    const TOKEN: &str = "8749159185:AAFakeTokenForTheTestOnly";
    // Port 1 on loopback refuses instantly: no network, no timeout, no flake.
    let url = format!("http://127.0.0.1:1/bot{TOKEN}/getUpdates");

    let error = reqwest::Client::new()
        .post(&url)
        .send()
        .await
        .expect_err("connecting to port 1 must fail");

    // The unredacted form is what we must never format.
    assert!(
        error.to_string().contains(TOKEN),
        "test is not exercising the leak any more: {error}"
    );
    assert!(
        !error.without_url().to_string().contains(TOKEN),
        "without_url() no longer strips the token"
    );
}
