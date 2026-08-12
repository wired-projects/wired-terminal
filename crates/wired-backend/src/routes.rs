//! HTTP + WebSocket surface.
//!
//! Every `/api` route sits behind `require_auth`, which enforces the Origin
//! allow-list and — when `WIRED_AUTH_TOKEN` is set — a bearer token. See
//! `config.rs` for why the defaults are closed.
//!
//! Errors are `{"detail": "..."}` with a matching status, which is the shape
//! every client here already unwraps.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use serde_json::{json, Value};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::agent_io;
use crate::assistant::Assistant;
use crate::config::Settings;
use crate::diagnostics;
use crate::gateway::{Gateway, Hub};
use crate::keys::{decode_escapes, encode_agent_message, resolve_key, SUBMIT_DELAY};
use crate::mcp::WiredMcp;
use crate::models::*;
use crate::providers::{auto_approve_enabled, probe_providers};
use crate::pty::PtyManager;
use crate::recorder::{EventKind, Recorder};
use crate::schedule::Schedule;
use crate::scheduler::Scheduler;
use crate::security::{origin_valid, presented_token, token_valid};
use crate::settings_store;
use crate::setup::{self, Installer};
use crate::transcript::TranscriptTail;

const READER_HTML: &str = include_str!("../assets/reader.html");

#[derive(Clone)]
pub struct AppState {
    pub settings: Settings,
    pub manager: PtyManager,
    pub assistant: Assistant,
    pub recorder: Recorder,
    pub gateway: Gateway,
    pub scheduler: Scheduler,
    pub installer: Installer,
}

impl AppState {
    /// The subset the chat bridge and the scheduler drive. Passing this rather
    /// than `AppState` keeps `Gateway` from having to contain the state that
    /// contains it.
    pub fn hub(&self) -> Hub {
        Hub {
            manager: self.manager.clone(),
            assistant: self.assistant.clone(),
            recorder: self.recorder.clone(),
            gateway: self.gateway.clone(),
        }
    }
}

// ── errors ──────────────────────────────────────────────────────────────

pub struct ApiError(StatusCode, String);

impl ApiError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    fn conflict(msg: impl Into<String>) -> Self {
        Self(StatusCode::CONFLICT, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "detail": self.1 }))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

// ── auth ────────────────────────────────────────────────────────────────

async fn require_auth(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let headers = request.headers();
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());

    if !origin_valid(&state.settings, origin) {
        tracing::warn!(?origin, "rejected request from origin");
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "detail": "Origin not allowed. Set WIRED_ALLOWED_ORIGINS to permit it."
            })),
        )
            .into_response();
    }

    if !state.settings.auth_required() {
        return next.run(request).await;
    }

    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let query_token = query_param(request.uri().query().unwrap_or(""), "token");
    let presented = presented_token(authorization, query_token.as_deref());

    if !token_valid(&state.settings, &presented) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(json!({
                "detail": "Missing or invalid token. Send `Authorization: Bearer <WIRED_AUTH_TOKEN>`."
            })),
        )
            .into_response();
    }
    next.run(request).await
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

/// Minimal percent-decoding — tokens are URL-safe base64, so this only has to
/// survive the encoding a client might apply, not decode arbitrary UTF-8.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ── router ──────────────────────────────────────────────────────────────

pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(if state.settings.allow_any_origin {
            AllowOrigin::any()
        } else {
            AllowOrigin::list(
                state
                    .settings
                    .allowed_origins
                    .iter()
                    .filter_map(|o| o.parse().ok()),
            )
        })
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);
    // Deliberately no allow_credentials: auth travels in an Authorization
    // header, never a cookie. That is what makes an allow-list meaningful.

    let api = Router::new()
        .route("/api/health", get(health))
        .route("/api/providers", get(list_providers))
        .route("/api/agent/status", get(agent_status))
        .route("/api/agent/configure", post(agent_configure))
        .route("/api/agent/start", post(agent_start))
        .route("/api/agent/message", post(agent_message))
        .route("/api/agent/output", get(agent_output))
        .route("/api/agent/output/text", get(agent_output_text))
        .route("/api/agent/output/stream", get(agent_output_stream))
        .route("/api/agent/key", post(agent_key))
        .route("/api/agent/approve", post(agent_approve))
        .route("/api/agent/stop", post(agent_stop))
        // Everything a person changes by clicking rather than by exporting.
        .route("/api/settings", get(read_settings).post(write_settings))
        .route("/api/setup/state", get(setup_state))
        .route("/api/setup/install", post(setup_install))
        .route("/api/setup/login", post(setup_login))
        .route("/api/setup/folder", post(setup_folder))
        .route("/api/gateway/status", get(gateway_status))
        .route("/api/gateway/configure", post(gateway_configure))
        .route("/api/gateway/reset", post(gateway_reset))
        .route("/api/gateway/pairings", get(gateway_pairings))
        .route("/api/gateway/pairings/approve", post(gateway_approve))
        .route("/api/gateway/pairings/deny", post(gateway_deny))
        .route("/api/gateway/unpair", post(gateway_unpair))
        .route("/api/schedules", get(list_schedules).post(save_schedule))
        .route("/api/schedules/delete", post(delete_schedule))
        .route("/api/schedules/run", post(run_schedule))
        .route("/api/history/days", get(history_days))
        .route("/api/history/day", get(history_day))
        .route("/api/history/search", get(history_search))
        .route("/api/diagnostics", get(diagnostics_report))
        .route("/api/diagnostics/reset", post(diagnostics_reset))
        .route("/api/pty/start", post(start_pty))
        .route("/api/pty/write", post(write_pty))
        .route("/api/pty/key", post(send_key))
        .route("/api/pty/resize", post(resize_pty))
        .route("/api/pty/kill", post(kill_pty))
        .route("/api/pty/status", get(pty_status))
        .route("/api/command", post(send_command))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let mcp_state = state.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(WiredMcp::new(mcp_state.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    let mcp_routes = Router::new()
        .route_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            reject_self_call,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/reader", get(reader))
        .route("/ws", get(websocket_handler))
        .merge(api)
        .merge(mcp_routes)
        .layer(cors)
        .with_state(state)
}

/// Refuse MCP calls that come from the agent this server is supervising.
///
/// Wired runs an agent CLI; that agent can be configured with MCP servers. Point
/// it at this one and it can drive the session it is itself running in — typing
/// into its own composer and then blocking on a reply it cannot produce, because
/// it is blocked in the tool call. The agent has no way to notice this: from its
/// side it is an ordinary HTTP tool.
///
/// So we hand each PTY child a per-session nonce in its environment and refuse
/// any request that presents it. This is a footgun guard, not a security
/// boundary — anyone can omit the header. It costs one line of MCP config
/// (`"headers": {"X-Wired-Session-Nonce": "${WIRED_SESSION_NONCE}"}`) and turns
/// a silent, confusing hang into a clear error.
async fn reject_self_call(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get("x-wired-session-nonce")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !presented.is_empty() && state.manager.session_nonce().as_deref() == Some(presented) {
        tracing::warn!("refused an MCP call from the supervised agent session");
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "detail": "This MCP server drives the agent session you are running in. Calling it would make you send tasks to yourself and wait for a reply you cannot give. Do the work directly instead."
            })),
        )
            .into_response();
    }
    next.run(request).await
}

// ── public ──────────────────────────────────────────────────────────────

/// Liveness only — deliberately leaks nothing about the host.
async fn healthz() -> Json<Value> {
    Json(json!({"ok": true, "service": "wired-terminal"}))
}

/// Readable live transcript — the same SSE feed as `curl -N`, rendered.
///
/// Served from the API so EventSource is same-origin and no build step or
/// frontend dev server is needed (append `?token=…` when a token is set).
async fn reader() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        READER_HTML,
    )
        .into_response()
}

// ── health / providers ──────────────────────────────────────────────────

async fn health(State(state): State<AppState>) -> Json<Value> {
    let providers: HashMap<String, Value> = probe_providers()
        .into_iter()
        .map(|p| {
            (
                p.id.clone(),
                json!({"available": p.available, "path": p.path}),
            )
        })
        .collect();

    let gateway = state.gateway.status();
    Json(json!({
        "ok": true,
        "version": diagnostics::VERSION,
        "providers": providers,
        "assistant": state.assistant.status(),
        // The badge and the notification both hang off this: an approval nobody
        // answers is an agent that has silently stopped.
        "pending_prompt": state.recorder.pending_prompt(),
        "onboarded": settings_store::get().onboarded,
        "folder": crate::providers::agent_cwd().display().to_string(),
        "chat": {
            "enabled": gateway["enabled"],
            "connected": gateway["connected"],
            "pending_pairings": gateway["pending"].as_array().map(|p| p.len()).unwrap_or(0),
        },
        "security": {
            "auth_required": state.settings.auth_required(),
            "loopback_only": state.settings.is_loopback(),
            "agent_auto_approve": auto_approve_enabled(),
        },
    }))
}

async fn list_providers() -> Json<Value> {
    let providers: HashMap<String, Value> = probe_providers()
        .into_iter()
        .map(|p| {
            (
                p.id.clone(),
                json!({
                    "id": p.id,
                    "label": p.label,
                    "available": p.available,
                    "path": p.path,
                    "detail": p.detail,
                }),
            )
        })
        .collect();
    Json(json!(providers))
}

// ── agent API ───────────────────────────────────────────────────────────

async fn agent_status(State(state): State<AppState>) -> Json<Value> {
    let snap = state.manager.snapshot();
    let providers: HashMap<String, Value> = probe_providers()
        .into_iter()
        .map(|p| {
            (
                p.id.clone(),
                json!({"available": p.available, "path": p.path, "label": p.label}),
            )
        })
        .collect();

    Json(json!({
        "providers": providers,
        "assistant": state.assistant.status(),
        "session": {
            "running": snap.running,
            "provider": snap.provider,
            "generation": snap.generation,
        },
    }))
}

async fn agent_configure(
    State(state): State<AppState>,
    Json(req): Json<AssistantConfigRequest>,
) -> ApiResult {
    state
        .assistant
        .configure(
            req.provider.as_deref(),
            req.keep_alive,
            req.auto_start,
            req.cols,
            req.rows,
        )
        .map_err(ApiError::bad_request)?;
    Ok(Json(
        json!({"status": "success", "assistant": state.assistant.status()}),
    ))
}

/// Start an agent CLI as your personal assistant (with keep-alive).
async fn agent_start(
    State(state): State<AppState>,
    body: Option<Json<AgentStartRequest>>,
) -> ApiResult {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    state
        .assistant
        .configure(
            req.provider.as_deref(),
            Some(req.keep_alive),
            None,
            Some(req.cols),
            Some(req.rows),
        )
        .map_err(ApiError::bad_request)?;

    let assistant = state.assistant.clone();
    // enable() forks a PTY and may reap a previous child — keep it off the runtime.
    let result = tokio::task::spawn_blocking(move || assistant.enable())
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
        .map_err(ApiError::bad_request)?;

    let mut out = json!({"status": "success", "assistant": state.assistant.status()});
    if let (Some(obj), Some(extra)) = (out.as_object_mut(), result.as_object()) {
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
    }
    Ok(Json(out))
}

/// Send a task into the live agent session.
///
/// Set `wait_seconds > 0` to get terminal feedback in the same response, which
/// is what makes this usable from cron or a phone shortcut with no UI at all.
async fn agent_message(
    State(state): State<AppState>,
    Json(req): Json<AgentMessageRequest>,
) -> ApiResult {
    if req.text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(ApiError::bad_request(format!(
            "text exceeds {MAX_MESSAGE_CHARS} characters"
        )));
    }

    if !state.manager.running() && req.ensure_session {
        let assistant = state.assistant.clone();
        tokio::task::spawn_blocking(move || assistant.enable())
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?
            .map_err(|e| ApiError::bad_request(format!("Could not start assistant: {e}")))?;
    }

    if !state.manager.running() {
        return Err(ApiError::conflict(
            "No active agent session. POST /api/agent/start with one of the providers \
             from /api/providers.",
        ));
    }

    let since = state.manager.current_seq();
    let (body, submit_key) = encode_agent_message(&req.text, req.submit, req.multiline);

    state.manager.write(&body).map_err(ApiError::conflict)?;
    if !submit_key.is_empty() {
        tokio::time::sleep(SUBMIT_DELAY).await;
        state
            .manager
            .write(&submit_key)
            .map_err(ApiError::conflict)?;
    }

    let mut result = json!({
        "status": "success",
        "bytes": body.len() + submit_key.len(),
        "provider": state.manager.provider(),
        "generation": state.manager.generation(),
        "seq_before": since,
    });

    if req.wait_seconds > 0.0 {
        let manager = state.manager.clone();
        let timeout = Duration::from_secs_f64(req.wait_seconds.clamp(0.0, 600.0));
        let idle = Duration::from_secs_f64(req.idle_seconds.clamp(0.2, 60.0));
        let plain = req.plain;
        // Block in a worker thread so the runtime stays free for WS reads.
        let output =
            tokio::task::spawn_blocking(move || manager.wait_output(since, timeout, idle, plain))
                .await
                .map_err(|e| ApiError::bad_request(e.to_string()))?;

        let obj = result.as_object_mut().unwrap();
        obj.insert("text".into(), json!(output.text));
        obj.insert("seq".into(), json!(output.seq));
        obj.insert("output".into(), serde_json::to_value(&output).unwrap());
    }

    Ok(Json(result))
}

async fn output_for(state: &AppState, q: &OutputQuery) -> crate::pty::Output {
    let manager = state.manager.clone();
    let (since, plain, wait, idle, full) = (q.since, q.plain, q.wait, q.idle, q.full);

    tokio::task::spawn_blocking(move || {
        if full {
            manager.get_output_full(plain, false)
        } else if wait > 0.0 {
            manager.wait_output(
                since,
                Duration::from_secs_f64(wait.clamp(0.0, 600.0)),
                Duration::from_secs_f64(idle.clamp(0.2, 60.0)),
                plain,
            )
        } else {
            manager.get_output(since, plain, false)
        }
    })
    .await
    .expect("output task panicked")
}

/// Fetch terminal output without the frontend (cleaned by default).
async fn agent_output(State(state): State<AppState>, Query(q): Query<OutputQuery>) -> Json<Value> {
    Json(serde_json::to_value(output_for(&state, &q).await).unwrap())
}

/// Plain-text body — easiest for scripts.
async fn agent_output_text(
    State(state): State<AppState>,
    Query(q): Query<OutputQuery>,
) -> Response {
    let out = output_for(&state, &q).await;
    let body = if out.text.is_empty() {
        String::new()
    } else {
        format!("{}\n", out.text)
    };
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

/// One transcript row as an SSE event.
fn sse(event: &crate::recorder::Event) -> String {
    let id = format!("id: {}\n", event.seq);
    match event.kind {
        // Leading blank data line = the paragraph break that makes `curl -N` readable.
        EventKind::User => format!("{id}data:\ndata: ❯ {}\n\n", event.text),
        EventKind::Text => format!("{id}data: {}\n\n", event.text),
        kind => format!("{id}event: {}\ndata: {}\n\n", kind.as_str(), event.text),
    }
}

/// Live tail of the conversation (SSE) — one transcript line per event.
///
/// The CLIs repaint a fixed viewport, so this streams the *transcript* rather
/// than the screen: banners, box borders, spinners and status bars are dropped,
/// and a repaint never re-sends a line you already have.
///
/// Rows come from the single process-wide recorder rather than being derived
/// here, so several readers — this endpoint, `/reader`, the chat bridge and the
/// day store — always see exactly the same conversation.
async fn agent_output_stream(
    State(state): State<AppState>,
    Query(q): Query<StreamQuery>,
) -> Response {
    if q.chrome {
        return stream_raw_screens(state, q).await;
    }

    struct Live {
        events: tokio::sync::broadcast::Receiver<crate::recorder::Event>,
        backlog: std::collections::VecDeque<crate::recorder::Event>,
        started: bool,
    }

    let init = Live {
        // Subscribed *before* the backlog is taken, so a row arriving between
        // the two is delivered late rather than lost.
        events: state.recorder.subscribe(),
        backlog: state.recorder.since(q.since).into(),
        started: false,
    };

    let stream = futures_util::stream::unfold(init, |mut st| async move {
        if !st.started {
            st.started = true;
            return Some((
                Ok::<_, std::convert::Infallible>(
                    "retry: 2000\n\n: wired live tail\n\n".to_string(),
                ),
                st,
            ));
        }
        if let Some(event) = st.backlog.pop_front() {
            return Some((Ok(sse(&event)), st));
        }
        loop {
            match tokio::time::timeout(Duration::from_secs(15), st.events.recv()).await {
                Ok(Ok(event)) => return Some((Ok(sse(&event)), st)),
                // A slow reader missed rows; the live view is the tail, not an
                // archive, and `/api/history` has the whole thing.
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return None,
                // Enough to hold proxies open.
                Err(_) => return Some((Ok(": keep-alive\n\n".to_string()), st)),
            }
        }
    });

    sse_response(Body::from_stream(stream))
}

fn sse_response(body: Body) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .unwrap()
}

/// `?chrome=1` — the unfiltered screen, for working out why a filter dropped
/// something. Derived per client because nothing else wants it.
async fn stream_raw_screens(state: AppState, q: StreamQuery) -> Response {
    struct StreamState {
        manager: PtyManager,
        tail: TranscriptTail,
        cursor: u64,
        generation: Option<u64>,
        announced_down: bool,
        quiet: u32,
        chrome: bool,
        started: bool,
    }

    let init = StreamState {
        manager: state.manager.clone(),
        tail: TranscriptTail::default(),
        cursor: q.since,
        generation: None,
        announced_down: false,
        quiet: 0,
        chrome: q.chrome,
        started: false,
    };

    let stream = futures_util::stream::unfold(init, |mut st| async move {
        loop {
            if !st.started {
                st.started = true;
                return Some((
                    Ok::<_, std::convert::Infallible>(
                        "retry: 2000\n\n: wired live tail\n\n".to_string(),
                    ),
                    st,
                ));
            }

            // Immediate first paint on the first pass, then long-poll.
            let manager = st.manager.clone();
            let cursor = st.cursor;
            let running_now = manager.running();
            let snap = if !running_now {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let m = manager.clone();
                tokio::task::spawn_blocking(move || m.get_output_full(true, false))
                    .await
                    .ok()?
            } else {
                let m = manager.clone();
                tokio::task::spawn_blocking(move || {
                    m.wait_output(
                        cursor,
                        Duration::from_secs(3),
                        Duration::from_millis(400),
                        true,
                    )
                })
                .await
                .ok()?
            };

            let mut chunk = String::new();

            let fresh = snap.seq > st.cursor;
            st.cursor = snap.seq;

            if st.generation != Some(snap.generation) {
                if st.generation.is_some() {
                    st.tail.reset();
                    chunk.push_str(&format!(
                        "event: session\ndata: new session (generation {})\n\n",
                        snap.generation
                    ));
                }
                st.generation = Some(snap.generation);
            }

            for row in st.tail.update(&snap.text, fresh, st.chrome) {
                chunk.push_str(&format!(
                    "event: {}\ndata: {}\n\n",
                    row.kind.as_str(),
                    row.text
                ));
            }

            if snap.running {
                st.announced_down = false;
            } else if !st.announced_down {
                st.announced_down = true;
                chunk.push_str("event: status\ndata: no session — POST /api/agent/start\n\n");
            }

            if fresh {
                st.quiet = 0;
            } else {
                st.quiet += 1;
                if st.quiet % 5 == 0 {
                    // ~15s — enough to hold proxies open
                    chunk.push_str(": keep-alive\n\n");
                }
            }

            if !chunk.is_empty() {
                return Some((Ok(chunk), st));
            }
        }
    });

    sse_response(Body::from_stream(stream))
}

async fn agent_key(State(state): State<AppState>, Json(req): Json<KeyRequest>) -> ApiResult {
    write_payload(
        &state,
        WriteRequest {
            key: Some(req.key),
            ..Default::default()
        },
    )
}

/// Answer the approval the agent is blocked on.
///
/// The endpoint the buttons call — in the app, in `/reader`, and on the inline
/// keyboard in Telegram. Before this existed, every one of those surfaces
/// rendered the question and then told the reader to reach for curl.
async fn agent_approve(
    State(state): State<AppState>,
    Json(req): Json<ApproveRequest>,
) -> ApiResult {
    agent_io::answer(&state.manager, req.allow)
        .await
        .map_err(ApiError::conflict)?;
    state.recorder.clear_pending();
    Ok(Json(json!({
        "status": "success",
        "answered": if req.allow { "allow" } else { "deny" },
    })))
}

/// Stop keep-alive and kill the agent session.
async fn agent_stop(State(state): State<AppState>) -> ApiResult {
    let assistant = state.assistant.clone();
    tokio::task::spawn_blocking(move || assistant.disable(true))
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(
        json!({"status": "success", "assistant": state.assistant.status()}),
    ))
}

// ── settings ────────────────────────────────────────────────────────────

fn settings_snapshot(state: &AppState) -> Value {
    let stored = settings_store::get();
    json!({
        "assistant": crate::providers::default_assistant_provider(),
        "folder": crate::providers::agent_cwd().display().to_string(),
        "always_on": state.assistant.config().keep_alive,
        "auto_start": state.assistant.config().auto_start,
        "ask_before_acting": !auto_approve_enabled(),
        "start_at_login": stored.start_at_login.unwrap_or(false),
        "notifications": stored.notifications.unwrap_or(true),
        "onboarded": stored.onboarded,
        "port": state.settings.port,
        "auth_required": state.settings.auth_required(),
        "secrets": settings_store::secret_backing().as_str(),
        "config_file": crate::paths::settings_file().display().to_string(),
        "log_file": diagnostics::log_file().display().to_string(),
        // Anything set in the environment outranks this screen, and a setting
        // that silently does nothing is worse than one that is greyed out.
        "env_overrides": env_overrides(),
    })
}

/// Which settings the environment has taken out of the user's hands.
fn env_overrides() -> Vec<&'static str> {
    [
        ("assistant", "WIRED_ASSISTANT_PROVIDER"),
        ("folder", "WIRED_AGENT_CWD"),
        ("always_on", "WIRED_ASSISTANT_KEEP_ALIVE"),
        ("auto_start", "WIRED_ASSISTANT_AUTO_START"),
        ("ask_before_acting", "WIRED_AGENT_AUTO_APPROVE"),
        ("port", "WIRED_PORT"),
        ("auth_token", "WIRED_AUTH_TOKEN"),
    ]
    .into_iter()
    .filter(|(_, env)| std::env::var_os(env).is_some())
    .map(|(field, _)| field)
    .collect()
}

async fn read_settings(State(state): State<AppState>) -> Json<Value> {
    Json(settings_snapshot(&state))
}

/// Apply whatever moved, and say plainly what needs a restart.
async fn write_settings(
    State(state): State<AppState>,
    Json(req): Json<SettingsRequest>,
) -> ApiResult {
    let mut restart_required = false;

    if let Some(folder) = &req.folder {
        setup::choose_folder(folder).map_err(ApiError::bad_request)?;
    }
    if let Some(token) = &req.auth_token {
        settings_store::set_auth_token(token).map_err(ApiError::bad_request)?;
        restart_required = true;
    }
    if req.port.is_some_and(|p| p != state.settings.port) {
        restart_required = true;
    }

    settings_store::update(|s| {
        if let Some(assistant) = &req.assistant {
            s.assistant = Some(assistant.trim().to_ascii_lowercase());
        }
        if let Some(value) = req.always_on {
            s.always_on = Some(value);
        }
        if let Some(value) = req.auto_start {
            s.auto_start = Some(value);
        }
        if let Some(value) = req.ask_before_acting {
            s.ask_before_acting = Some(value);
        }
        if let Some(value) = req.start_at_login {
            s.start_at_login = Some(value);
        }
        if let Some(value) = req.notifications {
            s.notifications = Some(value);
        }
        if let Some(value) = req.port {
            s.port = Some(value);
        }
        if let Some(value) = req.onboarded {
            s.onboarded = value;
        }
    })
    .map_err(ApiError::bad_request)?;

    // Push what the running supervisor can adopt without a restart.
    state
        .assistant
        .configure(
            req.assistant.as_deref(),
            req.always_on,
            req.auto_start,
            None,
            None,
        )
        .map_err(ApiError::bad_request)?;

    // Two settings cannot reach a session that is already up: the approval flags
    // are command-line arguments, and the provider is a different binary
    // altogether. The supervisor will not notice either on its own — it leaves a
    // running session alone — so the caller has to restart it.
    let provider_pending = req
        .assistant
        .as_deref()
        .map(|p| p.trim().to_ascii_lowercase())
        .is_some_and(|p| state.manager.provider().as_deref() != Some(p.as_str()));
    let restart_assistant =
        (provider_pending || req.ask_before_acting.is_some()) && state.manager.running();

    Ok(Json(json!({
        "status": "success",
        "settings": settings_snapshot(&state),
        "restart_required": restart_required,
        "restart_assistant": restart_assistant,
    })))
}

// ── setup wizard ────────────────────────────────────────────────────────

async fn setup_state(State(state): State<AppState>) -> Json<Value> {
    Json(setup::state(&state.installer))
}

async fn setup_install(
    State(state): State<AppState>,
    Json(req): Json<ProviderRequest>,
) -> ApiResult {
    state
        .installer
        .start(req.provider.trim())
        .map_err(ApiError::bad_request)?;
    Ok(Json(
        json!({"status": "success", "install": state.installer.status()}),
    ))
}

/// Run the CLI's own sign-in, in the terminal panel.
///
/// This is the step the README called unscriptable. It still is — but it does
/// not need scripting, because the app already owns a PTY and can simply let
/// the user answer the CLI's questions in it.
async fn setup_login(State(state): State<AppState>, Json(req): Json<ProviderRequest>) -> ApiResult {
    let provider = req.provider.trim().to_ascii_lowercase();
    let cmd = crate::providers::resolve_login_cmd(&provider)
        .ok_or_else(|| ApiError::bad_request(format!("{provider} isn't installed yet.")))?;

    let cfg = state.assistant.config();
    let manager = state.manager.clone();
    let label = provider.clone();
    tokio::task::spawn_blocking(move || manager.start_command(&label, cmd, cfg.cols, cfg.rows))
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
        .map_err(ApiError::bad_request)?;

    Ok(Json(json!({
        "status": "success",
        "provider": provider,
        "message": "Follow the sign-in in the terminal below.",
    })))
}

async fn setup_folder(State(state): State<AppState>, Json(req): Json<FolderRequest>) -> ApiResult {
    let path = setup::choose_folder(&req.folder).map_err(ApiError::bad_request)?;
    Ok(Json(json!({
        "status": "success",
        "folder": path.display().to_string(),
        "restart_assistant": state.manager.running(),
    })))
}

// ── chat bridge ─────────────────────────────────────────────────────────

async fn gateway_status(State(state): State<AppState>) -> Json<Value> {
    Json(state.gateway.status())
}

async fn gateway_configure(
    State(state): State<AppState>,
    Json(req): Json<GatewayRequest>,
) -> ApiResult {
    if let Some(token) = &req.bot_token {
        settings_store::set_telegram_bot_token(token).map_err(ApiError::bad_request)?;
    }
    if let Some(enabled) = req.enabled {
        settings_store::update(|s| s.telegram.enabled = enabled).map_err(ApiError::bad_request)?;
    }
    if let Some(muted) = req.muted {
        state.gateway.set_muted(muted);
    }
    // Reconnects with the new configuration, so pasting a token is the last
    // thing the user has to do rather than the first of two.
    state.gateway.restart(state.hub());
    Ok(Json(
        json!({"status": "success", "gateway": state.gateway.status()}),
    ))
}

/// Throw the bot token away, along with everything paired to it, so a new one
/// can be pasted into a clean setup screen.
async fn gateway_reset(State(state): State<AppState>) -> ApiResult {
    state.gateway.reset().map_err(ApiError::bad_request)?;
    Ok(Json(
        json!({"status": "success", "gateway": state.gateway.status()}),
    ))
}

async fn gateway_pairings(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"pending": state.gateway.pending_pairings()}))
}

async fn gateway_approve(
    State(state): State<AppState>,
    Json(req): Json<PairingRequest>,
) -> ApiResult {
    let paired = state
        .gateway
        .approve_pairing(&req.code)
        .map_err(ApiError::bad_request)?;
    Ok(Json(json!({"status": "success", "paired": paired})))
}

async fn gateway_deny(State(state): State<AppState>, Json(req): Json<PairingRequest>) -> ApiResult {
    let denied = state
        .gateway
        .deny_pairing(&req.code)
        .map_err(ApiError::bad_request)?;
    Ok(Json(json!({"status": "success", "denied": denied})))
}

async fn gateway_unpair(
    State(state): State<AppState>,
    Json(req): Json<UnpairRequest>,
) -> ApiResult {
    state
        .gateway
        .unpair(req.chat)
        .map_err(ApiError::bad_request)?;
    Ok(Json(
        json!({"status": "success", "gateway": state.gateway.status()}),
    ))
}

// ── schedules ───────────────────────────────────────────────────────────

fn schedule_view(schedule: &Schedule) -> Value {
    let mut value = serde_json::to_value(schedule).unwrap_or_else(|_| json!({}));
    value["when_readable"] = json!(schedule
        .trigger()
        .map(|t| t.describe())
        .unwrap_or_else(|e| e));
    value["next_readable"] = json!(schedule.next_run.map(crate::scheduler::describe_when));
    value
}

async fn list_schedules(State(state): State<AppState>) -> Json<Value> {
    let schedules: Vec<Value> = state.scheduler.list().iter().map(schedule_view).collect();
    Json(json!({"schedules": schedules, "running": state.scheduler.active()}))
}

async fn save_schedule(State(state): State<AppState>, Json(schedule): Json<Schedule>) -> ApiResult {
    let saved = state
        .scheduler
        .upsert(schedule)
        .map_err(ApiError::bad_request)?;
    Ok(Json(
        json!({"status": "success", "schedule": schedule_view(&saved)}),
    ))
}

async fn delete_schedule(
    State(state): State<AppState>,
    Json(req): Json<ScheduleIdRequest>,
) -> ApiResult {
    state
        .scheduler
        .remove(&req.id)
        .map_err(ApiError::bad_request)?;
    Ok(Json(json!({"status": "success"})))
}

/// Fire one now. A schedule you cannot test is a schedule you do not trust.
async fn run_schedule(
    State(state): State<AppState>,
    Json(req): Json<ScheduleIdRequest>,
) -> ApiResult {
    let result = state
        .scheduler
        .run_once(&state.hub(), &req.id)
        .await
        .map_err(ApiError::conflict)?;
    Ok(Json(json!({"status": "success", "result": result})))
}

// ── history ─────────────────────────────────────────────────────────────

fn store_or_error(state: &AppState) -> Result<&crate::recorder::DayStore, ApiError> {
    state.recorder.store().ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            "History isn't being saved on this machine.".to_string(),
        )
    })
}

async fn history_days(State(state): State<AppState>) -> ApiResult {
    let store = store_or_error(&state)?;
    Ok(Json(json!({"days": store.days()})))
}

async fn history_day(State(state): State<AppState>, Query(q): Query<HistoryQuery>) -> ApiResult {
    let store = store_or_error(&state)?;
    let day = q
        .day
        .unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
    let events = store.read_day(&day, q.limit.clamp(1, 20_000));
    Ok(Json(json!({"day": day, "events": events})))
}

async fn history_search(State(state): State<AppState>, Query(q): Query<HistoryQuery>) -> ApiResult {
    let store = store_or_error(&state)?;
    let query = q.query.unwrap_or_default();
    let hits: Vec<Value> = store
        .search(&query, q.limit.clamp(1, 500))
        .into_iter()
        .map(|(day, event)| json!({"day": day, "event": event}))
        .collect();
    Ok(Json(json!({"query": query, "hits": hits})))
}

// ── diagnostics ─────────────────────────────────────────────────────────

async fn diagnostics_report(State(state): State<AppState>) -> Json<Value> {
    let gateway = state.gateway.status();
    Json(diagnostics::report(
        &state.settings.host,
        state.settings.port,
        &gateway,
        &state.assistant.status(),
    ))
}

/// Remove everything Wired put on this machine.
///
/// Dragging the app to the Trash leaves the settings, the login item and the
/// transcript behind; this is the button that does not. It never touches the
/// agent CLIs, their credentials, or the working folder — those were not ours.
async fn diagnostics_reset(State(state): State<AppState>, Json(req): Json<Value>) -> ApiResult {
    if req["confirm"] != json!("erase") {
        return Err(ApiError::bad_request(
            "Send {\"confirm\": \"erase\"} to delete Wired's settings and history.",
        ));
    }
    state.gateway.stop();
    let removed = diagnostics::forget_everything().map_err(ApiError::bad_request)?;
    Ok(Json(json!({"status": "success", "removed": removed})))
}

// ── low-level PTY API ───────────────────────────────────────────────────

fn resolve_write_payload(req: &WriteRequest) -> Result<Vec<u8>, String> {
    if let Some(key) = &req.key {
        return resolve_key(key);
    }
    if let Some(hex) = &req.hex {
        let h: String = hex.replace(' ', "").replace("0x", "");
        if !h.len().is_multiple_of(2) {
            return Err("hex must have an even number of digits".into());
        }
        return (0..h.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&h[i..i + 2], 16).map_err(|e| format!("invalid hex: {e}")))
            .collect();
    }
    let text = req
        .data
        .as_ref()
        .or(req.command.as_ref())
        .ok_or("Provide one of: key, data/command, or hex")?;

    if req.interpret_escapes && text.contains('\\') {
        Ok(decode_escapes(text))
    } else {
        Ok(text.as_bytes().to_vec())
    }
}

fn write_payload(state: &AppState, req: WriteRequest) -> ApiResult {
    if !state.manager.running() {
        return Err(ApiError::conflict("No active PTY session"));
    }
    let payload = resolve_write_payload(&req).map_err(ApiError::bad_request)?;
    if payload.is_empty() {
        return Err(ApiError::bad_request("Empty payload"));
    }
    state.manager.write(&payload).map_err(ApiError::conflict)?;
    Ok(Json(json!({"status": "success", "bytes": payload.len()})))
}

async fn start_pty(State(state): State<AppState>, Json(req): Json<StartRequest>) -> ApiResult {
    let manager = state.manager.clone();
    let (provider, cols, rows) = (req.provider.clone(), req.cols, req.rows);
    tokio::task::spawn_blocking(move || manager.start(&provider, cols, rows))
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?
        .map_err(ApiError::bad_request)?;

    Ok(Json(json!({
        "status": "success",
        "generation": state.manager.generation(),
        "provider": state.manager.provider(),
        "running": state.manager.running(),
    })))
}

async fn write_pty(State(state): State<AppState>, Json(req): Json<WriteRequest>) -> ApiResult {
    write_payload(&state, req)
}

async fn send_key(State(state): State<AppState>, Json(req): Json<KeyRequest>) -> ApiResult {
    write_payload(
        &state,
        WriteRequest {
            key: Some(req.key),
            ..Default::default()
        },
    )
}

async fn resize_pty(State(state): State<AppState>, Json(req): Json<ResizeRequest>) -> ApiResult {
    if !state.manager.running() {
        return Err(ApiError::conflict("No active PTY session"));
    }
    state.manager.set_size(req.cols, req.rows);
    Ok(Json(json!({"status": "success"})))
}

async fn kill_pty(State(state): State<AppState>) -> ApiResult {
    // A manual kill does not disable the supervisor — keep-alive may restart it.
    let manager = state.manager.clone();
    tokio::task::spawn_blocking(move || manager.kill())
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(json!({"status": "success"})))
}

async fn pty_status(State(state): State<AppState>) -> Json<Value> {
    let snap = state.manager.snapshot();
    Json(json!({
        "running": snap.running,
        "provider": snap.provider,
        "generation": snap.generation,
    }))
}

/// Inject text + Enter into the live PTY (the shell-oriented sibling of agent/message).
async fn send_command(State(state): State<AppState>, Json(req): Json<CommandRequest>) -> ApiResult {
    if !state.manager.running() {
        return Err(ApiError::conflict("No active PTY session"));
    }
    let mut payload = if req.command.contains('\\') {
        decode_escapes(&req.command)
    } else {
        req.command.as_bytes().to_vec()
    };
    if req.submit.unwrap_or(true) && !matches!(payload.last(), Some(b'\r') | Some(b'\n')) {
        payload.push(b'\r');
    }
    state.manager.write(&payload).map_err(ApiError::conflict)?;
    Ok(Json(json!({
        "status": "success",
        "message": format!("Injected {} byte(s)", payload.len()),
    })))
}

// ── live terminal WebSocket ─────────────────────────────────────────────

async fn websocket_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Checked before the upgrade is accepted: WebSockets bypass CORS entirely,
    // so an unguarded upgrade would hand any open tab a keyboard on this PTY.
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if !origin_valid(&state.settings, origin) {
        tracing::warn!(?origin, "rejected websocket from origin");
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }

    if state.settings.auth_required() {
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        let presented = presented_token(authorization, params.get("token").map(|s| s.as_str()));
        if !token_valid(&state.settings, &presented) {
            return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }
    }

    ws.on_upgrade(move |socket| websocket_session(socket, state))
}

async fn websocket_session(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.manager.subscribe();

    let snap = state.manager.snapshot();
    let init = json!({
        "type": "init",
        "running": snap.running,
        "provider": snap.provider,
        "generation": snap.generation,
    });
    if sender
        .send(Message::Text(init.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    // Replay the current session so a reconnect is not a blank screen.
    for (gen, raw) in snap.recent {
        if gen != snap.generation {
            continue;
        }
        let msg = json!({
            "type": "data",
            "generation": gen,
            "chunk": base64::engine::general_purpose::STANDARD.encode(&raw),
        });
        if sender
            .send(Message::Text(msg.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    }

    let manager = state.manager.clone();
    let mut send_task = tokio::spawn(async move {
        while let Ok(msg) = events.recv().await {
            if sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            if let Message::Text(text) = msg {
                // Keystrokes into a dead session are not worth closing over.
                let _ = manager.write(text.as_bytes());
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
}
