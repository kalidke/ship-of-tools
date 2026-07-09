#!/usr/bin/env bash
# launch-sot.sh — Linux/macOS frontend client → remote backend over SSH.
#
# Opens an SSH tunnel to the backend host (forwarding a local TCP port to the
# remote user's per-user `sotd` socket, plus Pluto 1234 / video 1235 / docs
# 1236), ensures the remote `sotd` is running, then runs the local frontend
# pointed at the forwarded local port. The remote BE must already be BUILT on
# the host.
#
# Idempotent: an `ssh -fN` tunnel is backgrounded and OUTLIVES the FE window, so
# a naive re-run would collide on the forwarded ports (Address already in use)
# and — under `set -e` — abort before launching the FE. We therefore reuse an
# existing tunnel instead of opening a second one, and only (re)spawn the backend
# when it isn't already up.
#
# Overridable via env: SOT_HOST, SOT_TCP_PORT, SOT_REMOTE_REPO,
# SOT_REMOTE_SOCKET (default: query `sotd session-socket-path sot` remotely),
# SOT_RESTART_BE=1 (force a backend restart even if one is running).
set -uo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
: "${SOT_HOST:?set SOT_HOST or configure .sot/hosts.toml}"
: "${SOT_REMOTE_REPO:?set SOT_REMOTE_REPO or configure .sot/hosts.toml}"
HOST="$SOT_HOST"
PORT="${SOT_TCP_PORT:-18743}"
REMOTE_REPO="$SOT_REMOTE_REPO"
REMOTE_SOCKET="${SOT_REMOTE_SOCKET:-}"
PLUTO_PORT="${SOT_PLUTO_PORT:-1234}"
VIDEO_PORT="${SOT_VIDEO_PORT:-1235}"
DOCS_PORT="${SOT_DOCS_PORT:-1236}"
AUX_PORTS=("$PLUTO_PORT" "$VIDEO_PORT" "$DOCS_PORT" "$((DOCS_PORT+1))" "$((DOCS_PORT+2))" "$((DOCS_PORT+3))" "$((DOCS_PORT+4))")

port_open() {
    if (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; then exec 3>&-; return 0; fi
    command -v nc >/dev/null 2>&1 && nc -z 127.0.0.1 "$1" >/dev/null 2>&1
}

ensure_aux_tunnel() {
    local missing=()
    local p
    for p in "${AUX_PORTS[@]}"; do
        port_open "$p" || missing+=("$p")
    done
    if [ "${#missing[@]}" -eq 0 ]; then
        echo "browser aux ports already forwarded (${AUX_PORTS[*]})"
        return 0
    fi
    if [ "${#missing[@]}" -ne "${#AUX_PORTS[@]}" ]; then
        echo "ERROR: only some browser aux ports are open; missing: ${missing[*]}" >&2
        echo "       stop stale tunnels/services or free ports: ${AUX_PORTS[*]}" >&2
        exit 1
    fi
    ssh -fN -o ServerAliveInterval=30 -o ExitOnForwardFailure=yes \
        -L "$PLUTO_PORT:127.0.0.1:$PLUTO_PORT" \
        -L "$VIDEO_PORT:127.0.0.1:$VIDEO_PORT" \
        -L "$DOCS_PORT:127.0.0.1:$DOCS_PORT" \
        -L "$((DOCS_PORT+1)):127.0.0.1:$((DOCS_PORT+1))" \
        -L "$((DOCS_PORT+2)):127.0.0.1:$((DOCS_PORT+2))" \
        -L "$((DOCS_PORT+3)):127.0.0.1:$((DOCS_PORT+3))" \
        -L "$((DOCS_PORT+4)):127.0.0.1:$((DOCS_PORT+4))" "$HOST" \
        || { echo "ERROR: could not open browser aux SSH tunnel to $HOST" >&2; exit 1; }
}

if [ -z "$REMOTE_SOCKET" ]; then
    REMOTE_SOCKET="$(ssh "$HOST" "cd '$REMOTE_REPO' && ./rust/target/release/sotd session-socket-path sot")" \
        || { echo "ERROR: could not query remote sotd socket path on $HOST" >&2; exit 1; }
fi

# 1. Tunnel — reuse only a tunnel that visibly targets the same remote socket.
if pgrep -f "ssh .*${PORT}:${REMOTE_SOCKET}.*${HOST}" >/dev/null 2>&1; then
    echo "port $PORT already forwards to $REMOTE_SOCKET — reusing existing tunnel"
elif port_open "$PORT"; then
    echo "ERROR: local port $PORT is already open, but not by a tunnel to $REMOTE_SOCKET" >&2
    echo "       stop the stale tunnel or set SOT_TCP_PORT to a free local port" >&2
    exit 1
else
    ssh -fN -o ServerAliveInterval=30 -o ExitOnForwardFailure=yes \
        -L "$PORT:$REMOTE_SOCKET" -L "$PLUTO_PORT:127.0.0.1:$PLUTO_PORT" -L "$VIDEO_PORT:127.0.0.1:$VIDEO_PORT" \
        -L "$DOCS_PORT:127.0.0.1:$DOCS_PORT" \
        -L "$((DOCS_PORT+1)):127.0.0.1:$((DOCS_PORT+1))" -L "$((DOCS_PORT+2)):127.0.0.1:$((DOCS_PORT+2))" \
        -L "$((DOCS_PORT+3)):127.0.0.1:$((DOCS_PORT+3))" -L "$((DOCS_PORT+4)):127.0.0.1:$((DOCS_PORT+4))" "$HOST" \
        || { echo "ERROR: could not open SSH tunnel to $HOST (stale tunnel holding ports? try: pkill -f 'ssh -fN.*$PORT')" >&2; exit 1; }
fi
ensure_aux_tunnel

# 2. Backend — ensure one is running on the host (don't disrupt a live session).
if [ "${SOT_RESTART_BE:-0}" = "1" ] || ! ssh "$HOST" "[ -S '$REMOTE_SOCKET' ]"; then
    # Delegate lifecycle details to the canonical restart script. It knows
    # whether the daemon is systemd-supervised and validates that the socket,
    # not a machine-wide TCP port, came up.
    # ADR 0030 dev-freshness rev 2: the launcher does NOT update the shared
    # daemon — it updates on its own cadence (the BE session's on-merge
    # deploy). SOT_RESTART_BE=1 remains the explicit force path below; the
    # staleness check after this block reports drift without acting on it.
    ssh "$HOST" "cd '$REMOTE_REPO' && scripts/restart-backend.sh" || true
    i=0
    while [ "$i" -lt 40 ]; do
        ssh "$HOST" "[ -S '$REMOTE_SOCKET' ]" && break
        sleep 0.25
        i=$((i+1))
    done
    ssh "$HOST" "[ -S '$REMOTE_SOCKET' ]" \
        || { echo "ERROR: remote backend did not create socket $REMOTE_SOCKET" >&2; exit 1; }
else
    # A running backend is intentionally left alone (don't disrupt a live REPL
    # session). But a normal launch won't load a backend FIX either, so warn
    # loudly when the running daemon PREDATES the built binary — that gap once
    # let a fix sit built-but-unloaded for ~17h (agents read "idle" when busy).
    # Staleness check is delegated to the canonical restart script (--check only
    # reports, never restarts; exit 3 = stale).
    if ! ssh "$HOST" "cd '$REMOTE_REPO' && scripts/restart-backend.sh --check" >/dev/null 2>&1; then
        echo "WARNING: remote backend is STALE — it predates the built binary; a normal launch will NOT load it." >&2
        echo "         load the latest: SOT_RESTART_BE=1 $(basename "$0")  (or on $HOST: scripts/restart-backend.sh)" >&2
    fi
    echo "remote backend already running — leaving it (SOT_RESTART_BE=1 to force restart)"
fi

# 3. Frontend freshness (ADR 0030 dev-freshness rev 2): pull + rebuild THIS
# machine's FE before launching. FAIL-OPEN — offline/dirty/broken-main warns
# and launches the existing binary. SOT_NO_UPDATE=1 skips.
if [ "${SOT_NO_UPDATE:-0}" != 1 ] && [ -d "$REPO/.git" ]; then
    if git -C "$REPO" pull --rebase --autostash >/dev/null 2>&1; then
        cargo build --release -p sot-frontend --manifest-path "$REPO/rust/Cargo.toml"             || echo "WARNING: frontend rebuild failed — launching existing binary" >&2
    else
        echo "WARNING: git pull failed (offline or dirty) — launching existing binary" >&2
    fi
fi

# 4. Frontend (blocks; GPU window).
exec "$REPO/rust/target/release/sot" --tcp "127.0.0.1:$PORT"
