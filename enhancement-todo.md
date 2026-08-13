# Enhancement TODO — make Wired usable by a non-coder

> **Status: implemented.** Everything below is built and passing except the
> items that need a purchase, a Windows machine, or a screen recorder — those
> are marked ⏳ and say exactly what they are waiting on. See
> [Where it stands](#where-it-stands) at the end.

## The user

A normal person who wants a 24/7 assistant. Owns a laptop, does not open a
terminal, has never run `npm install`, does not know what a port is. Not stupid
— not a developer, and this is not his project.

Today Wired is excellent software aimed squarely at someone who already lives in
a terminal. Nothing below is a criticism of what exists: the REST API, the MCP
surface, the transcript filtering and the security defaults are the hard parts
and they are done. What is missing is a **path in** for someone who cannot build
from source, and a **daily surface** that never shows him a curl command.

## The benchmark: Hermes Agent

[Hermes Agent](https://github.com/NousResearch/hermes-agent) (Nous Research) is
the reference point for this document — an open-source agent that non-coders
actually ran. It is worth being precise about *why* it worked, because the
answer is not "it was a better agent." It is that every step between a person
and a working assistant had been removed.

| Step | Hermes | Wired before | Wired now |
|---|---|---|---|
| Install | `curl \| bash` installer | five-step build from source | ⏳ CI builds installers per OS; signing needs a certificate |
| Windows | native PowerShell one-liner | `providers.rs` was POSIX-only | `.exe`/`.cmd` resolution, PowerShell, `;` PATH — untested on real Windows |
| Configure | `hermes setup` wizard | 14 environment variables | four-step wizard + a Settings screen; env still wins |
| Credentials | `hermes setup --portal` | "already logged in" | the CLI's own sign-in, inside the app's PTY |
| Daily use | six chat platforms | `curl -sX POST …` | Telegram bridge, transcript relayed |
| Phone | message the bot | `ssh -N -L 8000:…` | message the bot |
| New device auth | 8-char pairing code | `head -c 24 /dev/urandom` | 8-char pairing code, OWASP parameters |
| Scheduling | `hermes cron create …` | "From cron" + a bash script | "every morning at 8", or cron |
| When broken | `hermes doctor` | read the logs a `.app` discards | Help screen: chain checks, log file, copy diagnostics |
| Updates | `hermes update` | re-clone and rebuild | ⏳ documented; needs release signing keys |

**What does not transfer.** Hermes is enormous — an 862 KB `cli.py`, a 443 KB
state module, 91 entries at the repo root. Wired is lean and that is a real
virtue; a supervisor for CLIs that already exist is a *better* premise than
another agent runtime. Do not copy the architecture. Copy the onboarding, the
access model, and the config system — the parts that face a person.

**The one insight to steal above all others:** *the assistant does not live in a
window you keep open.* Hermes's answer to "how do I reach it from my phone" is
not a tunnel, a PWA or a QR code — it is a bot on a chat app the user already
has, holding an outbound connection. No port opened, no TLS, no token pasted
into a URL bar. That single decision deletes most of Wired's remaining
non-coder problems, and it is cheaper to build than the alternatives.

### The acceptance test

> He downloads one file, double-clicks it, follows what the app tells him, and
> within ten minutes has an assistant that answers him — in Telegram, from the
> sofa. It is still answering tomorrow morning without him having reopened
> anything. He never sees a terminal, a token, an env var, or a port number.

Every step of that is now implemented. The one thing standing between it and a
real person is the code-signing certificate — see item 2.

### Non-goals

- **Do not remove the developer path.** `npm run start`, the REST API, `/reader`,
  the MCP server and `install-ubuntu.sh` stay exactly as they are. Everything
  here is an *additional* front door.
- Do not turn Wired into an agent. It supervises Claude Code and Grok; the
  transcript is the product.
- Do not chase Hermes's feature count. Six platforms, seven terminal backends
  and a skills marketplace are how Hermes got to 862 KB of CLI.

---

## 1. Talk to it from a chat app — the Hermes gateway model

Wired's README promises "from your phone" and used to deliver an SSH tunnel.
Hermes ran a gateway process holding an outbound connection to Telegram,
Discord, Slack, WhatsApp, Signal and email, and a non-coder's phone was already
logged into one of them.

Wired needs exactly **one** platform to close this gap. Telegram: best bot API,
no business verification, no per-message billing.

- [x] Telegram bridge — long-poll `getUpdates`, forward each message into the
      live session, stream replies back from the transcript. Outbound only.
      `gateway/telegram.rs`.
- [x] Reply with the *transcript* view, not raw screens. The bridge subscribes
      to the same `recorder` the SSE tail does, batching rows into paragraphs so
      a working agent does not arrive as forty notifications.
- [x] Approval prompts become inline **[Allow] / [Don't]** buttons, wired to the
      new `/api/agent/approve`. Never suppressed, even mid-scheduled-run — a
      hidden approval is a blocked agent.
- [x] **Pairing codes instead of tokens.** `gateway/pairing.rs`: 8 characters, no
      `0`/`O`/`1`/`I`, one-hour expiry, max 3 pending, one request per sender per
      10 minutes, 15-minute lockout after 5 failed attempts, `0600` state, and a
      `Debug` impl that redacts the code so it cannot reach a log.
- [x] Set-up flow in the app: paste the BotFather token once, message the bot,
      approve the code. Three steps, no terminal. Settings → *Talk to it from
      your phone*.
- [x] Structured as a `gateway` module behind a `Platform` trait. Discord is a
      file, not a rewrite. Not built.

*Acceptance: verified against the real Telegram API — an invalid token surfaces
as "Unauthorized: invalid token specified" on the Settings screen rather than a
silent failure.*

## 2. Ship a downloadable, signed installer

Hermes's install was one line that brought every dependency with it. Wired's
advantage is that the backend is a *single Rust binary* with no runtime — it can
ship a real double-clickable app with nothing underneath it.

- [x] `.github/workflows/release.yml` — builds macOS (arm64 + x86_64), Windows
      and Linux on a `v*` tag and attaches the bundles to a draft release. Plus
      `ci.yml` for fmt, clippy, tests, typecheck and build on every push.
- [x] **Signing: not happening, and no longer pending.** No Apple Developer
      account (US$99/year) and no Windows certificate, by choice — the
      certificate plumbing is gone from the workflow rather than sitting there
      waiting for secrets that will never be set. macOS builds are **ad-hoc**
      signed, which is free and is what keeps Gatekeeper saying "Apple could not
      verify…" (two clicks past) instead of **"damaged"** (no way past); see
      [docs/releasing.md](docs/releasing.md#ad-hoc-signing-is-free-and-it-is-not-optional)
      for why those are different failures. Windows ships unsigned;
      [docs/troubleshooting.md](docs/troubleshooting.md) documents the exact two
      clicks and the antivirus path.
- [x] Windows code paths fixed: `which` now tries `.exe`/`.cmd`/`.bat` (npm
      installs a shim, not a binary), `resolve_shell` returns PowerShell, `home`
      falls back to `%USERPROFILE%`, and the desktop shell's `widen_path` uses
      `split_paths`/`join_paths` instead of splitting on `:`. ⏳ **Not verified on
      a real Windows machine** — the acceptance list is in `docs/releasing.md`.
- [x] ⏳ Updater: **deliberately not enabled.** The Tauri updater plugin refuses
      to build without a minisign public key, and the matching private key is a
      release secret that cannot live in a repository. Four-step turn-on
      instructions are in `docs/releasing.md`; this is the strongest remaining
      argument for doing signing before a public launch.
- [x] One-line server install untouched — `install-ubuntu.sh` is the right path
      for off-laptop and is unchanged.
- [ ] ⏳ A landing page with one download button per OS. There is no website in
      this repository; the release page and `docs/getting-started.md` carry it
      for now.

## 3. First-run setup that installs and logs in the agent CLI

The README used to require the `claude` CLI "already logged in", and conceded the
login "is interactive and cannot be scripted". A non-coder had neither, and got
two greyed-out cards reading **"Not installed"** — a dead end inside a working
app.

Wired does better than Hermes on the hardest step, because **the app already
owns a PTY**.

- [x] The wizard (`Wizard.tsx`) replaces `Welcome` in the unconfigured state.
      Four steps, one screen each, visible progress.
- [x] **Check** — `/api/setup/state` probes Node, both CLIs, sign-in evidence and
      the folder in one call. Found → skip ahead.
- [x] **Install** — a button. `npm install --global --prefix ~/.npm-global`, so it
      never needs a password, into a prefix `providers.rs` already searches.
      Progress is a sentence ("Downloading…"), with the raw npm log behind a
      disclosure. Node is detected *first* and gets its own explanation.
- [x] **Sign in** — `/api/setup/login` starts the bare CLI (no auto-approve
      flags) in the PTY, and the wizard renders the terminal panel underneath.
      The unscriptable step is not scripted; it just happens.
- [x] **Pick a folder** — native picker via a Tauri command, defaulting to a
      created `~/Wired`, framed as *"Which folder may your assistant read and
      write?"*. The choice is write-tested before it is promised, and it now
      outranks whatever directory a GUI launch inherited.
- [x] Existing installs are detected and reused: `probe_providers` finds a CLI
      wherever it already is, and `signed_in` reports evidence without raising a
      keychain prompt — returning "can't tell" rather than a false negative.

*Acceptance: `/api/setup/state` on a live server reports Node, both CLIs, the
suggested folder and the install phase.*

## 4. Lead with the conversation, not the terminal

`App.tsx` defaulted to `view: 'terminal'` — his first sight of his assistant was
a raw full-screen TUI repainting itself.

- [x] Defaults to the conversation. Terminal is the second tab.
- [x] Message box moved **under the transcript**. The sidebar is deleted.
- [x] **Enter sends**, Shift+Enter makes a newline. IME composition is left
      alone, so Enter still confirms a candidate.
- [x] **"Type only"**, **"Line break"** and **"⏎ Enter"** are gone from the app.
      The API keeps them for scripts.
- [x] The `POST /api/agent/message` hint and the `WIRED_*` env dump are gone.
- [x] Welcome copy rewritten around *what can I ask it, right now?*, with three
      clickable starter prompts.
- [x] Renamed: "Keep alive 24/7 — restart the CLI if it exits" → **Always on**;
      "session" is gone from the default view.
- [x] Plain status: *"Working on it…"* / *"Waiting for you"* / *"Ready"* /
      *"Not running"*, replacing `CLAUDE · 24/7 · gen 3`.

## 5. Answer approval prompts with a button

The backend emitted `prompt` events, both readers rendered them, and then both
told the user to send `POST /api/agent/key`. The agent sat blocked until someone
reached for curl.

- [x] `POST /api/agent/approve {"allow": bool}` — one implementation in
      `agent_io::answer`, so the app, `/reader` and Telegram cannot drift apart.
- [x] **[Allow] / [Don't]** on every live prompt row in the app, in `/reader`,
      and as an inline keyboard in Telegram.
- [x] Auto-approve default reconsidered. `config::is_desktop()` splits it: the
      desktop app asks first, because there is somebody there to press a button
      and "stop asking me" is his own choice in Settings. Unattended servers keep
      today's pre-answered default. `WIRED_AGENT_AUTO_APPROVE` overrides both.

## 6. A config system, not fourteen environment variables

- [x] `settings.json` in the platform config directory (`paths.rs` covers macOS,
      Windows and XDG), loaded *under* the environment so every documented
      `WIRED_*` variable still wins and the developer path is untouched.
- [x] A Settings screen covering the four things he will actually change — which
      assistant, the folder, always-on, approvals — plus login item and
      notifications. Any setting the environment has taken over is reported in
      `env_overrides` and shown read-only rather than pretending to work.
- [x] **Token architecture fixed.** The window asks the desktop shell for its
      port and token at runtime (`runtime_config`); `VITE_AUTH_TOKEN` is now only
      the dev-server fallback. Secrets prefer the OS keychain (`security` on
      macOS, `secret-tool` on Linux) and fall back to the `0600` settings file —
      never both, so rotating a secret cannot leave a stale one behind.
- [x] Port is configurable and falls back to the next free one, in the app only:
      a server's port is somebody's firewall rule. When it moves, the log names
      the program that took it.

## 7. Never show an error he cannot act on

- [x] `lib/errors.ts` is the error map — symptom → one sentence → one button —
      written once and used by every surface.
- [x] *"Backend offline. Run `npm run start` from the project root"* → **"Wired
      isn't running"** with a **Try again** button. The API client no longer
      returns a shell command at all; it returns `offline` and the map does the
      talking.
- [x] The dev-only hint is gone from `Welcome`.
- [x] `providers.rs` no longer says *"Install the `claude` CLI and ensure it is
      on PATH"*. It says "Not installed yet — Wired can install it for you", and
      the button is in the wizard.
- [x] A busy port names the program holding it (`diagnostics::port_holder`), and
      the app moves rather than showing offline forever.

## 8. Keep running, and remember what happened

- [x] **Tray icon** with *Open Wired*, *Stop the assistant* and *Quit*. Closing
      the window hides it instead of quitting, so "24/7" on a laptop means 24/7.
- [x] **Start at login** via `tauri-plugin-autostart`, driven from Settings.
- [x] **Transcript persisted** — `recorder.rs` appends NDJSON, one file per local
      day, pruned to 30 days. A single process-wide recorder owns the only
      `TranscriptTail`, which is also what lets the SSE tail, the chat bridge and
      the store all see the same conversation.
- [x] **History view**: past days, searchable, with *Ask again* on anything you
      said.
- [x] Desktop notification when an approval is waiting and the window is not
      focused, plus a badge on the tab for a pending pairing request.
- [x] **Stop everything** is a red button in the header and an item in the tray.

## 9. Scheduling, without cron

- [x] Backend scheduler (`schedule.rs` + `scheduler.rs`), persisted alongside
      settings, firing the same path a phone message takes.
- [x] Human intervals — "every hour", "every morning at 8", "every monday at
      9am", "at 6:30 pm" — *and* five-field cron. What it cannot parse comes back
      as an example rather than an error code. Midnight/noon, DST gaps and
      "next Monday" are covered by unit tests.
- [x] Results delivered to the chat bridge — as **one labelled message**. The
      live relay is held back for the duration of a run (`Gateway::hold_relay`),
      so a 3am job arrives as a conclusion rather than forty fragments.
      Approval prompts are exempt.
- [x] **`[SILENT]`** implemented: the instruction is appended to the task, and a
      run whose whole output is `[SILENT]` sends nothing at all.
- [x] UI: task text, a when-field that says back what it understood ("every day
      at 08:00 · next tomorrow at 09:00"), each schedule's last result, a **Run
      it now** button, and three pre-filled examples so the feature explains
      itself.

## 10. Make the risk legible

- [x] A one-screen, plain-language explanation as step one of setup: what it can
      touch, what "always on" means, how to stop it, and that it asks first. No
      CORS tables.
- [x] The scoped folder is on screen at all times, under the composer.
- [x] A genuinely restricted default for first-timers: one folder, approvals on
      — with today's behaviour as an explicit choice in Settings, and unchanged
      on servers.

## 11. Support and diagnostics

- [x] **Copy diagnostics** — version, OS, arch, provider paths, port, folder,
      secret backing, gateway state, the chain checks and a log tail, as one
      pasteable block.
- [x] Logs go to a findable file as well as stdout — `~/Library/Logs/com.wired.terminal`
      on macOS — with **Open logs folder** next to it. A `.app` launch no longer
      discards the one artefact worth asking for.
- [x] A self-check that walks the chain — binary found, signed in, folder
      writable, port, logs, bridge connected, phone paired — and names the broken
      link with the button that fixes it. "Can't tell" is a distinct state from
      "broken".
- [x] An uninstall path: **Erase Wired's settings and history** removes the
      settings file, the schedules, the transcript, the logs and the keychain
      entries — and deliberately touches neither the user's files nor the CLIs
      Wired installed.

## 12. Documentation for a second audience

- [x] [**docs/getting-started.md**](docs/getting-started.md) — download, install,
      sign in, ask it something, reach it from a phone, schedule something, stop
      it. Zero shell blocks. `README.md` is unchanged in character and the two
      link to each other.
- [x] [**docs/troubleshooting.md**](docs/troubleshooting.md) — symptom → fix:
      *"It says damaged"*, *"It says offline"*, *"It stopped answering"*, *"How
      do I make it stop?"*, antivirus, phone pairing, missed schedules.
- [x] [**docs/releasing.md**](docs/releasing.md) — why releases are unsigned, why
      ad-hoc signing still matters, the updater, and the acceptance list.
- [ ] ⏳ Screenshots. `getting-started.md` is written to work without them and
      marks where each one goes; capturing them needs a person at a screen.
- [ ] ⏳ A 60-second demo GIF at the top of the repo.
- [ ] Translations. Deliberately later, as this document said.

---

## Where it stands

**Done and verified.** Chat bridge with pairing, setup wizard with install and
in-PTY sign-in, approval buttons on all three surfaces, settings store with
keychain-backed secrets, persisted searchable history, sentence-based scheduling
with `[SILENT]`, tray and login item, the diagnostics screen, the error map, the
chat-first UI, release and CI workflows, and the non-coder documentation.

46 tests pass; clippy is clean with `-D warnings`; the frontend typechecks and
builds; `npm run build:desktop` produces a working `.dmg`. The new endpoints were
exercised against a running server, including a real outbound call to the
Telegram API.

**Waiting on something outside the code:**

| | Waiting on |
|---|---|
| Windows verification | a Windows machine; the code paths are fixed, not tested |
| Automatic updates | release signing keys, which a repository cannot hold |
| Landing page, screenshots, demo GIF | a person at a screen, and somewhere to host |

Start the Apple enrolment now: it is the longest lead time on the list, and
until it finishes, the first thing a non-coder sees is *"Wired Terminal is
damaged and can't be opened."*
