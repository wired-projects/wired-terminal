//! Request authentication and origin checks.
//!
//! Two distinct jobs, easy to conflate:
//!
//!   * **Token** — proves the caller is you. Optional (unset = open), enforced
//!     on every `/api` route and the WebSocket once `WIRED_AUTH_TOKEN` is set.
//!   * **Origin** — proves a request is not a *page* the browser loaded from
//!     somewhere else. Needed even on loopback: `http://localhost:8000` is
//!     reachable from any tab, and CORS does not stop the request being *sent*,
//!     only the reply being read — which is plenty when the request itself
//!     starts a shell. WebSockets get no CORS at all, so that check is manual.

use subtle::ConstantTimeEq;

use crate::config::Settings;

/// Bearer header first; `?token=` is the fallback EventSource forces on us.
///
/// `EventSource` and the WebSocket constructor cannot set headers, so the
/// reader UI and `curl -N` have no way to send `Authorization`.
pub fn presented_token(authorization: Option<&str>, query_token: Option<&str>) -> String {
    if let Some(auth) = authorization {
        let (scheme, value) = auth.split_once(' ').unwrap_or(("", ""));
        if scheme.eq_ignore_ascii_case("bearer") && !value.trim().is_empty() {
            return value.trim().to_string();
        }
    }
    query_token.unwrap_or("").trim().to_string()
}

pub fn token_valid(settings: &Settings, presented: &str) -> bool {
    if !settings.auth_required() {
        return true;
    }
    // Constant-time: a plain == leaks the matching prefix through timing.
    !presented.is_empty()
        && presented
            .as_bytes()
            .ct_eq(settings.auth_token.as_bytes())
            .into()
}

/// A missing Origin is a non-browser client (curl, a script) — allowed.
///
/// Browsers always send one on cross-origin requests, so absence cannot be
/// forged by the page we are defending against.
pub fn origin_valid(settings: &Settings, origin: Option<&str>) -> bool {
    match origin {
        None | Some("") => true,
        Some(o) => settings.origin_allowed(o),
    }
}
