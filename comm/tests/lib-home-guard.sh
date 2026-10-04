# lib-home-guard.sh — every test-*.sh under comm/ sources this before any
# command but `set`, and test-rm-guard.sh fails a suite that does not (test code only; never
# deployed). A suite's setup deletes registry.json and inbox files, so a comm
# home that resolved to the live one would wipe the fleet's registry and inboxes.
#
# Sourcing records the live comm homes: $HOME/.sot-comm, the account's own
# home as the system records it plus /.sot-comm (`~name` for `id -un`, which
# reads getpwnam on every platform; a HOME swapped by env -i or by an earlier
# guard is not the live folder; a name not starting [A-Za-z_] or outside [A-Za-z0-9._-] records none),
# and any inherited SOT_COMM_HOME. It then drops the host's comm identity and
# its daemon route: SOT_COMM_HOME, SOT_COMM_NAME, SOT_COMM_SELF_FILE,
# SOT_WORKSPACE_ID, SOT_SOCKET and every exported *_ENDPOINT, plus the host
# session's CLAUDE_CODE_SESSION_ID, which the hooks key their tick files on
# ahead of SOT_WORKSPACE_ID (inherited, it merged a suite's per-run keys into
# one and throttled every run after the first). It then closes the daemon
# discovery routes comm-lib.sh has beyond those variables, so a suite can never
# reach a live daemon by rediscovery (that once sent a test frame to the real
# one): SOTD_BIN and SOT_WORKSPACE are unset, XDG_RUNTIME_DIR (and, once
# guard_fresh_home runs, TMPDIR) point at fresh directories, LOCALAPPDATA is
# under the guard's own directory, and a directory of refusing `sotd`,
# `sotd.exe`, `pgrep`, `powershell.exe`, `pwsh` and `pwsh.exe` stubs (exit 97)
# leads PATH: on Windows the library asks PowerShell for the RUNNING sotd's path and
# connect-probes its pipe, so those stubs and LOCALAPPDATA leave discovery no
# executable to find. `nc` and `ssh` are not stubbed: they only dial an endpoint
# discovery produced, and suites run them against sockets and hosts of their own.
# A self-test sources the tree's comm-lib.sh and calls sot_daemon_endpoint,
# sot_relay_endpoint and _sot_windows_local_pipe (on every host); if any
# prints or succeeds, or the library does not load, the suite stops with FATAL
# before any test runs. Right after the
# suite makes its mktemp work directory and names its comm home, before any
# other command, it calls:
#   guard_fresh_home WORK            HOME becomes WORK/test-home, fresh, so the
#                                    suite's own cleanup of WORK removes it;
#                                    the discovery guard moves under WORK too
#   guard_refuse_live_home HOME_DIR  FATAL and exit 2 when HOME_DIR, the
#                                    suite's comm home, is empty, or equals or
#                                    lies under a recorded live home
#   guard_stage_bin DIR              stage-bin.sh lays the comm scripts flat in
#                                    DIR/staged-bin, as the installer lays out
#                                    ~/.sot-comm/bin, and prints that path; the
#                                    suite runs its scripts from there
# Paths compare physically (cd -P); the part of a path that does not exist yet
# is kept as written, so a live home that does not exist (as on CI) compares as
# its literal path.
# A suite that pins a row-shaped identity (a self file named <host>__<id>.txt, or
# SOT_WORKSPACE_ID with no self file) runs a gated comm script only beneath that
# row's capsule (PROTOCOL.md's row rule), so the file also defines
#   in_row ID CMD...                 run CMD beneath a stand-in for row ID's capsule
# The path need not exist: the walk reads only the command line. CMD must be an
# executable (a script or a binary), not a shell function of the suite; exported
# variables reach it, unexported ones do not. Linux and macOS only: a Windows walk names the
# executable, which `exec -a` cannot change.
_GUARD_LIVE=("$HOME/.sot-comm")
_GUARD_ACCT="" _GUARD_USER="$(id -un 2>/dev/null)"
case "$_GUARD_USER" in
    ''|[!A-Za-z_]*|*[!A-Za-z0-9._-]*) ;;
    *) eval "_GUARD_ACCT=~$_GUARD_USER" ;;  # the name is checked: no shell syntax, and no ~-, ~+ or ~0 form
esac
case "$_GUARD_ACCT" in /*) _GUARD_LIVE+=("$_GUARD_ACCT/.sot-comm") ;; *) _GUARD_ACCT="" ;; esac
[ -z "${SOT_COMM_HOME:-}" ] || _GUARD_LIVE+=("$SOT_COMM_HOME")
unset SOT_COMM_HOME SOT_COMM_NAME SOT_COMM_SELF_FILE SOT_WORKSPACE_ID SOT_SOCKET CLAUDE_CODE_SESSION_ID
for _guard_v in $(compgen -e); do
    case "$_guard_v" in *_ENDPOINT) unset "$_guard_v" ;; esac
done
unset _guard_v

_guard_fatal() { echo "lib-home-guard: FATAL $*" >&2; [ -z "${_GUARD_BOOT:-}" ] || rm -rf "${_GUARD_BOOT:?}"; exit 1; }

# _guard_self_test DIR — the tree's own comm-lib.sh must find no daemon and no hub.
_guard_self_test() {
    local lib fn out rc
    lib="$(dirname "${BASH_SOURCE[0]}")/../lib/comm-lib.sh"
    [ -r "$lib" ] || _guard_fatal "daemon discovery cannot be checked: no $lib"
    for fn in sot_daemon_endpoint sot_relay_endpoint _sot_windows_local_pipe; do
        out="$( export SOT_COMM_HOME="$1/comm"; . "$lib" >/dev/null 2>&1 || exit 99; "$fn" 2>/dev/null )" && rc=0 || rc=$?
        if [ "$rc" -eq 0 ] || [ -n "$out" ] || [ "$rc" -eq 99 ]; then
            _guard_fatal "daemon discovery is reachable ($fn: rc=$rc out='$out')"
        fi
    done
}

# _guard_close_discovery DIR [tmp] — the run's own scratch under DIR, refusing
# stubs first on PATH, then the self-test. Called again by guard_fresh_home.
_guard_close_discovery() {
    local d="$1" s
    mkdir -p "$d/bin" "$d/run" "$d/tmp" "$d/comm" "$d/localappdata" && chmod 700 "$d/run" || _guard_fatal "no scratch directory $d"
    for s in sotd sotd.exe pgrep powershell.exe pwsh pwsh.exe; do
        printf '#!/bin/sh\necho "lib-home-guard: refused daemon discovery ($0 $*)" >&2\nexit 97\n' > "$d/bin/$s" \
            && chmod +x "$d/bin/$s" || _guard_fatal "cannot install the $s stub"
    done
    unset SOTD_BIN SOT_SOCKET SOT_WORKSPACE_ID SOT_COMM_SELF_FILE SOT_COMM_NAME SOT_WORKSPACE
    [ -z "${_GUARD_STUBS:-}" ] || PATH="${PATH//$_GUARD_STUBS:/}"
    _GUARD_STUBS="$d/bin"
    export PATH="$_GUARD_STUBS:$PATH" XDG_RUNTIME_DIR="$d/run" LOCALAPPDATA="$d/localappdata"
    [ -z "${2:-}" ] || export TMPDIR="$d/tmp"
    _guard_self_test "$d"
}
_GUARD_BOOT="$(mktemp -d)" || _guard_fatal "no scratch directory"
_guard_close_discovery "$_GUARD_BOOT"

_guard_phys() {  # PATH
    local d="${1%/}" rest=""
    while [ ! -d "$d" ]; do
        case "$d" in */?*) ;; *) printf '%s\n' "$1"; return 0 ;; esac
        rest="/${d##*/}$rest"; d="${d%/*}"
    done
    printf '%s%s\n' "$(cd -P -- "$d" 2>/dev/null && pwd -P || printf '%s' "$d")" "$rest"
}

guard_fresh_home() {  # WORK
    [ -n "${1:-}" ] && [ -d "$1" ] && mkdir -p "$1/test-home" \
        || { echo "FATAL: no work directory to hold a fresh HOME (got: '${1:-}')" >&2; exit 2; }
    export HOME="$1/test-home"
    _guard_close_discovery "$1/guard" tmp
    [ -z "${_GUARD_BOOT:-}" ] || rm -rf "${_GUARD_BOOT:?}"
    _GUARD_BOOT=""
}

guard_refuse_live_home() {  # HOME_DIR
    local home live
    [ -n "${1:-}" ] || { echo "FATAL: this suite names no comm home of its own" >&2; exit 2; }
    home="$(_guard_phys "$1")"
    for live in "${_GUARD_LIVE[@]}"; do
        case "$home/" in
            "$(_guard_phys "$live")/"*)
                echo "FATAL: the comm home $1 is, or lies under, the live comm home $live — refusing to run" >&2
                exit 2 ;;
        esac
    done
}

guard_stage_bin() {  # DIR : prints DIR/staged-bin, the comm scripts laid out flat
    local out
    out="$(bash "$(dirname "${BASH_SOURCE[0]}")/stage-bin.sh" "$1/staged-bin" 2>&1)" || _guard_fatal "$out"
    printf '%s\n' "$1/staged-bin"
}

in_row() {  # ID CMD... : run CMD beneath a stand-in for row ID's capsule
    local id="$1"; shift
    ( exec -a sot-capsule bash -c 'shift; "$@"; exit $?' _ "/in-row/state/workspaces/$id/voyages/v0" "$@" )
}
