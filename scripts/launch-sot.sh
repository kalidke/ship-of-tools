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
# ADR 0042 L2b design E, topology plan (lane D): every OTHER dialable host
# `sotd topology plan [--self <host>]` names gets its OWN tunnel too (see
# `sot_ensure_remote_host` / scripts/sot-hosts.sh's `sot_topology_plan`
# below). $SOT_HOST/$PORT keep their exact pre-L2b meaning by default
# (env-var driven), falling back to the plan's declared `hub` when unset;
# other hosts' ports come straight from the plan's `tunnel` lines. A
# remote's `sotd` is always `systemctl --user start/restart` (never
# started by path) and its socket always queried
# (`sotd session-socket-path sot` on the remote) -- there is no more
# per-host ssh_alias/remote_repo/tcp_port/remote_socket to configure.
#
# Codex follow-up (design 3): $SOT_HOST is NONFATAL now too, exactly like
# every other configured remote -- with the frontend's own `--socket` for
# its local daemon (when one is up) usually present, an unconfigured or
# unreachable default remote is no longer a reason to refuse to launch the
# frontend at all. $SOT_HOST being unset just skips the primary host's own
# ensure+tunnel step; every failure past that point (unreachable, no
# socket, etc.) logs one line and falls through to the same `exec` at the
# bottom either way.
#
# Idempotent: an `ssh -fN` tunnel is backgrounded and OUTLIVES the FE window, so
# a naive re-run would collide on the forwarded ports (Address already in use)
# and — under `set -e` — abort before launching the FE. We therefore reuse an
# existing tunnel instead of opening a second one, and only (re)spawn the backend
# when it isn't already up.
#
# Overridable via env: SOT_HOST (or SOT_HOST_NAME), SOT_TCP_PORT,
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
# an unset SOT_HOST/no declared hub just means "no default remote", which
# is now a normal, nonfatal state (see the header). HOST stays empty in
# that case; every call site below checks for that instead of relying on
# a startup abort.
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

# sot_ensure_remote_host <name> <ssh_alias> <port>
# The ONE ensure+resolve+tunnel plan every host uses now (codex follow-up,
# item 3; topology plan, lane D): `systemctl --user start/restart sotd` on
# the remote (SOT_RESTART_BE=1 forces a restart; otherwise an already-up
# backend is left alone, a down one is started and waited for), query its
# socket path (`sotd session-socket-path sot`, always, never configured --
# remote_repo/remote_socket are gone: this launcher never starts a remote
# daemon by path any more), then open (or reuse) the tunnel. Every failure
# is NONFATAL: one log line and `return 1` -- the caller decides what that
# means for it.
sot_ensure_remote_host() {
    local name="$1" alias="$2" port="$3"
    # export PATH first: a non-interactive ssh command's PATH doesn't
    # always carry ~/.local/bin (matching launch-sot.ps1's own remote
    # command, same reason).
    local remote_path_prelude='export PATH="$HOME/.local/share/sot/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH";'
    if [ "${SOT_RESTART_BE:-0}" = "1" ]; then
        if sot_ssh_bounded "$alias" "$remote_path_prelude systemctl --user restart sotd.service"; then
            echo "tunnel: host '$name' backend force-restarted via systemd"
        else
            echo "tunnel: host '$name' backend force-restart FAILED" >&2
        fi
    elif sot_ssh_bounded "$alias" "$remote_path_prelude systemctl --user is-active --quiet sotd.service" 2>/dev/null; then
        : # already running -- left alone
    else
        sot_ssh_bounded "$alias" "$remote_path_prelude systemctl --user reset-failed sotd.service 2>/dev/null; systemctl --user start sotd.service" \
            || echo "tunnel: host '$name' could not start sotd via systemd" >&2
    fi
    local remote_socket i=0
    while [ "$i" -lt 40 ]; do
        remote_socket="$(sot_ssh_bounded "$alias" "$remote_path_prelude sotd session-socket-path sot" 2>/dev/null)"
        [ -n "$remote_socket" ] && sot_ssh_bounded "$alias" "[ -S '$remote_socket' ]" 2>/dev/null && break
        sleep 0.25
        i=$((i+1))
    done
    if [ -z "$remote_socket" ]; then
        echo "tunnel: host '$name' unreachable (could not query sotd socket path)" >&2
        return 1
    fi
    if ! sot_ssh_bounded "$alias" "[ -S '$remote_socket' ]" 2>/dev/null; then
        echo "tunnel: host '$name' backend did not create socket $remote_socket" >&2
        return 1
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

# resolve_local_sotd_bin: dev build first, then a release install's staged
# sotd -- either way, a LOCAL binary this box can run `topology plan`
# with. Empty when neither exists (a fresh checkout with nothing built
# yet), which read_topology_plan below treats as "no plan yet", same as
# an absent hosts.toml always was.
resolve_local_sotd_bin() {
    if [ -x "$REPO/rust/target/release/sotd" ]; then
        printf '%s\n' "$REPO/rust/target/release/sotd"
    elif [ -x "$HOME/.local/share/sot/bin/sotd" ]; then
        printf '%s\n' "$HOME/.local/share/sot/bin/sotd"
    fi
}

# read_topology_plan: (re)resolves the local sotd binary, SYNCS, then
# (re)runs sot_topology_plan into $PLAN. Called once, early (steps 1-2b
# below need it), and AGAIN after the freshness rebuild (step 3) --
# ordering risk (manager review): a brand-new box has no sotd built yet at
# the first call, so an early-only read would leave the frontend's --dial
# args permanently empty on the very first launch. The second call, right
# before the frontend actually launches, picks up a binary the rebuild
# below may have just produced -- a fresh box needs exactly one launch,
# not two.
#
# Sync BEFORE plan, never the other way around (matches
# launch-sot.ps1's Update-SotTopologyPlan): `plan` needs this box listed
# in the hosts.toml it reads, and the one file that CANNOT list this box
# is exactly the stale/never-synced copy the self-heal exists to replace
# -- gating the sync on a successful plan starves it of the one thing it
# exists to fix. The hub is the env override when set, else `sotd
# topology sync` derives it from whatever hub the LOCAL copy already
# names -- no plan round-trip needed to learn it.
read_topology_plan() {
    SOTD_BIN="$(resolve_local_sotd_bin)"
    if [ -n "$SOTD_BIN" ]; then
        local sync_hub="${SOT_HOST_NAME:-${SOT_HOST:-}}" sync_out
        if sync_out="$(sot_topology_sync "$SOTD_BIN" "$sync_hub")"; then
            [ -n "$sync_out" ] && echo "topology sync: $sync_out"
        else
            # No hint appended here: sotd's own message already names the
            # fix (e.g. "... pass --hub <alias>") when there is one.
            echo "topology sync failed: $sync_out" >&2
        fi
    fi
    if PLAN="$(sot_topology_plan "$SOTD_BIN")"; then
        :
    else
        PLAN=""
        echo "topology: ${SOT_TOPOLOGY_PLAN_ERR:-no plan available yet (no sotd binary built)} - continuing with no declared hosts" >&2
    fi
}
read_topology_plan

# $SOT_HOST_NAME/$SOT_HOST still override which declared host is the
# PRIMARY (the one that gets the opt-in legacy aux forwards); default the
# plan's declared hub. $PORT defaults to that host's own ordinal port from
# the plan's `tunnel` lines, falling back to 18743 for a box with no plan
# at all.
HOST="${SOT_HOST_NAME:-${SOT_HOST:-$(sot_topology_field "$PLAN" HUB)}}"
PORT="${SOT_TCP_PORT:-$(printf '%s\n' "$PLAN" | awk -F'|' -v h="$HOST" '$1=="TUNNEL" && $2==h {print $3}')}"
PORT="${PORT:-18743}"

# 1-2. Default remote: ensure it, same nonfatal plan as every other host
# (codex follow-up, item 3). $HOST empty (no SOT_HOST/SOT_HOST_NAME and no
# declared hub) just skips this entirely; every OTHER failure logs and
# falls through. Guarded against inherited errexit (item 13): a bare
# nonfatal call would abort under `-e` even though this script never sets
# it itself.
if [ -n "$HOST" ]; then
    default_remote_ok=1
    sot_ensure_remote_host "default" "$HOST" "$PORT" || default_remote_ok=0
    # Only worth trying the (opt-in, legacy) aux forwards to a host we just
    # confirmed we can reach -- otherwise this would hard-exit the script
    # (ensure_aux_tunnel's own failure path is NOT nonfatal) for a host
    # sot_ensure_remote_host already logged as unreachable, undoing the
    # nonfatal treatment above for anyone with SOT_LEGACY_FORWARDS set.
    if [ "$default_remote_ok" = 1 ]; then
        ensure_aux_tunnel
    fi
else
    echo "default remote: no declared hub and SOT_HOST/SOT_HOST_NAME unset - continuing without one"
fi

# 2b. Every OTHER dialable host in the plan gets its own tunnel too (ADR
# 0042 L2b design E) — $HOST's own tunnel above is untouched. Topology
# plan already excludes self and frontend hosts from `tunnel` lines (D8),
# so nothing here needs its own filter beyond skipping the primary.
# sot_ensure_remote_host does the ensure+resolve+open sequence, nonfatal:
# any failure logs one line and moves on to the next host instead of
# exiting the whole launch. The frontend's own --dial list (built from the
# SAME plan below, independent of whether the tunnel actually came up)
# shows an unreachable host as unreachable — that is the intended failure
# mode here, not a launch abort.
while IFS='|' read -r t_tag t_name t_port; do
    [ "$t_tag" = "TUNNEL" ] || continue
    [ "$t_name" = "$HOST" ] && continue   # the primary host's tunnel is step 1-2 above
    sot_ensure_remote_host "$t_name" "$t_name" "$t_port" || :
done <<EOF
$PLAN
EOF

# 3. Frontend + backend-pair rebuild (ADR 0030 dev-freshness rev 2). The
# git pull moved to the self-update prelude at the top; here we only
# REBUILD, and only when that pull succeeded (SOT_LAUNCH_REBUILD) so
# exactly one build runs in the final exec. sot-backend is built alongside
# sot-frontend now (ordering risk, above): it's the only source of a local
# `sotd` for read_topology_plan's re-read below, on a box with no release
# install. FAIL-OPEN: a broken build warns and launches with whatever
# plan/binary already existed.
if [ "${SOT_LAUNCH_REBUILD:-0}" = 1 ] && [ "${SOT_NO_UPDATE:-0}" != 1 ]; then
    unset SOT_LAUNCH_REBUILD || true
    cargo build --release -p sot-frontend -p sot-backend --manifest-path "$REPO/rust/Cargo.toml" \
        || echo "WARNING: frontend/backend rebuild failed - launching with the existing binaries" >&2
fi
read_topology_plan

# 4. Frontend (blocks; GPU window). Always runs -- one --dial per plan.Dials
# entry (this box's own local daemon, if it's ALSO a declared daemon host,
# plus every other dialable host), passed UNCONDITIONALLY: a tunnel that
# didn't come up just means the frontend shows that host unreachable and
# keeps retrying, never a reason to hold an arg back. No plan at all (no
# sotd binary anywhere) means no --dial args -- the frontend reports that
# plainly and runs offline, same as a box with no hosts.toml always did.
#
# SOT_FRONTEND_BIN (item 2 follow-up): the dev-checkout path is the
# default, unchanged; install.sh's generated launcher sets this to its own
# staged $PREFIX/bin/sot when it delegates here, since repo/current (a
# pinned release checkout, not necessarily built) has no
# rust/target/release of its own.
dial_args=()
while IFS='|' read -r d_tag d_host d_endpoint; do
    [ "$d_tag" = "DIAL" ] || continue
    dial_args+=(--dial "$d_host=$d_endpoint")
done <<EOF
$PLAN
EOF
exec "${SOT_FRONTEND_BIN:-$REPO/rust/target/release/sot}" "${dial_args[@]}"
