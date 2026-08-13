#!/usr/bin/env bash
# Install Wired Terminal as a systemd service on Ubuntu/Debian.
#
# The backend is one static binary — no interpreter, no venv, no runtime
# dependencies beyond libc. This installs a published one and wires up systemd.
#
#   sudo bash scripts/install-ubuntu.sh                     # loopback, no token
#   sudo bash scripts/install-ubuntu.sh --host 0.0.0.0      # network + token
#   sudo bash scripts/install-ubuntu.sh --uninstall
#
# Everything it touches:
#   /opt/wired-terminal/bin/wired-backend
#   /opt/wired-terminal/bin/wired  the `wired` command
#   /usr/local/bin/wired           symlink to it, so it is on everyone's PATH
#   /etc/wired-terminal/wired.env  settings and the auth token (0640)
#   /etc/systemd/system/wired-terminal.service
set -euo pipefail

SERVICE_NAME="wired-terminal"
INSTALL_DIR="/opt/wired-terminal"
ENV_DIR="/etc/wired-terminal"
ENV_FILE="$ENV_DIR/wired.env"
UNIT_FILE="/etc/systemd/system/$SERVICE_NAME.service"
# On Ubuntu this is already on PATH, including sudo's secure_path.
LINK_DIR="/usr/local/bin"
CLI_LINK="$LINK_DIR/wired"

HOST="127.0.0.1"
PORT="8000"
PROVIDER=""
TOKEN=""
AUTO_APPROVE="1"
SVC_USER=""
BINARY=""
# Published binaries, so a server install does not need a Rust toolchain. The
# alias always points at the newest release and is set to revalidate, which is
# why this is a fixed URL and not a manifest lookup: no jq, no parsing, and one
# thing to override when someone mirrors the bucket.
RELEASES_BASE="${WIRED_RELEASES_BASE:-https://wired-terminal-releases.wired.dev}"
SERVER_URL=""
SKIP_NODE=0
SKIP_CLI=0
NO_SERVICE=0
UNINSTALL=0

bold=$'\033[1m'; dim=$'\033[2m'; red=$'\033[31m'; yellow=$'\033[33m'; off=$'\033[0m'
log()  { printf '%s==>%s %s\n' "$bold" "$off" "$*"; }
info() { printf '    %s%s%s\n' "$dim" "$*" "$off"; }
warn() { printf '%s !! %s%s\n' "$yellow" "$*" "$off" >&2; }
die()  { printf '%s !! %s%s\n' "$red" "$*" "$off" >&2; exit 1; }

usage() {
  sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
  cat <<'EOF'

Options:
  --host HOST        bind address (default 127.0.0.1; anything else needs a token)
  --port PORT        listen port (default 8000)
  --user NAME        run the agent as this user (default: the invoking user)
  --token VALUE      auth token (default: generated when --host is not loopback)
  --provider NAME    claude | grok | codex | gemini (default: first CLI found)
  --binary PATH      install this binary instead of the published one
  --server-url URL   tarball of wired-backend + wired to install
  --dir PATH         install directory (default /opt/wired-terminal)
  --no-auto-approve  make the agent pause for approval instead of acting freely
  --skip-node        do not install Node.js
  --skip-cli         do not install the Claude Code CLI
  --no-service       install files only, no systemd unit
  --uninstall        stop the service and remove everything it installed
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --host) HOST="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    --user) SVC_USER="$2"; shift 2 ;;
    --token) TOKEN="$2"; shift 2 ;;
    --provider) PROVIDER="$2"; shift 2 ;;
    --binary) BINARY="$2"; shift 2 ;;
    --server-url) SERVER_URL="$2"; shift 2 ;;
    --dir) INSTALL_DIR="$2"; shift 2 ;;
    --no-auto-approve) AUTO_APPROVE="0"; shift ;;
    --skip-node) SKIP_NODE=1; shift ;;
    --skip-cli) SKIP_CLI=1; shift ;;
    --no-service) NO_SERVICE=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
done

# ── Elevate ─────────────────────────────────────────────────────────────
# apt and systemd need root; the agent itself must not run as root (the CLIs
# refuse to skip permission prompts when they are).
if [ "$(id -u)" -ne 0 ]; then
  [ -f "$0" ] || die "run this with sudo: sudo bash $0 $*"
  command -v sudo >/dev/null || die "run this as root"
  log "re-running with sudo"
  exec sudo bash "$0" "$@"
fi

# --dir gets rm -rf'd on uninstall, so refuse shared paths.
case "${INSTALL_DIR%/}" in
  ""|/|/usr|/usr/*bin|/etc|/home|/var|/opt|/root|/tmp|/srv)
    die "refusing to manage $INSTALL_DIR — pass a dedicated --dir" ;;
esac

# ── Uninstall ───────────────────────────────────────────────────────────
if [ "$UNINSTALL" -eq 1 ]; then
  log "removing $SERVICE_NAME"
  systemctl stop "$SERVICE_NAME" 2>/dev/null || true
  systemctl disable "$SERVICE_NAME" 2>/dev/null || true
  rm -f "$UNIT_FILE"
  systemctl daemon-reload
  # Only ours: someone else's /usr/local/bin/wired is not this script's to delete.
  if [ -L "$CLI_LINK" ] && [ "$(readlink "$CLI_LINK")" = "$INSTALL_DIR/bin/wired" ]; then
    rm -f "$CLI_LINK"
  fi
  rm -rf "$INSTALL_DIR" "$ENV_DIR"
  info "removed $INSTALL_DIR, $ENV_DIR, $CLI_LINK and the systemd unit"
  info "the service user, Node.js and the agent CLI were left in place"
  exit 0
fi

command -v apt-get >/dev/null || die "this installer targets Ubuntu/Debian (no apt-get found)"

# ── Who runs the agent ──────────────────────────────────────────────────
# Whatever this account can do, anyone who can reach the API can do.
if [ -z "$SVC_USER" ]; then
  SVC_USER="${SUDO_USER:-}"
fi
if [ -z "$SVC_USER" ] || [ "$SVC_USER" = "root" ]; then
  SVC_USER="wired"
  if ! id -u "$SVC_USER" >/dev/null 2>&1; then
    log "creating service user '$SVC_USER'"
    useradd --create-home --shell /bin/bash "$SVC_USER"
  fi
fi
id -u "$SVC_USER" >/dev/null 2>&1 || die "user '$SVC_USER' does not exist"
SVC_GROUP="$(id -gn "$SVC_USER")"
SVC_HOME="$(getent passwd "$SVC_USER" | cut -d: -f6)"
log "agent will run as $SVC_USER ($SVC_HOME)"

IS_LOOPBACK=0
case "$HOST" in 127.0.0.1|::1|localhost) IS_LOOPBACK=1 ;; esac

# ── Packages ────────────────────────────────────────────────────────────
log "installing system packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq curl ca-certificates >/dev/null
info "curl, ca-certificates"

node_major() {
  command -v node >/dev/null || { echo 0; return; }
  local v
  v="$(node -v 2>/dev/null | sed -n 's/^v\([0-9][0-9]*\).*/\1/p')"
  echo "${v:-0}"
}

# Node is for the agent CLI, not for Wired — the backend needs nothing.
if [ "$SKIP_NODE" -eq 0 ]; then
  if [ "$(node_major)" -lt 18 ]; then
    log "installing Node.js for the agent CLI"
    apt-get install -y -qq nodejs npm >/dev/null 2>&1 || true
    if [ "$(node_major)" -lt 18 ]; then
      info "apt's Node is too old — using NodeSource 20.x"
      curl -fsSL https://deb.nodesource.com/setup_20.x | bash - >/dev/null
      apt-get install -y -qq nodejs >/dev/null
    fi
  fi
  info "node $(node -v 2>/dev/null || echo missing)"
fi

if [ "$SKIP_CLI" -eq 0 ] && ! command -v claude >/dev/null; then
  if command -v npm >/dev/null; then
    log "installing Claude Code CLI"
    npm install -g @anthropic-ai/claude-code >/dev/null
    info "$(command -v claude || echo 'claude not on PATH — check npm global bin')"
  else
    warn "npm not available — install the claude CLI yourself"
  fi
fi

# ── The binary ──────────────────────────────────────────────────────────
# This installer does not build anything. It installs a published binary, or one
# you hand it with --binary, and nothing else — no build-essential, no rustup, no
# cargo, no compile on the machine being installed to. A server needs none of
# that to *run* Wired, and installing a toolchain to produce a binary that is
# already published was minutes of work and a permanent Rust install for nothing.
#
# The cost is deliberate: an architecture with no published build cannot install
# itself here. Build it elsewhere and pass --binary. Building is a developer
# task, and `cargo build --release` from a checkout is the whole of it.
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) SERVER_TARGET="linux-x86_64" ;;
  aarch64|arm64) SERVER_TARGET="linux-aarch64" ;;
  *) SERVER_TARGET="" ;;
esac
if [ -z "$SERVER_URL" ] && [ -n "$SERVER_TARGET" ]; then
  SERVER_URL="$RELEASES_BASE/downloads/$SERVER_TARGET-server.tar.gz"
fi

# Unpack the published pair into $1, or fail with a reason.
#
# The version check is the point of this, not decoration. The tarball is built
# on Ubuntu 22.04, so glibc 2.35 is its floor; on anything older the dynamic
# loader kills it with "GLIBC_2.35 not found" at first start — which, without
# this, would be after systemd had already been pointed at it.
try_download_server() {
  local dest="$1" tmp rc=1
  tmp="$(mktemp -d)"
  if ! curl -fsSL --retry 3 --retry-delay 2 -o "$tmp/server.tar.gz" "$SERVER_URL"; then
    warn "nothing published at $SERVER_URL"
  elif ! tar -xzf "$tmp/server.tar.gz" -C "$tmp" 2>/dev/null; then
    warn "the downloaded archive did not unpack"
  elif [ ! -x "$tmp/wired-backend" ] || [ ! -x "$tmp/wired" ]; then
    warn "the archive did not contain wired-backend and wired"
  elif ! "$tmp/wired" --version >/dev/null 2>&1; then
    warn "the published binary does not run on this system (glibc older than the build's?)"
  else
    install -m 0755 "$tmp/wired-backend" "$dest/wired-backend"
    install -m 0755 "$tmp/wired" "$dest/wired"
    rc=0
  fi
  rm -rf "$tmp"
  return "$rc"
}

if systemctl is-active --quiet "$SERVICE_NAME" 2>/dev/null; then
  log "stopping the running service first"
  systemctl stop "$SERVICE_NAME"
fi

install -d -m 0755 "$INSTALL_DIR/bin"

if [ -n "$BINARY" ]; then
  [ -x "$BINARY" ] || die "--binary $BINARY is not executable"
  log "installing the binary you passed"
  install -m 0755 "$BINARY" "$INSTALL_DIR/bin/wired-backend"
  # The CLI is built from the same crate and ships beside the server.
  if [ -x "$(dirname "$BINARY")/wired" ]; then
    install -m 0755 "$(dirname "$BINARY")/wired" "$INSTALL_DIR/bin/wired"
  fi
else
  [ -n "$SERVER_URL" ] || die "no binaries are published for $ARCH — build one elsewhere and pass --binary <path>"
  log "fetching the published binaries"
  try_download_server "$INSTALL_DIR/bin" || die "could not install a published binary (see above) — pass --binary <path> to install one you built"
fi
info "$(du -h "$INSTALL_DIR/bin/wired-backend" | cut -f1) → $INSTALL_DIR/bin/wired-backend"

# One command to manage the rest of this, without remembering where it lives.
if [ -x "$INSTALL_DIR/bin/wired" ]; then
  install -d -m 0755 "$LINK_DIR"
  ln -sfn "$INSTALL_DIR/bin/wired" "$CLI_LINK"
  info "wired → $CLI_LINK"
else
  warn "no 'wired' CLI alongside --binary; managing this install means systemctl and curl"
fi

# ── Settings ────────────────────────────────────────────────────────────
if [ "$IS_LOOPBACK" -eq 0 ] && [ -z "$TOKEN" ]; then
  TOKEN="$(head -c 24 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=')"
  info "generated an auth token (--host $HOST is reachable off-box)"
fi

log "writing $ENV_FILE"
install -d -m 0755 "$ENV_DIR"
{
  echo "# Wired Terminal — read by systemd."
  echo "WIRED_HOST=$HOST"
  echo "WIRED_PORT=$PORT"
  if [ -n "$TOKEN" ]; then
    echo "WIRED_AUTH_TOKEN=$TOKEN"
  else
    echo "# WIRED_AUTH_TOKEN=   # required to bind off-loopback"
  fi
  if [ -n "$PROVIDER" ]; then
    echo "# Pins the assistant. This wins over the /claude, /grok, /codex and"
    echo "# /gemini switches from chat, so"
    echo "# those switches last until the next restart. Delete it to let them stick."
    echo "WIRED_ASSISTANT_PROVIDER=$PROVIDER"
  fi
  echo "WIRED_ASSISTANT_AUTO_START=1"
  echo "WIRED_ASSISTANT_KEEP_ALIVE=1"
  echo "WIRED_AGENT_AUTO_APPROVE=$AUTO_APPROVE"
  echo "# Where the agent works. Its own home by default."
  echo "WIRED_AGENT_CWD=$SVC_HOME"
  echo "# ANTHROPIC_API_KEY=    # alternative to an interactive claude login"
} > "$ENV_FILE"
chown "root:$SVC_GROUP" "$ENV_FILE"
chmod 0640 "$ENV_FILE"

if [ "$NO_SERVICE" -eq 1 ]; then
  log "done (no service installed)"
  info "run it with: sudo -u $SVC_USER -H env \$(grep -v '^#' $ENV_FILE | xargs) \\"
  info "  $INSTALL_DIR/bin/wired-backend"
  exit 0
fi

# ── systemd ─────────────────────────────────────────────────────────────
# Deliberately unhardened: this service exists to run whatever the agent
# decides to run, so ProtectSystem/ReadOnlyPaths would only break it. The
# boundary is the service account and the token, not the sandbox.
log "installing $UNIT_FILE"
cat > "$UNIT_FILE" <<EOF
[Unit]
Description=Wired Terminal — 24/7 agent control plane
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$SVC_USER
Group=$SVC_GROUP
WorkingDirectory=$SVC_HOME
Environment=HOME=$SVC_HOME
EnvironmentFile=$ENV_FILE
ExecStart=$INSTALL_DIR/bin/wired-backend
Restart=always
RestartSec=3
TimeoutStopSec=15
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable --now "$SERVICE_NAME" >/dev/null
log "waiting for the API"

PROBE_HOST="$HOST"
case "$HOST" in 0.0.0.0|::|"") PROBE_HOST="127.0.0.1" ;; esac
BASE="http://$PROBE_HOST:$PORT"

ok=0
for _ in $(seq 1 20); do
  if curl -fsS "$BASE/healthz" >/dev/null 2>&1; then ok=1; break; fi
  sleep 0.5
done

echo
if [ "$ok" -eq 1 ]; then
  log "Wired Terminal is running on $BASE"
else
  warn "service did not answer /healthz — check: journalctl -u $SERVICE_NAME -n 50"
fi

AUTH_HDR=""
[ -n "$TOKEN" ] && AUTH_HDR=" -H 'Authorization: Bearer $TOKEN'"

cat <<EOF

  ${bold}Next${off}
  1. Log the agent CLI in, once, as $SVC_USER:
       sudo -u $SVC_USER -H claude
     (or put ANTHROPIC_API_KEY in $ENV_FILE and restart)
  2. Restart so the session picks it up:
       wired restart
  3. Send it a task:
       wired ask "Summarize my git status"

  ${bold}Manage it${off}        wired status · wired watch · wired logs -f · wired doctor
  ${bold}From your laptop${off} wired remote add $(hostname -s 2>/dev/null || echo box) $SVC_USER@<this-server>
  ${bold}Live transcript${off}  $BASE/reader$([ -n "$TOKEN" ] && echo "?token=$TOKEN")
  ${bold}Settings${off}         $ENV_FILE  (restart after editing)
  ${bold}The same by hand${off} curl -sX POST $BASE/api/agent/message$AUTH_HDR \\
                     -H 'Content-Type: application/json' \\
                     -d '{"text":"Summarize my git status","submit":true,"wait_seconds":45}'
EOF

if [ -n "$TOKEN" ]; then
  cat <<EOF
  ${bold}Token${off}            $TOKEN
                   also in $ENV_FILE
EOF
fi

echo
if [ "$IS_LOOPBACK" -eq 1 ]; then
  info "Bound to loopback. Reach it from your laptop over SSH rather than opening the port:"
  info "  ssh -N -L $PORT:127.0.0.1:$PORT $SVC_USER@<this-server>"
else
  warn "Port $PORT is reachable from the network. The token is the only thing between"
  warn "a stranger and a shell as $SVC_USER — put it behind TLS and a firewall:"
  warn "  sudo ufw allow from <your-ip> to any port $PORT"
fi
if [ "$AUTO_APPROVE" = "1" ]; then
  warn "Auto-approve is ON: the agent acts without confirming. WIRED_AGENT_AUTO_APPROVE=0 to change."
fi
