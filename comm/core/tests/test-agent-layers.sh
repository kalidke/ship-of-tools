#!/usr/bin/env bash
# test-agent-layers.sh — a process acts as a comm handle only if at most one
# agent lies between it and its row's capsule (or the top of its process tree
# outside a row). A second agent started inside a session (`codex exec`,
# `claude -p`), and anything it starts, has no comm identity: it never reads
# the inbox, sends, joins, stamps, blocks the Stop hook or refreshes the
# heartbeat as the row's handle.
#
#   1. TABLE: _sot_agent_layers counts agent layers in a process chain
#      (comm-lib.sh), over the shapes the rule has to get right.
#   2. END TO END: fake processes, named by argv[0] only (`exec -a`), stand in
#      for a row's capsule and agents, so the result does not depend on what
#      runs this suite. Own first (one agent, no agent, an npm codex that is two
#      processes), then the child (a second agent between the first and the
#      tool shell).
#
# A process whose identity names a row (a self file `<host>__<id>.txt`, or
# SOT_WORKSPACE_ID with no self file) must also reach THAT row's capsule: the top,
# another row's capsule and a capsule with an unreadable command line are refused
# (rc 2); a process that names no row passes at the top and at any capsule. The
# row cases run beneath `in_row` (lib-home-guard.sh), a capsule stand-in.
#
# HERMETIC: a temp $SOT_COMM_HOME, a pinned $SOT_COMM_TEST_HOST, per-handle
# $SOT_COMM_SELF_FILEs — never the real ~/.sot-comm.
#
# Usage: comm/core/tests/test-agent-layers.sh
# Exit: 0 if every case PASSes; 1 if any FAILs. On a host that is not Linux the cases that
# read the live process table (the badawk case, section 2 on, SIMWIN) are skipped, with one SKIP line.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home
# The end-to-end chains are read from Linux's /proc with the Windows walk off, and SIMWIN
# builds its Windows stand-in from Linux's /proc. On Windows the walk would climb past the
# stand-ins into the live native process tree through sotd.exe, which lib-home-guard.sh hides
# on purpose; the tables and bridge fixtures below read no live process table and run anywhere.
LINUX=1; [ "$(uname -s)" = Linux ] || LINUX=""

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/../../adapters/claude/hooks" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-agent-layers-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
mkdir -p "$SOT_COMM_HOME"
trap 'rm -rf "${WORK:?}"' EXIT
# Run against a copy of the scripts that can find no daemon and no hub, so a
# setup send can never reach a live one (the test-hub-files.sh seam); the
# append is checked, since a read-only source mode would silently skip it.
cp -r "$SCRIPTS_DIR" "$WORK/scripts" && chmod -R u+w "$WORK/scripts" || { echo "FATAL: cannot copy the scripts" >&2; exit 1; }
SCRIPTS_DIR="$WORK/scripts"
cat >> "$SCRIPTS_DIR/comm-lib.sh" <<'STUB'

# ---- no daemon, no hub (test only) ------------------------------------------
sot_daemon_endpoint() { return 1; }
sot_relay_endpoint() { return 1; }
STUB
grep -q '^sot_relay_endpoint() { return 1; }$' "$SCRIPTS_DIR/comm-lib.sh" || { echo "FATAL: the no-daemon stub did not land in the copy" >&2; exit 1; }
cat >> "$SCRIPTS_DIR/comm-lib.sh" <<'SIMWIN'

# ---- a Windows host on Linux (test only, only while SIMWIN is set) ----------
# Windows pids are Linux pids; a process is native when its argv[0] ends in .exe,
# else MSYS. Cygwin's /proc, this process and its MSYS ancestors (ppid 1 under a
# native parent), is built here, when the library is sourced; $SIMWIN/sotd walks
# the native part.
if [ -n "${SIMWIN:-}" ]; then
    _sot_is_windows() { return 0; }
    SOTD_BIN="$SIMWIN/sotd"
    _SOT_PROC="$SIMWIN/proc.$$"
    rm -rf "${_SOT_PROC:?}"; mkdir -p "$_SOT_PROC"
    _sim_p=$$
    while [ "$_sim_p" -gt 1 ] && IFS= read -r _sim_l < "/proc/$_sim_p/stat" 2>/dev/null; do
        _sim_l="${_sim_l##*) }"; _sim_l="${_sim_l#* }"; _sim_pp="${_sim_l%% *}"
        _sim_a0=""; IFS= read -r -d '' _sim_a0 < "/proc/$_sim_p/cmdline" 2>/dev/null || :
        _sim_pa0=""; [ "$_sim_pp" -le 1 ] || { IFS= read -r -d '' _sim_pa0 < "/proc/$_sim_pp/cmdline" 2>/dev/null || :; }
        case "$_sim_a0" in
            *.exe) ;;
            *) mkdir -p "$_SOT_PROC/$_sim_p"
               ln -sf "/proc/$_sim_p/cmdline" "$_SOT_PROC/$_sim_p/cmdline"
               echo "$_sim_p" > "$_SOT_PROC/$_sim_p/winpid"
               case "$_sim_pa0" in *.exe|'') _sim_q=1 ;; *) _sim_q="$_sim_pp" ;; esac
               printf '%s (sim) S %s\n' "$_sim_p" "$_sim_q" > "$_SOT_PROC/$_sim_p/stat" ;;
        esac
        _sim_p="$_sim_pp"
    done
fi
SIMWIN
JOIN="$SCRIPTS_DIR/comm-join.sh"; POLL="$SCRIPTS_DIR/comm-poll.sh"
SEND="$SCRIPTS_DIR/comm-send.sh"; STATUS="$SCRIPTS_DIR/comm-status.sh"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
# A fake findmnt first on PATH reports a local filesystem, so the inbox lock is
# this box's own and every filing is local, whatever $WORK sits on and with no
# daemon involved (the same seam as test-relay-file-first.sh).
mkdir -p "$WORK/findmnt-bin" "$SOT_COMM_HOME/inbox"
printf '#!/bin/sh\necho "ext4 rw,relatime /dev/fake"\n' > "$WORK/findmnt-bin/findmnt"
chmod +x "$WORK/findmnt-bin/findmnt"
export PATH="$WORK/findmnt-bin:$PATH"
bash -c 'source "$1"; sot_inbox_lock_identity "$INBOX_DIR"' _ "$SCRIPTS_DIR/comm-lib.sh" > "$SOT_COMM_HOME/inbox-lock-manager"
export SOT_COMM_TEST_HOST="testhost"
unset SOT_COMM_NAME COMM_STATUS_ORIGIN CLAUDE_CODE_SESSION_ID SOT_WORKSPACE_ID SOT_COMM_SELF_FILE SOT_COMM_HOOKS

PASS=0; FAIL=0
ok()  { echo "PASS: $1"; PASS=$((PASS + 1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }
has()   { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1 (no '$3' in: $2)" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1 (found '$3' in: $2)" ;; *) ok "$1" ;; esac; }
eq()    { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (got '$2', want '$3')"; fi; }

# --- 0. the guard closed daemon discovery (lib-home-guard.sh, its self-test passed) ---
eq  "guard: sotd resolves to the refusing stub" "$(command -v sotd)" "$_GUARD_STUBS/sotd"
gout="$(sotd probe 2>&1)"; grc=$?
eq  "guard: the sotd stub exits 97" "$grc" 97
has "guard: the sotd stub says it refused" "$gout" "refused daemon discovery"
eq  "guard: pgrep resolves to the refusing stub" "$(command -v pgrep)" "$_GUARD_STUBS/pgrep"
for gs in powershell.exe pwsh pwsh.exe; do
    eq  "guard: $gs resolves to the refusing stub" "$(command -v "$gs")" "$_GUARD_STUBS/$gs"
    gout="$("$gs" -NoProfile 2>&1)"; grc=$?
    eq  "guard: the $gs stub exits 97" "$grc" 97
    has "guard: the $gs stub says it refused" "$gout" "refused daemon discovery"
done
case "${LOCALAPPDATA:-}" in "$WORK/guard/"*) ok "guard: LOCALAPPDATA is under the guard directory" ;; *) bad "guard: LOCALAPPDATA is under the guard directory (got '${LOCALAPPDATA:-}')" ;; esac

. "$(dirname "${BASH_SOURCE[0]}")/agent_layers/table.sh"
. "$(dirname "${BASH_SOURCE[0]}")/agent_layers/end_to_end.sh"

echo "agent layers: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
