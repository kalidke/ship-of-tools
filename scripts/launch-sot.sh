#!/usr/bin/env bash
# launch-sot.sh — Linux/macOS frontend client → remote backend over SSH.
#
# Reaching a daemon that is not on this box means the frontend itself
# spawning an ssh child (C3, isolation-plan.md §3, amended by
# dev/output/c3-second-connection-amendment.md) and speaking the protocol
# over its stdio — never a port, on either end. This launcher's own job is
# reduced to reading the topology plan and handing every declared host to
# the frontend as a `--dial <host>=<endpoint>` argument; it opens nothing
# on any remote's behalf and starts no remote daemon that is down (an
# enrolled box runs `sotd` as a systemd service). The remote BE must
# already be BUILT on the host.
#
# ADR 0042 L2b design E, topology plan (lane D): `sotd topology plan
# [--self <host>]` (scripts/sot-hosts.sh's `sot_topology_plan`) is read
# once early and again right before the frontend launches, so a box whose
# `sotd` was only just built by this same launch still gets its dial set.
# $SOT_HOST_NAME/$SOT_HOST are read only as a hint for which hub `sotd
# topology sync` targets when this box's own hosts.toml copy needs it —
# they no longer name a "primary remote" with its own ensure/tunnel step.
#
# Overridable via env: SOT_HOST (or SOT_HOST_NAME) — the sync-hub hint
# above; SOT_RESTART_BE has no meaning left here (it named the primary
# remote's own force-restart, now gone with the rest of that step).
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

# shellcheck source=lib/sot-hosts.sh
. "$(dirname "$0")/lib/sot-hosts.sh"
# shellcheck source=lib/sot-daemon.sh
. "$(dirname "$0")/lib/sot-daemon.sh"

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

# 1-2b. Reaching a host that is not this box's own daemon means spawning
# an ssh child (C3, isolation-plan.md §3) -- there is nothing left for
# THIS launcher to ensure, probe, or forward before the frontend starts.
# Every declared host, primary or not, is just a `DIAL` line the plan
# already carries; `dial_args` below turns each into `--dial <host>=
# <endpoint>` unconditionally, and the frontend's own ssh child reports an
# unreachable one the loud way (its last stderr line), never a silent
# hang.

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
# plus every other dialable host), passed UNCONDITIONALLY: an ssh child
# that fails to connect just means the frontend shows that host
# unreachable and keeps retrying, never a reason to hold an arg back. No
# plan at all (no sotd binary anywhere) means no --dial args -- the
# frontend reports that plainly and runs offline, same as a box with no
# hosts.toml always did.
#
# SOT_FRONTEND_BIN (item 2 follow-up): the dev-checkout path is the
# default, unchanged; install.sh's generated launcher sets this to its own
# staged $PREFIX/bin/sot when it delegates here, since repo/current (a
# pinned release checkout, not necessarily built) has no
# rust/target/release of its own.
dial_args=()
local_sock=""
self_host="$(sot_topology_field "$PLAN" SELF)"
while IFS='|' read -r d_tag d_host d_endpoint; do
    [ "$d_tag" = "DIAL" ] || continue
    dial_args+=(--dial "$d_host=$d_endpoint")
    # This computer's own backend: the plan's self dial line is the socket the
    # frontend dials, so it is the one to ensure.
    case "$d_endpoint" in
        unix:*) [ "$d_host" != "$self_host" ] || local_sock="${d_endpoint#unix:}" ;;
    esac
done <<EOF
$PLAN
EOF
if [ -n "$local_sock" ]; then
    sot_daemon_ensure "${SOT_PREFIX:-$HOME/.local/share/sot}" "$SOTD_BIN" "$local_sock" \
        || echo "WARNING: this computer's backend did not start; the window will show it unreachable" >&2
fi
# Finding 3b (v0.6.5 macOS field report): bash 3.2 (macOS's stock
# /bin/bash, still there under set -u) treats an EMPTY array's
# "${arr[@]}" expansion as an unset variable and aborts -- the
# portable ${arr[@]+"${arr[@]}"} form below only expands the array
# when it has at least one element, and is a no-op otherwise (a
# hosts.toml with no usable plan means zero --dial args, which is a
# normal, supported "run offline" case, not an error).
exec "${SOT_FRONTEND_BIN:-$REPO/rust/target/release/sot}" ${dial_args[@]+"${dial_args[@]}"}
