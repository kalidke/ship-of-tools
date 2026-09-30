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
# SOT_WORKSPACE_ID, SOT_SOCKET and every exported *_ENDPOINT. Right after the
# suite makes its mktemp work directory and names its comm home, before any
# other command, it calls:
#   guard_fresh_home WORK            HOME becomes WORK/test-home, fresh, so the
#                                    suite's own cleanup of WORK removes it
#   guard_refuse_live_home HOME_DIR  FATAL and exit 2 when HOME_DIR, the
#                                    suite's comm home, is empty, or equals or
#                                    lies under a recorded live home
# Paths compare physically (cd -P); the part of a path that does not exist yet
# is kept as written, so a live home that does not exist (as on CI) compares as
# its literal path.
_GUARD_LIVE=("$HOME/.sot-comm")
_GUARD_ACCT="" _GUARD_USER="$(id -un 2>/dev/null)"
case "$_GUARD_USER" in
    ''|[!A-Za-z_]*|*[!A-Za-z0-9._-]*) ;;
    *) eval "_GUARD_ACCT=~$_GUARD_USER" ;;  # the name is checked: no shell syntax, and no ~-, ~+ or ~0 form
esac
case "$_GUARD_ACCT" in /*) _GUARD_LIVE+=("$_GUARD_ACCT/.sot-comm") ;; *) _GUARD_ACCT="" ;; esac
[ -z "${SOT_COMM_HOME:-}" ] || _GUARD_LIVE+=("$SOT_COMM_HOME")
unset SOT_COMM_HOME SOT_COMM_NAME SOT_COMM_SELF_FILE SOT_WORKSPACE_ID SOT_SOCKET
for _guard_v in $(compgen -e); do
    case "$_guard_v" in *_ENDPOINT) unset "$_guard_v" ;; esac
done
unset _guard_v

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
