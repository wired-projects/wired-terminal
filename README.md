<div align="center">
  <img src="brand/app-icon-512.png" alt="" width="88" height="88">
  <h1>Wired Terminal</h1>
  <p><strong>Run Claude Code, Grok, Codex or Gemini as your 24/7 personal assistant.</strong></p>
</div>

They already have agent capabilities. Wired keeps their CLI process alive and
lets you send it tasks over HTTP — from a script, a cron job, or your phone.

```
  your scripts / cron        Telegram (outbound long poll)
           │                          │
           ▼                          ▼
    POST /api/agent/message      gateway + pairing
           │                          │
           └──────────┬───────────────┘
                      ▼
                axum + keep-alive + scheduler
                      │
     PTY: claude | grok | codex | gemini
                      │
        ┌─────────────┼─────────────┐
        ▼             ▼             ▼
   WebSocket      recorder      day store
   live terminal  SSE / chat    searchable history
```

> **Not a developer?** [**docs/getting-started.md**](docs/getting-started.md) is
> the same product with no shell in it: download, install, sign in, ask it
> something. Everything below is the source build and the API.

## Quick start

```bash
npm run install:all          # frontend deps (the backend needs none)
npm run start                # UI on :5173, API on :8000
```

One of the `claude`, `grok`, `codex` or `gemini` CLIs must be on your `PATH`.
If it is not installed or
not logged in, the app's setup wizard will do both for you — the sign-in runs in
the terminal panel, which is why it no longer has to be done by hand.

For always-on mode, which starts the agent with the API and restarts it if it
exits:

```bash
WIRED_ASSISTANT_PROVIDER=claude npm run start:24x7
```

Settings that the desktop app writes live in a `settings.json` beside the
platform's other application data, and **every `WIRED_*` environment variable
still wins over it** — see [`.env.example`](.env.example).

### Requirements

- [Rust](https://rustup.rs) 1.85+ — to build. The result runs on its own.
- Node 18+ — for the desktop UI and the agent CLIs, not for the backend
- One or more agent CLIs: [Claude Code](https://docs.anthropic.com/en/docs/claude-code),
  Grok, [Codex](https://developers.openai.com/codex/cli) or
  [Gemini](https://github.com/google-gemini/gemini-cli)

## The `wired` command

The server install puts a `wired` on your `PATH`. It is the same crate as the
backend — one more binary, no interpreter — and it drives the API and the
service so that neither `systemctl` nor a remembered `curl` is required:

```bash
wired status                              # service, agent, API and chat, one screen
wired ask "summarise my git status"       # send a task, print the reply
wired watch                               # live transcript, ctrl-c to detach
wired approve                             # answer what the agent is blocked on
wired logs -f
wired restart
wired doctor                              # the setup checks, with an exit code
```

It manages the machine it runs on, and any server you can SSH to:

```bash
wired remote add pilot ubuntu@203.0.113.10
wired --remote pilot status               # tunnels for this one command, then closes
wired --remote pilot logs -f
wired remote default pilot                # …and now --remote is implied
```

**A remote needs no open port.** The API is reached through an `ssh -L` tunnel
held open for exactly as long as the command runs — the same tunnel the section
below tells you to open by hand — and `start`/`stop`/`restart` run over `ssh`
directly. Saved servers live in `cli.json` beside `settings.json`, `0600`,
because a saved token is a password to a shell.

On a laptop there is no systemd, so `wired start` runs the backend as a
background process tracked by a pid file, and `wired logs` reads the log file
rather than the journal. Everything else is identical.

```bash
cargo install --path crates/wired-backend   # wired + wired-backend onto PATH
npm run wired -- status                     # or run it from the checkout
```

`wired --help` is the reference; `wired <command> --help` explains one of them.
Exit codes are `0` fine, `1` the command failed, `2` the thing it asked about is
unhealthy — which is what makes `wired doctor` usable from cron.

## Run it on a server

> **[docs/server.md](docs/server.md) is the full headless walkthrough** —
> install, signing the CLI in, and the whole Telegram pairing flow as
> copy-pasteable commands, since a server has none of the buttons the other
> docs describe. What follows is the summary.

On Ubuntu/Debian one script does the whole install — the Rust toolchain, Node,
the Claude Code CLI, a service account and a systemd unit:

```bash
curl -fsSL https://terminal.wired.dev/install.sh | sudo bash
```

That clones this repo to `/opt/wired-terminal/src` and runs the script below
for you. From a checkout it is the same script, by hand:

```bash
git clone https://github.com/wired-projects/wired-terminal.git
cd wired-terminal && sudo bash scripts/install-ubuntu.sh
```

It is headless: the backend only. A server does not need the desktop app, and
the readable transcript is already served at `/reader`.

```bash
sudo bash scripts/install-ubuntu.sh --host 0.0.0.0   # reachable + generated token
sudo bash scripts/install-ubuntu.sh --user wired     # run the agent as this account
sudo bash scripts/install-ubuntu.sh --binary ./wired-backend   # skip the build
sudo bash scripts/install-ubuntu.sh --uninstall      # remove service, files, env
```

| Path | What |
|------|------|
| `/opt/wired-terminal/bin/wired-backend` | the whole backend, one binary |
| `/opt/wired-terminal/bin/wired` | the CLI, symlinked to `/usr/local/bin/wired` |
| `/etc/wired-terminal/wired.env` | settings and the auth token (`0640`) |
| `/etc/systemd/system/wired-terminal.service` | keeps the API up across reboots |

Re-running the script upgrades in place. Then log the CLI in once — on a server
that part is still a shell command, because there is no window to run it in:

```bash
sudo -u wired -H claude          # or put ANTHROPIC_API_KEY in wired.env
wired restart
wired logs -f
```

The agent runs as that account with auto-approve on, so it can do anything the
account can. Keep the default loopback bind and reach it over SSH —

```bash
ssh -N -L 8000:127.0.0.1:8000 you@server     # or: wired --remote <name> status
```

— or, if you open the port, treat the token as a root password and put TLS and
a firewall in front of it.

**Or don't open anything.** Configure the Telegram bridge and the server dials
out instead: `POST /api/gateway/configure` with `{"bot_token": "…", "enabled":
true}`, message the bot, then approve the code it returns with
`POST /api/gateway/pairings/approve`. No port, no tunnel, no TLS to terminate.

## Security

**This API starts processes on your machine.** Anything that can reach it can
run commands as you, so the defaults are closed:

| Default | Why |
|---------|-----|
| Binds `127.0.0.1` | Not reachable from the network |
| Origin allow-list | A random web page cannot POST to your local port |
| No wildcard CORS with credentials | Removes the drive-by request path |
| WebSocket checks Origin **and** token before `accept()` | WebSockets bypass CORS entirely |
| Refuses to start if bound off-host without a token | Prevents the worst misconfiguration silently happening |

To expose it beyond loopback you must set a token:

```bash
export WIRED_AUTH_TOKEN=$(head -c 24 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=')
WIRED_HOST=0.0.0.0 npm run start:assistant
```

Then authenticate every call:

```bash
curl -H "Authorization: Bearer $WIRED_AUTH_TOKEN" http://host:8000/api/agent/status
```

`EventSource` and the WebSocket cannot set headers, so those accept
`?token=…` instead. The desktop app keeps its token in the OS keychain and hands
it to the window at runtime, so a packaged build can be given one after it was
compiled; `VITE_AUTH_TOKEN` remains the dev-server fallback.

> **Auto-approve, and who it defaults to.** On a server the CLIs launch with
> their permission prompts pre-answered (`--dangerously-skip-permissions` /
> `--always-approve`) — that is what makes unattended operation work, and it is
> unchanged. In the desktop app the default is the other way round: prompts are
> **on**, because there is a person there to answer them with a button, and
> "stop asking me" is their own choice in Settings. `WIRED_AGENT_AUTO_APPROVE`
> overrides both.
>
> An approval arrives on the transcript stream as a `prompt` event. Answer it
> with `POST /api/agent/approve {"allow": true|false}` — or with the buttons in
> the app, in `/reader`, or on the Telegram message.

> **Authorising a new device.** No longer "generate 24 random bytes and paste
> them somewhere". An unknown chat that messages the bot gets a one-time
> 8-character code from an alphabet with no `0`/`O`/`1`/`I`; the owner approves
> it in the app. One hour to use it, at most three pending, one request per
> sender per ten minutes, and approval locks for fifteen minutes after five
> wrong codes. Codes are never written to the log.

See [`.env.example`](.env.example) for every setting, and
[docs/troubleshooting.md](docs/troubleshooting.md) for what to do when one of
them is wrong.

## Command your assistant

```bash
# Start Claude (or grok, codex, gemini) with keep-alive
curl -sX POST http://127.0.0.1:8000/api/agent/start \
  -H 'Content-Type: application/json' \
  -d '{"provider":"claude","keep_alive":true}'

# Send a task and wait for terminal feedback in the same response
curl -sX POST http://127.0.0.1:8000/api/agent/message \
  -H 'Content-Type: application/json' \
  -d '{"text":"Summarize my git status","submit":true,"wait_seconds":45,"idle_seconds":2}' \
  | jq

# Or: send, then poll / tail
curl -s 'http://127.0.0.1:8000/api/agent/output/text?since=0&wait=30'

# Live tail of the conversation
curl -N http://127.0.0.1:8000/api/agent/output/stream
```

```
retry: 2000
: wired live tail

data:
data: ❯ Recite this chant: WIRED ONE / WIRED TWO / WIRED THREE

data: WIRED ONE

data: WIRED TWO

data: WIRED THREE

: keep-alive
```

The CLIs are full-screen TUIs that repaint a fixed viewport, so the tail streams
the *transcript*, not the screen: banner art, box borders, spinners
(`Sautéed for 1s`), status bars and the composer are dropped, and a repaint
never re-sends a line you already have. Named events carry the rest:

| Event | Meaning |
|-------|---------|
| *(unnamed)* | Transcript line. `❯ …` marks your turn |
| `prompt` | The agent is asking for approval — answer with `/api/agent/key` |
| `notice` | Standing warning from the CLI (sent once) |
| `status` | No session running |
| `session` | The CLI restarted (keep-alive); transcript starts over |

Add `?chrome=1` to stream the raw screen instead, for debugging.

### Reading it in a browser

The same feed, rendered — no build step, no frontend dev server:

```
open http://127.0.0.1:8000/reader
```

Turns, replies, tool activity, approval prompts and warnings each get their own
treatment; it follows the tail unless you scroll up. `raw` toggles the
unfiltered screen, and `?theme=light` / `?theme=dark` overrides the OS palette.
It is read-only — send with `/api/agent/message`, answer prompts with
`/api/agent/key`. The desktop app has the same view behind the
**Transcript** tab.

### More control

```bash
# Multi-line task
curl -sX POST http://127.0.0.1:8000/api/agent/message \
  -H 'Content-Type: application/json' \
  -d '{"text":"Line 1 of context\nLine 2 of the task","submit":true}'

# Soft newline / Enter only
curl -sX POST http://127.0.0.1:8000/api/agent/key \
  -H 'Content-Type: application/json' -d '{"key":"lf"}'

# Status / stop
curl -s http://127.0.0.1:8000/api/agent/status | jq
curl -sX POST http://127.0.0.1:8000/api/agent/stop
```

### On a schedule

There is a scheduler now, so this no longer has to be a crontab entry calling a
shell script. It takes sentences as well as cron:

```bash
curl -sX POST http://127.0.0.1:8000/api/schedules \
  -H 'Content-Type: application/json' \
  -d '{"name":"Morning briefing","when":"every morning at 8",
       "task":"List my top 3 priorities for today","quiet_when_nothing":true}'
```

`quiet_when_nothing` appends the `[SILENT]` instruction and drops the result if
that is all that comes back — a monitor that reports nothing sends nothing.
Results go to the chat bridge as one labelled message rather than a live stream.

The cron form still works, of course:

```bash
curl -sX POST http://127.0.0.1:8000/api/agent/message \
  -H 'Content-Type: application/json' \
  -d '{"text":"Check calendar and list top 3 priorities","ensure_session":true}'
```

## API surface

| Endpoint | Role |
|----------|------|
| `GET /healthz` | Liveness, unauthenticated |
| `POST /api/agent/start` | Start an agent CLI + optional keep-alive |
| `POST /api/agent/message` | **Main control** — send tasks |
| `POST /api/agent/key` | `enter`, `lf`, `ctrl+c`, … |
| `POST /api/agent/approve` | Answer an approval prompt — `{"allow": true}` |
| `POST /api/agent/stop` | Stop supervisor + kill CLI |
| `GET /api/agent/status` | Providers + session + supervisor |
| `POST /api/agent/configure` | Change provider / keep_alive |
| `GET /api/agent/output` | Readable screen (supports long-poll) |
| `GET /api/agent/output/text` | Same, as `text/plain` |
| `GET /api/agent/output/stream` | Live transcript tail (SSE) |
| `GET`/`POST /api/settings` | Persisted settings; env still wins |
| `GET /api/setup/state` | Node, providers, install progress, folder |
| `POST /api/setup/install` | Install an agent CLI (npm, user prefix) |
| `POST /api/setup/login` | Run the CLI's interactive sign-in in the PTY |
| `POST /api/setup/folder` | Set — and verify — the agent's working folder |
| `GET /api/gateway/status` | Chat bridge, pairings, last error |
| `POST /api/gateway/configure` | Bot token, on/off, mute |
| `POST /api/gateway/pairings/approve` | Let a chat in, by its 8-char code |
| `POST /api/gateway/reset` | Forget the bot token and every paired chat |
| `GET`/`POST /api/schedules` | List / create / update schedules |
| `POST /api/schedules/run` | Fire one now |
| `GET /api/history/days`, `/day`, `/search` | Persisted transcript |
| `GET /api/diagnostics` | Version, paths, chain checks, log tail |
| `GET /reader` | Readable live transcript in a browser |
| `WS /ws` | Live terminal stream |
| `POST /mcp` | MCP server — the agent-facing surface |

## Let an agent drive it (MCP)

The REST API is shaped for scripts. `/mcp` is the same control plane shaped for
a model — point Claude Code, Claude Desktop, or anything else speaking MCP at
it and it drives your assistant with typed tools:

```bash
claude mcp add --transport http wired http://127.0.0.1:8000/mcp
# with a token:
claude mcp add --transport http wired http://127.0.0.1:8000/mcp \
  --header "Authorization: Bearer $WIRED_AUTH_TOKEN"
```

Five tools, and the list is deliberate:

| Tool | Does |
|------|------|
| `wired_send_task` | Give the assistant work; optionally wait up to 60s for the reply |
| `wired_read_transcript` | Read what it has been asked and what it answered |
| `wired_session_status` | Is a session up, which CLI, is keep-alive on |
| `wired_answer_prompt` | Answer an approval dialog it is blocked on |
| `wired_set_assistant` | Save which CLI to run — `claude`, `grok`, `codex` or `gemini` — from the next session on |

Starting, stopping, killing and raw writes are **not** exposed. A tool the model
cannot call is the cheapest guardrail there is, and it matters more here than
usual — see below.

`wired_set_assistant` stops at the preference for that same reason: switching
CLIs ends the running session, and the caller may well *be* it. The session
keeps its own CLI until it exits — with always-on, it then comes back as the new
one; otherwise you restart it from Settings.

> **The loop.** Wired supervises an agent CLI. Point *that* agent at this MCP
> server and it can drive the session it is itself running in: typing into its
> own composer, then blocking on a reply it cannot produce because it is blocked
> in the tool call. It has no way to notice — from its side this is an ordinary
> HTTP tool.
>
> So each PTY child is handed a per-session nonce in `WIRED_SESSION_NONCE`, and
> `/mcp` refuses any request presenting it. Wire it up in the supervised agent's
> MCP config and the loop becomes a clear error instead of a silent hang:
>
> ```json
> { "headers": { "X-Wired-Session-Nonce": "${WIRED_SESSION_NONCE}" } }
> ```
>
> This is a footgun guard, not a security boundary — anyone can omit the header.
> The boundary is the token and the absent tools.

## Layout

```
crates/wired-backend/src/
  main.rs           the standalone server binary
  lib.rs            wiring; `serve()` is what the desktop app calls
  routes.rs         axum routes, the SSE tail and the WebSocket
  pty.rs            the shared PTY: spawn, read loop, ring buffer, fan-out
  assistant.rs      keep-alive supervisor
  agent_io.rs       say / answer — one path for REST, chat and scheduler
  recorder.rs       the single transcript everything else subscribes to
  gateway/          chat bridge: platform trait, Telegram, pairing codes
  schedule.rs       "every morning at 8" → a firing time (and cron)
  scheduler.rs      runs them, and delivers one summary rather than a stream
  setup.rs          Node detection, CLI install, folder scoping
  diagnostics.rs    the chain checks and the copyable report
  settings_store.rs settings.json, loaded under the environment
  secrets.rs        OS keychain, falling back to a 0600 file
  paths.rs          where settings, history and logs live per platform
  config.rs         env-driven settings + the unsafe-config guard
  security.rs       token and Origin checks (HTTP and WebSocket)
  transcript.rs     repainting screen  →  append-only conversation
  vt_screen.rs      vt100-backed virtual terminal
  terminal_clean.rs ANSI stripping for the raw debug path
  keys.rs           key names and escape decoding
  models.rs         request schemas
  mcp.rs            MCP tools — the agent-facing surface
  assets/reader.html  the standalone browser reader (compiled in)
  tests/            unit + a real end-to-end run over HTTP and a WebSocket
app/src/
  components/       conversation, composer, wizard, settings, history, schedule
  hooks/            backend status polling
  lib/              typed API client, the error map, xterm theme
  styles.css        design tokens as CSS custom properties
app/src-tauri/
  src/lib.rs        backend lifecycle, tray, runtime config, native pickers
docs/               getting-started, troubleshooting, releasing
scripts/
  install-ubuntu.sh one-shot server install
brand/              logo, mark and the icon master (see brand/README.md)
```

## Development

```bash
npm test                      # 46 tests: unit, live end-to-end, and MCP over JSON-RPC
npm run lint                  # clippy (deny warnings) + tsc
npm run format                # cargo fmt
npm run build:backend         # release binary
npm run build:desktop         # Tauri bundle, backend included
```

CI runs the same on every push; tagging `v*` builds installers for all four
targets — see [docs/releasing.md](docs/releasing.md), which also covers what has
to be true before a download link is worth giving to a non-developer.

### The desktop bundle

The backend is a *library* here, not a second process: `app/src-tauri` links
`wired-backend` and runs the API on the window's own Tokio runtime. The `.app`
holds one executable and needs nothing installed on the machine.

On launch the app asks `127.0.0.1:8000/healthz` who is there first:

| It finds | It does |
|----------|---------|
| A Wired backend already running | Attaches. Never starts a second one, never stops it on quit |
| Nothing | Serves the API in-process, and shuts it down when you quit |
| An unrelated process on the port | Binds the next free port and tells the window where it went |

That check is what keeps the desktop app compatible with the 24/7 premise —
opening the window next to a systemd service or an `npm run start` session
observes it rather than fighting it. The window learns its port and token from
the shell at runtime (`runtime_config`), which is what a build-time
`VITE_AUTH_TOKEN` could never do for a packaged app.

Closing the window no longer quits: the tray icon keeps the assistant answering,
with a visible **Stop the assistant** and a real **Quit**. Quitting runs the same
graceful path as Ctrl-C on the server — the chat bridge stops, the supervisor
stops, the agent's PTY is signalled, and the process exits. There is no second
process that can outlive the window.

## License

MIT — see [LICENSE](LICENSE).
