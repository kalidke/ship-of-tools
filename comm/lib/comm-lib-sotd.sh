# comm-lib-sotd.sh: messaging's shell door. A script reaches messaging through `sot_comm VERB`, which runs `sotd comm`,
# the one implementation (rust/backend/src/comm/CLAUDE.md); nothing here reads or writes the comm folder itself. Three
# names stay functions because many scripts call them and tests stand in for them: the agent gate, the routable
# identity check and the registry read. Sourced by comm-lib.sh; defines functions and one global, SOT_LOCK_WAIT_SECS.

# _sot_msys_table — on Windows (Git Bash) the Cygwin process table the agent-layer walk needs, as `<this shell's
# pid>;<pid>:<winpid>:<ppid>,...`, read from Cygwin's own /proc: an MSYS program that execs another is a new Windows
# process whose Windows parent has exited, so a native walk alone stops short of the session. Nothing elsewhere.
_sot_msys_table() {
    _sot_is_windows || return 0
    local f p w pp out="$$;"
    for f in /proc/[0-9]*/winpid; do
        p="${f#/proc/}"; p="${p%/winpid}"
        { read -r w < "$f" && read -r pp < "/proc/$p/ppid"; } 2>/dev/null || continue
        out="$out$p:$w:$pp,"
    done
    printf '%s' "$out"
}

# sot_comm VERB [ARG...] — `sotd comm VERB ARG...` with this shell's stdin, stdout and stderr, and its exit status;
# 127, with one line on stderr, when no sotd is found. SOT_COMM_BIN names the scripts' folder for the verbs that run a
# sibling script (a join's `comm-status.sh join`, a list's `sot-fe version`); MSYS2_ARG_CONV_EXCL keeps Git Bash from
# rewriting an argument that starts with `/` for the native sotd.exe.
sot_comm() {
    local sotd
    sotd="$(sot_sotd)" || { echo "sot-comm: cannot run: no sotd found (SOTD_BIN, PATH, the install paths)" >&2; return 127; }
    SOT_COMM_BIN="${SOT_COMM_BIN:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}" SOT_COMM_MSYS="$(_sot_msys_table)" \
        MSYS2_ARG_CONV_EXCL='*' "$sotd" comm "$@"
}

# The registry lock's wait for a command, in whole seconds: sotd reads this knob, and rows' work-state script passes it
# on (`--lock-wait`).
SOT_LOCK_WAIT_SECS="${SOT_LOCK_WAIT_SECS:-10}"

# sot_require_agent — 0 when this process may act as its row's handle; else one reason on stdout, and 1 for a second
# agent inside the session, 2 for an ancestry that cannot be read or a row it is not shown to run inside.
sot_require_agent() { sot_comm require-agent; }
# sot_require_routable_identity — the gate, then `sotd comm routable`: NAME has an entry whose root is this
# project's (PROJECT_ROOT, from comm-context.sh); else one reason on stdout and 1.
sot_require_routable_identity() {
    local why
    why="$(sot_require_agent)" || { echo "$why"; return 1; }
    sot_comm routable "${NAME:-}" "${PROJECT_ROOT:-}"
}

# sot_registry_read [HANDLE] — the registry (no HANDLE) or HANDLE's entry, compact JSON on stdout: 0 present, 1 absent,
# 2 unreadable (never absent).
sot_registry_read() { sot_comm registry read "$@"; }
# sot_canonical_path PATH — PATH absolute with every link resolved, or nothing and 1: a spawn's root, which it
# compares with what it wrote itself.
sot_canonical_path() {
    local out
    if command -v realpath >/dev/null 2>&1 && out="$(realpath -- "$1" 2>/dev/null)" && [ -n "$out" ]; then printf '%s\n' "$out"; return 0; fi
    if command -v readlink >/dev/null 2>&1 && out="$(readlink -f -- "$1" 2>/dev/null)" && [ -n "$out" ]; then printf '%s\n' "$out"; return 0; fi
    echo "sot_canonical_path: could not resolve a canonical path for '$1' (no working realpath or readlink -f) — refusing to record a relative/unresolved project root" >&2
    return 1
}
