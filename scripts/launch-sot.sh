#!/usr/bin/env bash
# launch-sot.sh — Linux/macOS frontend client → remote backend over SSH.
#
# Opens an SSH tunnel to the backend host (forwarding the backend TCP port plus
# Pluto 1234 / video 1235 / docs 1236), ensures the remote `sotd` is running, then
# runs the local frontend pointed at the forwarded port. The remote BE must
# already be BUILT on the host.
#
# Idempotent: an `ssh -fN` tunnel is backgrounded and OUTLIVES the FE window, so
# a naive re-run would collide on the forwarded ports (Address already in use)
# and — under `set -e` — abort before launching the FE. We therefore reuse an
# existing tunnel instead of opening a second one, and only (re)spawn the backend
# when it isn't already up.
#
# Overridable via env: SOT_HOST, SOT_TCP_PORT, SOT_REMOTE_REPO,
# SOT_RESTART_BE=1 (force a backend restart even if one is running).
set -uo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
: "${SOT_HOST:?set SOT_HOST or configure .sot/hosts.toml}"
: "${SOT_REMOTE_REPO:?set SOT_REMOTE_REPO or configure .sot/hosts.toml}"
HOST="$SOT_HOST"
PORT="${SOT_TCP_PORT:-18743}"
REMOTE_REPO="$SOT_REMOTE_REPO"

port_open() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null && exec 3>&-; }

# 1. Tunnel — reuse if the backend port is already forwarded, else open one.
if port_open "$PORT"; then
    echo "port $PORT already forwarded — reusing existing tunnel"
else
    DOCS_PORT="${SOT_DOCS_PORT:-1236}"
    ssh -fN -o ServerAliveInterval=30 -o ExitOnForwardFailure=yes \
        -L "$PORT:127.0.0.1:$PORT" -L 1234:127.0.0.1:1234 -L 1235:127.0.0.1:1235 \
        -L "$DOCS_PORT:127.0.0.1:$DOCS_PORT" \
        -L "$((DOCS_PORT+1)):127.0.0.1:$((DOCS_PORT+1))" -L "$((DOCS_PORT+2)):127.0.0.1:$((DOCS_PORT+2))" \
        -L "$((DOCS_PORT+3)):127.0.0.1:$((DOCS_PORT+3))" -L "$((DOCS_PORT+4)):127.0.0.1:$((DOCS_PORT+4))" "$HOST" \
        || { echo "ERROR: could not open SSH tunnel to $HOST (stale tunnel holding ports? try: pkill -f 'ssh -fN.*$PORT')" >&2; exit 1; }
fi

# 2. Backend — ensure one is running on the host (don't disrupt a live session).
if [ "${SOT_RESTART_BE:-0}" = "1" ] || ! ssh "$HOST" 'pgrep -x sotd >/dev/null 2>&1'; then
    # If sotd is systemd-supervised (sotd.service), DEFER to systemd — a raw
    # pkill+nohup here races its Restart=always and spawns a SECOND sotd that fights
    # for :$PORT/:1236 (the dual-daemon incident, 2026-06-30: a stale nohup squatted
    # the ports — its default label was the repo basename 'ship-of-tools', a bogus
    # extra workspace row — while systemd restart-looped). `systemctl restart` keeps
    # a single managed instance. Else fall back to the legacy pkill-wait+nohup
    # (ADR 0029: wait for the old sotd to release its ports — a fixed sleep lost the
    # bind race), then SIGKILL a holdout.
    # ADR 0030 dev-freshness rev 2: the launcher does NOT update the shared
    # daemon — it updates on its own cadence (the BE session's on-merge
    # deploy). SOT_RESTART_BE=1 remains the explicit force path below; the
    # staleness check after this block reports drift without acting on it.
    ssh "$HOST" "if systemctl --user is-enabled sotd.service >/dev/null 2>&1; then systemctl --user reset-failed sotd.service 2>/dev/null || true; systemctl --user restart sotd.service; else pkill -x sotd 2>/dev/null; for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do pgrep -x sotd >/dev/null 2>&1 || break; sleep 0.3; done; pgrep -x sotd >/dev/null 2>&1 && { pkill -9 -x sotd 2>/dev/null; sleep 0.5; }; cd '$REMOTE_REPO' && nohup ./rust/target/release/sotd --tcp 127.0.0.1:$PORT --project-root '$REMOTE_REPO' --label sot >/tmp/sotd.log 2>&1 </dev/null & disown; fi" || true
    for _ in $(seq 1 40); do port_open "$PORT" && break; sleep 0.25; done
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
