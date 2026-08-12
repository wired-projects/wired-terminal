# Running it on an Ubuntu server

Every other page here assumes a window with buttons. This one assumes SSH and
nothing else: no desktop app, no tray icon, no Settings screen. Everything the
app does with a click is done here with one `wired` command — and the `curl`
underneath it is shown alongside, because on a server it is worth knowing what
the command is actually doing.

If you have a screen and a mouse, read [getting-started.md](getting-started.md)
instead — it is the same product, and much shorter.

---

## The shape of it

Three moving parts, and it helps to keep them apart in your head:

| Part | What it is | Where it lives |
|---|---|---|
| **`wired-backend`** | one static binary, the API and the supervisor | `/opt/wired-terminal/bin/`, run by systemd |
| **the agent CLI** | `claude` (or `grok`, `codex`, `gemini`), running inside a pseudo-terminal | started *by* the backend, as the service user |
| **the way in** | either the HTTP API, or the Telegram bridge | see [Reaching it](#reaching-it) |

The backend does not think. It keeps the CLI alive, records what scrolls past,
and gives you two ways to type into it.

**Nothing listens on the network by default.** The install binds `127.0.0.1`,
which means the only ways in are an SSH tunnel or Telegram — and Telegram works
by the server dialling *out*, so it needs no open port at all. That is the
recommended setup and the rest of this page assumes it.

---

## 1. Install

### The normal case

```bash
git clone <this repo> && cd wired-terminal
sudo bash scripts/install-ubuntu.sh
```

The script self-elevates, so `sudo` is belt-and-braces. It installs build
tools and the Rust toolchain, compiles the binary, writes
`/etc/wired-terminal/wired.env`, installs a systemd unit, starts it, and waits
for `/healthz` to answer.

**Which account runs the agent** is the one decision that matters, because the
agent can do anything that account can:

```bash
sudo bash scripts/install-ubuntu.sh                 # defaults to the user you sudo'd from
sudo bash scripts/install-ubuntu.sh --user wired    # a dedicated account
```

With no `--user` and no `SUDO_USER` (a root shell), it creates and uses `wired`.

### If `claude` is already installed and logged in

Reuse it. Installing Node and the npm CLI over the top can shadow a working
native install with a second copy that is not signed in:

```bash
sudo bash scripts/install-ubuntu.sh --user ubuntu --skip-node --skip-cli
```

`--skip-node` skips the Node install, `--skip-cli` skips `npm i -g
@anthropic-ai/claude-code`. Pass both when `which claude` already answers for
the account you named in `--user`.

### If the box has no git

Small VPS images often ship without git, and there is no reason to install it
just to move a directory. Ship the tree over SSH from your laptop:

```bash
# from your checkout, on your machine
tar --exclude='.git' --exclude='node_modules' --exclude='target' \
    -czf - . | ssh you@server 'mkdir -p ~/wired-terminal && tar -xzf - -C ~/wired-terminal'

ssh you@server 'cd ~/wired-terminal && sudo bash scripts/install-ubuntu.sh --user $USER'
```

`rsync` is nicer if the box has it. Many do not.

### If the box is small

The release build wants roughly a gigabyte of RAM per core. On a 1–2 GB
instance it can be killed by the OOM reaper partway through, which looks like
the build "just stopping". Either add swap first:

```bash
sudo fallocate -l 2G /swapfile && sudo chmod 600 /swapfile
sudo mkswap /swapfile && sudo swapon /swapfile
```

…or don't build on the server at all — build somewhere else and install the
binary (see [Upgrading](#5-upgrading-and-rebuilding)):

```bash
sudo bash scripts/install-ubuntu.sh --binary ./wired-backend
```

### What it wrote

| Path | What |
|---|---|
| `/opt/wired-terminal/bin/wired-backend` | the whole backend |
| `/opt/wired-terminal/bin/wired` | the management CLI |
| `/usr/local/bin/wired` | symlink to it, so it is on your `PATH` |
| `/etc/wired-terminal/wired.env` | settings + auth token, `0640`, root-owned, service group |
| `/etc/systemd/system/wired-terminal.service` | keeps it up across reboots |

Uninstall — service, unit, symlink, `/opt` dir, `/etc` dir — with
`sudo bash scripts/install-ubuntu.sh --uninstall`. It leaves the service
account, Node and the agent CLI alone.

### The `wired` command

Same crate as the backend, built and installed with it. It finds the API and its
token by itself — reading `wired.env`, which is why it works as any user in the
service group and asks for nothing:

```bash
wired status          # service, agent, API and chat in one screen
wired ask "..."       # send a task, wait for the reply, print it
wired watch           # live transcript, ctrl-c to detach
wired logs -f         # journalctl -u wired-terminal -f
wired restart
wired doctor          # every check in §3 at once, and an exit code
```

`wired --help` lists the rest. Each section below leads with the command and
keeps the `curl` beside it.

---

## 2. Log the agent in

This is the one step that is genuinely a shell command, because signing in is
interactive and there is no window to put it in. Do it **as the service user**
— credentials live in that account's home, and a token in your own home does
the service no good:

```bash
sudo -u wired -H claude          # answer its prompts, then exit with Ctrl-D
sudo systemctl restart wired-terminal
```

The `-H` matters: without it `sudo` keeps *your* `HOME` and the login lands in
the wrong place.

The alternative is an API key, no interaction:

```bash
sudo sh -c 'echo "ANTHROPIC_API_KEY=sk-ant-..." >> /etc/wired-terminal/wired.env'
sudo systemctl restart wired-terminal
```

### While you are in there

Two settings worth adding to `/etc/wired-terminal/wired.env` on a headless box:

```ini
# No desktop keyring here. If `secret-tool` happens to be installed, a
# credential read can block waiting for a D-Bus session that will never exist.
WIRED_USE_KEYCHAIN=0

# Where the agent works. Not the service user's bare home, if that home holds
# .ssh or other services' .env files.
WIRED_AGENT_CWD=/home/wired/wired-work
```

Create that directory first (`sudo -u wired mkdir -p /home/wired/wired-work`) —
`WIRED_AGENT_CWD` is ignored if the path is not a directory, and it falls back
to the home directory silently. Restart after editing:
`sudo systemctl restart wired-terminal`.

> With auto-approve on — the server default — this is a speed bump, not a
> boundary. The agent can still `cd` anywhere the service user can read. The
> account is the boundary. `WIRED_AGENT_AUTO_APPROVE=0` if you would rather it
> stopped to ask, but then somebody has to be there to answer.

---

## 3. Check it is actually up

```bash
wired status          # the short answer
wired doctor          # the long one: CLI, sign-in, folder, port, logs
```

`wired status` exits `0` when the agent is running, `2` when something in that
screen is not — which is the whole of a health check for a monitoring cron.

The same three things by hand:

```bash
curl -s localhost:8000/healthz                                    # liveness
curl -s localhost:8000/api/agent/status | python3 -m json.tool     # the useful one
journalctl -u wired-terminal -f                                   # what it is doing
```

`/api/agent/status` tells you the three things that go wrong:

- `providers.claude.available` — is the CLI on the service user's `PATH`
- `session.running` — is there a live CLI in a PTY right now
- `assistant.keep_alive` — will it be restarted when it exits

No `Authorization` header anywhere here: on the default loopback bind the
install writes no token, so there is nothing to send. If you installed with
`--host 0.0.0.0` there is one, and every command below needs it:

```bash
TOKEN=$(sudo sed -n 's/^WIRED_AUTH_TOKEN=//p' /etc/wired-terminal/wired.env)
curl -s -H "Authorization: Bearer $TOKEN" localhost:8000/api/agent/status
```

Then send it something, and wait for the answer in the same response:

```bash
wired ask "What is in the current directory?"
```

```bash
curl -sX POST localhost:8000/api/agent/message \
  -H 'Content-Type: application/json' \
  -d '{"text":"What is in the current directory?","wait_seconds":45}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin).get("text",""))'
```

`wait_seconds` is what makes this usable from a script or a cron line with no
UI: it holds the response open, collects the terminal output, and returns early
once the agent has been quiet for `idle_seconds` (1.5 by default).

---

## 4. Telegram, end to end

This is the part with no GUI equivalent on a server, and the part the other
docs describe entirely in terms of buttons that do not exist here.

It is worth doing even if you are comfortable with SSH: the server dials out to
Telegram, so **no port is opened, no tunnel is held, and no TLS is terminated.**

### 4.1 Make a bot

In Telegram, message **@BotFather** → `/newbot` → answer the name and username
prompts. It replies with a token like `8000000000:AAF...`. That token is a
password to the chat; treat it that way.

### 4.2 Hand it to the server

One call sets the token, switches the bridge on, and reconnects:

```bash
curl -sX POST localhost:8000/api/gateway/configure \
  -H 'Content-Type: application/json' \
  -d '{"bot_token":"8000000000:AAF...","enabled":true}'
```

Both fields matter. The bridge stays down if `enabled` is false *or* the token
is empty, and it will not tell you which — check:

```bash
curl -s localhost:8000/api/gateway/status | python3 -m json.tool
```

You want `"configured": true`, `"enabled": true`, `"connected": true`, and
`"bot"` set to the username you chose. If `connected` is false, `last_error`
holds Telegram's own words — `Unauthorized` means a wrong or revoked token.

> Shell history: that command puts the bot token in `~/.bash_history`. Prefix
> it with a space (with `HISTCONTROL=ignorespace` set), or read the token from
> a file, if that matters to you.

### 4.3 Pair your phone

Open your new bot in Telegram and send it anything — `hello` is fine.

It answers with an 8-character code and tells you to *"open Wired, go to
Settings → Telegram"*. Ignore that sentence; it is written for the desktop app.
Here, you list the waiting requests and approve one:

```bash
wired pair                        # who is asking, and their code
wired pair approve K7M2PQ84       # once you have checked it is you
```

The same by hand:

```bash
curl -s localhost:8000/api/gateway/pairings | python3 -m json.tool
```

```json
{"pending": [{"platform":"telegram","chat":184766117,
              "display":"Sam (@samw)","code":"K7M2PQ84","expires_in":3502}]}
```

Check the `display` is you, then:

```bash
curl -sX POST localhost:8000/api/gateway/pairings/approve \
  -H 'Content-Type: application/json' -d '{"code":"K7M2PQ84"}'
```

Send the bot another message. It now reaches the agent, and answers come back
to your phone.

Things worth knowing about pairing:

- Codes expire after **one hour**. Message the bot again for a new one.
- The **first** chat you approve becomes the owner: scheduled results, approval
  requests and "I restarted" go there.
- A stranger who finds your bot gets a code and nothing else. Nothing reaches
  the server until you approve it. Deny with `wired pair deny <code>`
  (`POST /api/gateway/pairings/deny`), or just let it expire.
- **Five wrong codes locks pairing for 15 minutes.** Copy-paste, don't retype.
- One bot token, one poller. If another program is already long-polling the
  same token, the two fight over `getUpdates` and both behave erratically —
  make a second bot rather than sharing one.

### 4.4 From the phone

Send it plain English and it goes to the agent. These are intercepted instead:

| | |
|---|---|
| `/help`, `/menu` | the button menu |
| `/status` | provider, running or not, working folder |
| `/cancel`, `/esc` | Escape — backs out of a dialog the agent is showing |
| `/stop` | stop the session |
| `/mute`, `/unmute` | stop and resume replies |
| `/restart` | restart the session |
| `/claude`, `/grok`, `/codex`, `/gemini` | switch CLI (ends the running session) |

When the agent puts a menu on screen, the bridge renders one button per option
and **your next message is a keystroke, not a message.** Answer with a button
or `/cancel`, and know that a stray "ok" while a picker is open presses
whatever is highlighted.

### 4.5 Changing or revoking the bot

```bash
curl -sX POST localhost:8000/api/gateway/reset
```

That throws away the token, unpairs every chat, and switches the bridge off,
leaving a clean state to configure a new bot into. Revoke the old token in
BotFather too (`/revoke`) — the server forgetting it does not stop it working
for anyone else who has it.

To drop one phone rather than all of them: `wired pair unpair 184766117`
(`POST /api/gateway/unpair`). `wired pair` lists the chat ids.

---

## Reaching it

Without Telegram, from your laptop, no port opened:

```bash
ssh -N -L 8000:127.0.0.1:8000 you@server
```

Then `http://localhost:8000/reader` in your browser is the live transcript,
rendered as a conversation rather than a terminal, and the whole API is on
`localhost:8000` as if it were local.

If you have `wired` on your laptop too, it opens that tunnel itself, for the
length of one command, and closes it after:

```bash
wired remote add pilot wired@server     # once
wired --remote pilot status
wired --remote pilot ask "anything overnight?"
wired --remote pilot logs -f            # this one runs over ssh, not the tunnel
```

Install it there with `cargo install --path crates/wired-backend` from a
checkout. Saved servers go in `cli.json` next to `settings.json`, `0600` — a
`--token` you save there is a password to a shell on that box.

Opening the port instead (`--host 0.0.0.0`) means the token is the only thing
between a stranger and a shell as the service user. If you do it, put TLS and a
firewall in front — `sudo ufw allow from <your-ip> to any port 8000` at the
absolute minimum.

---

## 5. Upgrading and rebuilding

Re-running the installer upgrades in place: it stops the service, rebuilds,
replaces the binary, and starts it again. Settings in `wired.env` survive.

```bash
cd ~/wired-terminal && git pull
sudo bash scripts/install-ubuntu.sh --user wired --skip-node --skip-cli
```

**The trap:** the installer runs as root, so `cargo` runs as root, so
`crates/wired-backend/target/` ends up owned by root. A later `cargo build` as
yourself then fails with permission errors on a directory you appear to own the
parent of. Two ways out:

```bash
# keep building as root — needs rustup's PATH, which is under /root
sudo env PATH="/root/.cargo/bin:$PATH" cargo build --release \
  --manifest-path ~/wired-terminal/crates/wired-backend/Cargo.toml

# or hand the tree back to yourself, once
sudo chown -R $USER:$USER ~/wired-terminal/crates/wired-backend/target
```

Building elsewhere and shipping the binary avoids all of it — and avoids
keeping a Rust toolchain on the server. Build for the server's architecture
(most cheap ARM instances are `aarch64`; check with `uname -m`):

```bash
scp ./wired-backend you@server:~/
ssh you@server 'sudo bash ~/wired-terminal/scripts/install-ubuntu.sh --binary ~/wired-backend'
```

---

## Where things live

Not in the repo, and not where the desktop docs say. Everything below is under
the **service user's** home, so read it with `sudo -u wired`:

| What | Path |
|---|---|
| Settings, incl. paired chats | `~/.config/wired-terminal/settings.json` |
| Saved servers for `wired --remote` | `~/.config/wired-terminal/cli.json` |
| Transcripts, one file per day | `~/.local/share/wired-terminal/transcript/` |
| Logs | `journalctl -u wired-terminal`, not a file |
| Service settings | `/etc/wired-terminal/wired.env` |

The Telegram bot token is in `settings.json` too. On a desktop it would be in
the OS keychain; a server has none, so it falls back to the settings file,
written `0600`. Check which with `curl -s localhost:8000/api/settings` →
`"secrets"`. Back up or copy that file accordingly.

Every `WIRED_*` variable in `wired.env` **overrides** the matching value in
`settings.json`. If a setting will not stick, that is almost always why.

---

## When it goes wrong

**Start with `wired doctor`.** It checks the things most of these turn out to be
— CLI installed, signed in, folder writable, port, chat — and prints the paths
the rest of this section refers to. `wired doctor --log` adds the tail of the
log. What follows is what to do about each answer.

**`systemctl status` says active, but nothing answers.**
`wired logs -n 50`. A bad `WIRED_*` value in `wired.env` is the usual cause —
the service restarts every 3 seconds and the log says why.

**`session.running` is false and stays false.**
The CLI is not on the service user's `PATH`, or it is not signed in. Check as
that user, not as yourself: `sudo -u wired -H which claude`, then
`sudo -u wired -H claude` and see what it says.

**It answers, but every reply is a login prompt.**
You logged in as the wrong user, or without `-H`. Redo §2 as the service user.

**The bot is silent.**
`wired pair`, or `/api/gateway/status` behind it. `connected: false` with
`last_error` set is Telegram talking to you. `muted: true` means someone sent `/mute`. `paired_chats: 0`
means the pairing never completed.

**The agent is stuck on a question.**
Somebody has to answer it. `wired approve` (or `wired approve --deny`), `/cancel`
from Telegram, or watch it live with `wired watch`. Approval reads the menu off
the screen and picks the affirmative option — do not send raw digits, the number
that means "yes" is not the same on every dialog.

**A schedule never ran.** Two jobs never run at once; if one is still going the
next waits. Missed runs are rescheduled forward, not replayed.
