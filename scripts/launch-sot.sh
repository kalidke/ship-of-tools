#!/usr/bin/env bash
# launch-sot.sh — Linux/macOS frontend client → remote backend over SSH.
#
# Opens an SSH tunnel to $SOT_HOST (forwarding a local TCP port to the
# remote user's per-user `sotd` socket — browser pages ride this control
# forward via the daemon proxy, ADR 0035; the legacy fixed helper forwards
# are opt-in via SOT_LEGACY_FORWARDS=1), ensures that remote `sotd` is
# running, then runs the local frontend pointed at the forwarded local port.
# The remote BE must already be BUILT on the host.
#
# ADR 0042 L2b design E: every OTHER `[host.<name>]` entry in
# `.sot/hosts.toml` that has an `ssh_alias` gets its OWN tunnel too (see
# `sot_ensure_remote_host` / scripts/sot-hosts.sh's `sot_tunnel_plan`
# below). $SOT_HOST/$PORT keep their exact pre-L2b meaning and are NOT
# read from hosts.toml (env-var driven, same as always); other hosts'
# ssh_alias/remote_repo/tcp_port come straight from their own
# [host.<name>] section, with tcp_port required.
#
# Codex follow-up (design 3): $SOT_HOST is NONFATAL now too, exactly like
# every other configured remote -- with the implicit local connection
# (rust/frontend's hosts::resolve_connections, cross-platform) always
# present, an unconfigured or unreachable default remote is no longer a
# reason to refuse to launch the frontend at all. $SOT_HOST/$SOT_REMOTE_REPO
# being unset just skips the primary host's own ensure+tunnel step; every
# failure past that point (unreachable, no socket, etc.) logs one line and
# falls through to the same `exec` at the bottom either way.
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
#
# `-e` (errexit) is deliberately NOT set here (unlike some sibling scripts):
# its behavior inside functions/conditionals is notoriously surprising, and
# it can arrive uninvited anyway (bash exports errexit to child processes
# via $SHELLOPTS when it's active in the parent, so `-e` can be inherited
# even though this script never sets it itself). Every nonfatal call this
# script makes is guarded explicitly (an `if !`/`then :` around it) so it
# survives that inheritance rather than depending on -e's absence.
set -uo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"

# --- Self-update prelude (ADR 0032 - launcher self-update gap, 2026-07-13) ---
# A running script is read through an fd pinned to the old inode, so a git pull
# that adds e.g. a new -L forward to THIS script does not affect the current run:
# the launch that pulls the change still opens the old port set (the 1241 WGL
# connection-refused incident). Fix: pull FIRST (before the socket query and any
# tunnel/FE side effect) and, if this script itself changed, exec the fresh copy.
# SOT_LAUNCH_REEXEC guards it to one hop; SOT_LAUNCH_REBUILD hands the one cargo
# build to the final exec. Fail-open: a failed/absent pull, or a pulled copy that
# fails `bash -n`, runs the current version. SOT_NO_UPDATE=1 skips it.
if [ "${SOT_NO_UPDATE:-0}" != 1 ] && [ -z "${SOT_LAUNCH_REEXEC:-}" ] && [ -d "$REPO/.git" ]; then
    self_rel="scripts/launch-sot.sh"
    before="$(git -C "$REPO" rev-parse "HEAD:$self_rel" 2>/dev/null || true)"
    if git -C "$REPO" pull --rebase --autostash >/dev/null 2>&1; then
        export SOT_LAUNCH_REBUILD=1   # pull ok -> the final exec builds once
        after="$(git -C "$REPO" rev-parse "HEAD:$self_rel" 2>/dev/null || true)"
        if [ -n "$after" ] && [ -n "$before" ] && [ "$after" != "$before" ]; then
            if bash -n "${BASH_SOURCE[0]}" 2>/dev/null; then
                echo "self-update: launcher changed - re-exec fresh copy"
                export SOT_LAUNCH_REEXEC=1
                exec "$BASH" "${BASH_SOURCE[0]}" "$@"
            else
                echo "self-update: pulled launcher failed bash -n - staying on current copy" >&2
            fi
        fi
    else
        echo "WARNING: git pull failed (offline or dirty) - launching current version" >&2
    fi
fi
# Guard has served its purpose; do not leak it to the FE or an exit-75 relaunch.
unset SOT_LAUNCH_REEXEC || true
# --- end self-update prelude ---

# Codex follow-up (design 3): no longer a hard `${VAR:?...}` requirement --
# an unset SOT_HOST/SOT_REMOTE_REPO just means "no default remote", which is
# now a normal, nonfatal state (see the header). HOST/REMOTE_REPO stay empty
# in that case; every call site below checks for that instead of relying on
# a startup abort.
HOST="${SOT_HOST:-}"
PORT="${SOT_TCP_PORT:-18743}"
REMOTE_REPO="${SOT_REMOTE_REPO:-}"
REMOTE_SOCKET="${SOT_REMOTE_SOCKET:-}"
PLUTO_PORT="${SOT_PLUTO_PORT:-1234}"
VIDEO_PORT="${SOT_VIDEO_PORT:-1235}"
DOCS_PORT="${SOT_DOCS_PORT:-1236}"
# WGLMakie/Bonito interactive figures (ADR 0032). 1237-1240 are the docs pool
# (site_serve), so WGL sits at 1241 — the first free port above the daemon range.
WGL_PORT="${SOT_WGL_PORT:-1241}"
AUX_PORTS=("$PLUTO_PORT" "$VIDEO_PORT" "$DOCS_PORT" "$((DOCS_PORT+1))" "$((DOCS_PORT+2))" "$((DOCS_PORT+3))" "$((DOCS_PORT+4))" "$WGL_PORT")

port_open() {
    if (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; then exec 3>&-; return 0; fi
    command -v nc >/dev/null 2>&1 && nc -z 127.0.0.1 "$1" >/dev/null 2>&1
}

# sot_ssh_bounded <ssh-args...>
# The ssh options every remote step uses now (codex follow-up, item 5,
# trimmed): BatchMode=yes (never prompt for a password/passphrase -- that
# HANGS, not fails, on a misconfigured host) and ConnectionAttempts=1 (no
# silent retries) join the existing ConnectTimeout=10, default host
# included. No separate per-host deadline machinery beyond that -- a
# wedged remote command past the handshake is accepted as today's existing
# risk, not one this slice takes on.
sot_ssh_bounded() {
    ssh -o ConnectTimeout=10 -o BatchMode=yes -o ConnectionAttempts=1 "$@"
}

ensure_aux_tunnel() {
    # Retired by default (ADR 0035) — see the SOT_LEGACY_FORWARDS note at the
    # main tunnel. Without the opt-in there is nothing to top up: backend pages
    # ride the control tunnel through the verified-bound daemon proxy, and
    # forwarding a fixed port we cannot prove is ours is the failure this
    # retirement exists to prevent.
    if [ -z "${SOT_LEGACY_FORWARDS:-}" ]; then
        return 0
    fi
    local missing=()
    local p
    for p in "${AUX_PORTS[@]}"; do
        port_open "$p" || missing+=("$p")
    done
    if [ "${#missing[@]}" -eq 0 ]; then
        echo "browser aux ports already forwarded (${AUX_PORTS[*]})"
        return 0
    fi
    # Forward ONLY the missing ports (ADR 0032 launcher self-update gap). An old
    # `ssh -fN` aux tunnel OUTLIVES the FE window, so after a new port is added
    # (e.g. WGL 1241) a prior launch's tunnel covers 1234-1240 but not 1241.
    # Opening a SUPPLEMENTARY tunnel for just the missing ports repairs that
    # without the old hard-abort and without killing the live tunnel that also
    # carries the control forward. (The full fix - the FE forwarding on demand -
    # is ADR 0032's port-pool follow-up, PR #10.)
    if [ "${#missing[@]}" -ne "${#AUX_PORTS[@]}" ]; then
        echo "browser aux: forwarding missing ports only: ${missing[*]}"
    fi
    local fwd=()
    for p in "${missing[@]}"; do
        fwd+=(-L "$p:127.0.0.1:$p")
    done
    sot_ssh_bounded -fN -o ServerAliveInterval=30 -o ExitOnForwardFailure=yes \
        "${fwd[@]}" "$HOST" \
        || { echo "ERROR: could not open browser aux SSH tunnel to $HOST (missing: ${missing[*]})" >&2; exit 1; }
}

# sot_ensure_remote_host <name> <ssh_alias> <remote_repo> <port> <remote_socket-override-or-empty>
# The ONE ensure+resolve+tunnel plan every host uses now (codex follow-up,
# item 3): resolve the remote socket, ensure the backend is up (honoring
# SOT_RESTART_BE / warning when stale, exactly as the default host always
# did -- UNCONDITIONALLY, even when an existing tunnel is about to be
# reused, matching the default host's original behavior: a stale-backend
# warning is worth seeing on every launch, not just a cold start), then
# open (or reuse) the tunnel. Every failure is NONFATAL: one log line and
# `return 1` -- the caller decides what that means for it.
#
# <remote_repo> is OPTIONAL (item 2 follow-up): empty means "no local
# knowledge of the remote's checkout" -- the case for install.sh's
# generated launcher, a client-only install with no source tree of its
# own, connecting to a remote it may not even have SSH'd into before.
# Without a repo there is no `scripts/restart-backend.sh` to `cd` into, so
# backend management is skipped entirely (this host's backend is assumed
# to manage itself, e.g. its own systemd unit) and the socket is queried
# straight from the remote's well-known INSTALLED path -- exactly what the
# old heredoc this replaces did, and the only thing it did.
sot_ensure_remote_host() {
    local name="$1" alias="$2" repo="$3" port="$4" remote_socket="$5"
    if [ -z "$remote_socket" ]; then
        if [ -n "$repo" ]; then
            # Dev checkout first, then a release install's staged sotd on
            # the remote -- either way, this remote has a checkout at
            # $repo we know about.
            remote_socket="$(sot_ssh_bounded "$alias" "cd '$repo' && ./rust/target/release/sotd session-socket-path sot 2>/dev/null || \${SOT_REMOTE_SOTD:-\$HOME/.local/share/sot/bin/sotd} session-socket-path sot")" \
                || { echo "tunnel: host '$name' unreachable (could not query sotd socket path)" >&2; return 1; }
        else
            remote_socket="$(sot_ssh_bounded "$alias" '${SOT_REMOTE_SOTD:-$HOME/.local/share/sot/bin/sotd} session-socket-path sot')" \
                || { echo "tunnel: host '$name' unreachable (could not query sotd socket path)" >&2; return 1; }
        fi
    fi
    if [ -z "$remote_socket" ]; then
        echo "tunnel: host '$name' did not report a socket path" >&2
        return 1
    fi

    # Backend -- ensure one is running (don't disrupt a live session), ONLY
    # when a checkout is known (see the repo-optional note above).
    # SOT_RESTART_BE=1 forces a restart; otherwise an already-up backend is
    # left alone (with a staleness warning if it predates the built
    # binary), and a down one is started and waited for.
    if [ -z "$repo" ]; then
        :   # no checkout -- this remote manages its own backend
    elif [ "${SOT_RESTART_BE:-0}" = "1" ]; then
        if sot_ssh_bounded "$alias" "cd '$repo' && scripts/restart-backend.sh"; then
            echo "tunnel: host '$name' backend force-restarted at current build"
        else
            echo "tunnel: host '$name' backend force-restart FAILED" >&2
        fi
    elif sot_ssh_bounded "$alias" "[ -S '$remote_socket' ]" 2>/dev/null; then
        if ! sot_ssh_bounded "$alias" "cd '$repo' && scripts/restart-backend.sh --check" >/dev/null 2>&1; then
            echo "tunnel: host '$name' backend is STALE -- it updates on its own cadence -- force with SOT_RESTART_BE=1" >&2
        fi
    else
        sot_ssh_bounded "$alias" "cd '$repo' && scripts/restart-backend.sh" >/dev/null 2>&1 || true
        local i=0
        while [ "$i" -lt 40 ]; do
            sot_ssh_bounded "$alias" "[ -S '$remote_socket' ]" 2>/dev/null && break
            sleep 0.25
            i=$((i+1))
        done
        if ! sot_ssh_bounded "$alias" "[ -S '$remote_socket' ]" 2>/dev/null; then
            echo "tunnel: host '$name' backend did not create socket $remote_socket" >&2
            return 1
        fi
    fi

    # Reuse only a tunnel that visibly targets the same remote socket.
    if pgrep -f "ssh .*${port}:${remote_socket}.*${alias}" >/dev/null 2>&1; then
        echo "tunnel: host '$name' port $port already forwards to $remote_socket -- reusing"
        return 0
    fi
    if port_open "$port"; then
        echo "tunnel: skipping host '$name' -- local port $port is already open but not by its tunnel" >&2
        return 1
    fi
    sot_ssh_bounded -fN -o ServerAliveInterval=30 -o ExitOnForwardFailure=yes \
        -L "$port:$remote_socket" "$alias" \
        || { echo "tunnel: host '$name' could not open SSH tunnel" >&2; return 1; }
    echo "tunnel: host '$name' forwarding 127.0.0.1:$port -> $remote_socket"
}

# shellcheck source=sot-hosts.sh
. "$(dirname "$0")/sot-hosts.sh"
# $REPO/.sot/hosts.toml first (a dev checkout's own, machine-local, git-
# ignored config) -- but an install-layout checkout at repo/current (item 2
# follow-up: install.sh's generated launcher delegates here and has no
# .sot/hosts.toml of its own, only $REPO/scripts) has none, so fall back to
# the same XDG config path the Rust frontend already checks on its own
# (rust/frontend/src/hosts.rs's own candidate list) and install.sh itself
# writes to.
if [ -f "$REPO/.sot/hosts.toml" ]; then
    HOSTS_TOML="$REPO/.sot/hosts.toml"
else
    HOSTS_TOML="${XDG_CONFIG_HOME:-$HOME/.config}/sot/hosts.toml"
fi

# Codex follow-up, item 7: host identity is the hosts.toml KEY, not the ssh
# alias -- nothing stops two different hosts.toml entries from sharing one
# ssh_alias. DEFAULT_HOST_KEY is used ONLY to (a) skip the primary host's
# entry in the extra-hosts loop below, so it is never double-tunneled, and
# (b) let sot_tunnel_plan apply the SOT_TCP_PORT/18743 compatibility
# fallback to the right row. It does NOT drive $HOST/$PORT themselves,
# which stay purely env-var driven, same as always.
DEFAULT_HOST_KEY="$(sot_hosts_default_host "$HOSTS_TOML")"

# 1-2. Default remote: ensure it, same nonfatal plan as every other host
# (codex follow-up, item 3). $HOST empty (SOT_HOST never set) just skips
# this entirely; every OTHER failure logs and falls through. Guarded
# against inherited errexit (item 13): a bare nonfatal call would abort
# under `-e` even though this script never sets it itself.
#
# REMOTE_REPO is NOT required here (item 2 follow-up): gating on it too
# would skip a caller that only knows $HOST -- exactly install.sh's
# generated remote-role launcher, which has no local knowledge of the
# remote's checkout and relies on sot_ensure_remote_host's repo-optional
# path (see its own doc comment) to query the remote's installed sotd
# directly instead.
if [ -n "$HOST" ]; then
    default_remote_ok=1
    sot_ensure_remote_host "default" "$HOST" "$REMOTE_REPO" "$PORT" "$REMOTE_SOCKET" || default_remote_ok=0
    # Only worth trying the (opt-in, legacy) aux forwards to a host we just
    # confirmed we can reach -- otherwise this would hard-exit the script
    # (ensure_aux_tunnel's own failure path is NOT nonfatal) for a host
    # sot_ensure_remote_host already logged as unreachable, undoing the
    # nonfatal treatment above for anyone with SOT_LEGACY_FORWARDS set.
    if [ "$default_remote_ok" = 1 ]; then
        ensure_aux_tunnel
    fi
else
    echo "default remote: not configured (set SOT_HOST) - continuing without one"
fi

# 2b. Every OTHER configured remote gets its own tunnel too (ADR 0042 L2b
# design E) — $HOST's own tunnel above is untouched. sot_tunnel_plan (pure,
# no ssh) enumerates hosts.toml; sot_ensure_remote_host (above) does the
# ensure+resolve+open sequence, nonfatal: any failure logs one line and
# moves on to the next host instead of exiting the whole launch. The
# frontend reads hosts.toml itself and shows an unreachable host as
# unreachable — that is the intended failure mode here, not a launch abort.
while IFS='|' read -r t_name t_alias t_port t_repo t_socket t_err; do
    [ -n "$t_name" ] || continue
    [ "$t_name" = "$DEFAULT_HOST_KEY" ] && continue   # the default host's tunnel is step 1-2 above
    if [ -n "$t_err" ]; then
        echo "tunnel: skipping host '$t_name' -- $t_err" >&2
        continue
    fi
    if ! sot_ensure_remote_host "$t_name" "$t_alias" "$t_repo" "$t_port" "$t_socket"; then
        :
    fi
done <<EOF
$(sot_tunnel_plan "$HOSTS_TOML" "$DEFAULT_HOST_KEY" "$PORT")
EOF

# 3. Frontend rebuild (ADR 0030 dev-freshness rev 2). The git pull moved to the
# self-update prelude at the top; here we only REBUILD, and only when that pull
# succeeded (SOT_LAUNCH_REBUILD) so exactly one build runs in the final exec.
# FAIL-OPEN: a broken build warns and launches the existing binary.
if [ "${SOT_LAUNCH_REBUILD:-0}" = 1 ] && [ "${SOT_NO_UPDATE:-0}" != 1 ]; then
    unset SOT_LAUNCH_REBUILD || true
    cargo build --release -p sot-frontend --manifest-path "$REPO/rust/Cargo.toml" \
        || echo "WARNING: frontend rebuild failed - launching existing binary" >&2
fi

# 4. Frontend (blocks; GPU window). Always runs -- the implicit local
# connection (rust/frontend's hosts::resolve_connections) and/or any
# successfully tunneled remote is what the frontend actually has to show;
# --tcp here is only the DEFAULT host's endpoint (ADR 0042 L2a semantics),
# reachable or not.
#
# SOT_FRONTEND_BIN (item 2 follow-up): the dev-checkout path is the
# default, unchanged; install.sh's generated launcher sets this to its own
# staged $PREFIX/bin/sot when it delegates here, since repo/current (a
# pinned release checkout, not necessarily built) has no
# rust/target/release of its own.
exec "${SOT_FRONTEND_BIN:-$REPO/rust/target/release/sot}" --tcp "127.0.0.1:$PORT"
