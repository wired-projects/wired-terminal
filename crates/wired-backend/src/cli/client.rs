//! Talking to the API, including when it is on another machine.
//!
//! A Wired server binds loopback by default, and that is the recommended
//! setup — so "remote" here does not mean an open port. It means an SSH tunnel
//! held open for exactly as long as the command runs, which is the same thing
//! `docs/server.md` tells you to do by hand.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::profile::Target;

pub type Result<T> = std::result::Result<T, String>;

/// How long to wait for ssh to authenticate and bind the forward. Generous
/// because the user may be typing a key passphrase into the prompt ssh shows.
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(45);

/// An `ssh -N -L` child, killed when this value goes out of scope.
struct Tunnel {
    child: Child,
    port: u16,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Ask the OS for a port nobody is using, then let go of it.
///
/// Racy in principle; in practice the window is microseconds and ssh tells us
/// immediately if it lost the race, because of `ExitOnForwardFailure`.
fn free_port() -> Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|e| format!("could not reserve a local port: {e}"))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|e| format!("could not reserve a local port: {e}"))
}

impl Tunnel {
    fn open(host: &str, ssh_port: Option<u16>, remote_port: u16) -> Result<Tunnel> {
        let local = free_port()?;
        let mut cmd = Command::new("ssh");
        cmd.arg("-N")
            // Fail loudly instead of connecting and silently forwarding nothing.
            .arg("-o")
            .arg("ExitOnForwardFailure=yes")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-L")
            .arg(format!("127.0.0.1:{local}:127.0.0.1:{remote_port}"));
        if let Some(port) = ssh_port {
            cmd.arg("-p").arg(port.to_string());
        }
        // stdin and stderr stay attached: a passphrase prompt or a host-key
        // warning has to reach the person running the command.
        let child = cmd
            .arg(host)
            .stdout(Stdio::null())
            .spawn()
            .map_err(|e| match e.kind() {
                ErrorKind::NotFound => "ssh is not installed — remotes need it".to_string(),
                _ => format!("could not start ssh: {e}"),
            })?;

        let mut tunnel = Tunnel { child, port: local };
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, local));
        let deadline = Instant::now() + TUNNEL_TIMEOUT;
        loop {
            if TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok() {
                return Ok(tunnel);
            }
            // ssh giving up (bad host, refused key) is not worth waiting out.
            if let Ok(Some(status)) = tunnel.child.try_wait() {
                return Err(format!("ssh to {host} exited ({status}) — tunnel not open"));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out opening an SSH tunnel to {host} after {}s",
                    TUNNEL_TIMEOUT.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

pub struct Api {
    base: String,
    token: String,
    http: reqwest::Client,
    /// Held, not read: dropping it closes the tunnel.
    _tunnel: Option<Tunnel>,
}

impl Api {
    pub async fn connect(target: &Target) -> Result<Api> {
        let (base, tunnel) = match target {
            Target::Local { base, .. } | Target::Url { base, .. } => (base.clone(), None),
            Target::Remote { remote, .. } => {
                let tunnel = Tunnel::open(&remote.host, remote.ssh_port, remote.port)?;
                (format!("http://127.0.0.1:{}", tunnel.port), Some(tunnel))
            }
        };

        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // No overall timeout: `ask --wait 300` and `watch` are both meant
            // to hold the connection open.
            .build()
            .map_err(|e| format!("could not build an HTTP client: {e}"))?;

        Ok(Api {
            base,
            token: target.token().to_string(),
            http,
            _tunnel: tunnel,
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let builder = self.http.request(method, format!("{}{path}", self.base));
        if self.token.is_empty() {
            builder
        } else {
            builder.bearer_auth(&self.token)
        }
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let response = request.send().await.map_err(|e| self.explain(e))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        // The API answers errors as {"detail": "..."}; anything else we show raw.
        let body = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["detail"].as_str().map(str::to_string))
            .unwrap_or_else(|| body.trim().to_string());

        Err(match status.as_u16() {
            401 | 403 if detail.is_empty() => {
                "the API rejected the token — pass --token, or check WIRED_AUTH_TOKEN".to_string()
            }
            401 | 403 => format!("{detail} (pass --token if this API needs one)"),
            _ if detail.is_empty() => format!("the API returned {status}"),
            _ => detail,
        })
    }

    /// Turn a transport failure into something that says what to do next.
    fn explain(&self, err: reqwest::Error) -> String {
        if err.is_connect() {
            format!(
                "no API answering at {} — is the service running? try `wired start`",
                self.base
            )
        } else if err.is_timeout() {
            format!("timed out talking to {}", self.base)
        } else {
            format!("{}: {err}", self.base)
        }
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        let response = self.send(self.request(reqwest::Method::GET, path)).await?;
        response
            .json()
            .await
            .map_err(|e| format!("{path} returned something that is not JSON: {e}"))
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let response = self
            .send(self.request(reqwest::Method::POST, path).json(&body))
            .await?;
        response
            .json()
            .await
            .map_err(|e| format!("{path} returned something that is not JSON: {e}"))
    }

    /// `/healthz` without the error handling — this is a probe, not a request.
    pub async fn alive(&self) -> bool {
        matches!(
            self.request(reqwest::Method::GET, "/healthz").send().await,
            Ok(r) if r.status().is_success()
        )
    }

    pub async fn wait_alive(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.alive().await {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Read `/api/agent/output/stream` for as long as the caller wants it,
    /// handing back one SSE event at a time.
    pub async fn stream_sse(&self, path: &str, mut on_event: impl FnMut(SseEvent)) -> Result<()> {
        let mut response = self.send(self.request(reqwest::Method::GET, path)).await?;
        let mut buffer = String::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| self.explain(e))? {
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            // Events are terminated by a blank line; anything after the last
            // one is a partial event and stays in the buffer.
            while let Some(end) = buffer.find("\n\n") {
                let raw: String = buffer.drain(..end + 2).collect();
                if let Some(event) = SseEvent::parse(&raw) {
                    on_event(event);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// `text`, `user`, `prompt`, `notice`, `session`, `status`, `system`.
    pub kind: String,
    pub data: String,
}

impl SseEvent {
    /// Only the three fields this API uses; comments and retries are ignored.
    fn parse(raw: &str) -> Option<SseEvent> {
        let mut event = SseEvent {
            kind: "text".into(),
            data: String::new(),
        };
        let mut lines = 0;
        for line in raw.lines() {
            if let Some(value) = line.strip_prefix("event:") {
                event.kind = value.trim().to_string();
            } else if let Some(value) = line.strip_prefix("data:") {
                if lines > 0 {
                    event.data.push('\n');
                }
                // Exactly one space after the colon is the field separator; any
                // further indentation is the agent's own.
                event
                    .data
                    .push_str(value.strip_prefix(' ').unwrap_or(value));
                lines += 1;
            }
        }
        (lines > 0).then_some(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_text_event_parses() {
        let event = SseEvent::parse("id: 12\ndata: hello\n\n").unwrap();
        assert_eq!(event.kind, "text");
        assert_eq!(event.data, "hello");
    }

    #[test]
    fn a_kinded_event_keeps_its_kind() {
        let event = SseEvent::parse("id: 3\nevent: prompt\ndata: Allow this?\n\n").unwrap();
        assert_eq!(event.kind, "prompt");
        assert_eq!(event.data, "Allow this?");
    }

    #[test]
    fn the_leading_blank_data_line_becomes_a_paragraph_break() {
        // The API emits `data:\ndata: ❯ ...` for a user turn.
        let event = SseEvent::parse("id: 9\ndata:\ndata: ❯ hello\n\n").unwrap();
        assert_eq!(event.data, "\n❯ hello");
    }

    #[test]
    fn indentation_past_the_separator_is_preserved() {
        let event = SseEvent::parse("data:     indented\n\n").unwrap();
        assert_eq!(event.data, "    indented");
    }

    #[test]
    fn an_event_with_no_data_is_not_an_event() {
        assert_eq!(SseEvent::parse(": keep-alive\n\n"), None);
    }

    #[test]
    fn free_ports_are_actually_free() {
        let port = free_port().unwrap();
        assert!(port > 0);
        // Reservable again, which is what makes it usable for ssh -L.
        assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok());
    }
}
