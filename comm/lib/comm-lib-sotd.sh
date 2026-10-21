# comm-lib-sotd.sh: messaging's shell door. Every messaging function a script calls runs `sotd comm`, the one
# implementation (rust/backend/src/comm/CLAUDE.md); nothing here reads or writes the comm folder itself. The functions
# keep the names, arguments and answers the 0.6.6 library gave them, so rows' work-state scripts and the agents' CLIs
# call them unchanged.
# Sourced by comm-lib.sh; defines functions and one global, SOT_LOCK_WAIT_SECS.

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

# The tools a path that reads mail runs besides sotd: the end-of-turn check prints its blocks with jq.
sot_mail_tools() { echo jq; }

# sot_require_agent — 0 when this process may act as its row's handle; else one reason on stdout, and 1 for a second
# agent inside the session, 2 for an ancestry that cannot be read or a row it is not shown to run inside.
sot_require_agent() { sot_comm require-agent; }
# sot_require_routable_identity — the gate, then _sot_identity_routable: NAME has an entry whose root is this
# project's (PROJECT_ROOT, from comm-context.sh); else one reason on stdout and 1.
sot_require_routable_identity() { local why; why="$(sot_require_agent)" || { echo "$why"; return 1; }; _sot_identity_routable; }
_sot_identity_routable() { sot_comm routable "${NAME:-}" "${PROJECT_ROOT:-}"; }

# sot_registry_read [HANDLE] — the registry (no HANDLE) or HANDLE's entry, compact JSON on stdout: 0 present, 1 absent,
# 2 unreadable (never absent).
sot_registry_read() { sot_comm registry read "$@"; }
# The registry's writes, each one locked step in sotd (read, decide and write under the registry lock), so with_lock
# only runs its command: every caller passes it one of these.
with_lock() { "$@"; }
registry_put() {  # HANDLE ENTRY_JSON
    [ -n "$1" ] || { echo "registry_put: refusing to write an empty/blank handle" >&2; return 1; }
    printf '%s' "$2" | sot_comm registry put "$1"
}
registry_del() { sot_comm registry del "$1"; }
registry_touch() { sot_comm registry touch "$1"; }
# registry_del_if_provisional HANDLE ROOT NONCE — 0 deleted, 2 the entry is no longer the spawn's own, 1 failed.
registry_del_if_provisional() { sot_comm registry del-provisional "$1" "$2" "$3"; }

# sot_derive_handle MODE ROOT HOST — three lines, the handle, its qualifier and tier 1's name; nothing written.
sot_derive_handle() { sot_comm derive "$1" "$2" "$3"; }
# claim_derived_handle MODE ROOT HOST ENTRY_JSON — the derivation and its registry write as one locked step; sets
# CLAIMED_NAME, CLAIMED_QUALIFIER and CLAIMED_TIER1 (all empty first, so a failure leaves no stale value).
claim_derived_handle() {
    local out
    CLAIMED_NAME="" CLAIMED_QUALIFIER="" CLAIMED_TIER1=""
    out="$(printf '%s' "$4" | sot_comm derive "$1" "$2" "$3" --claim)" || return 1
    { IFS= read -r CLAIMED_NAME; IFS= read -r CLAIMED_QUALIFIER; IFS= read -r CLAIMED_TIER1; } <<< "$out"
    [ -n "$CLAIMED_NAME" ]
}

# _sot_self_slot HOST WORKSPACE_ID — the unpinned self file's leaf, `<host>__<id>.txt` (`nopane` for none).
_sot_self_slot() { sot_comm self-slot "$1" "${2:-}"; }
# sot_capsule_workspace_id — the row this process's identity names, or nothing and 1.
sot_capsule_workspace_id() { sot_comm row-id; }
# ensure_home — the comm folder made whole (its folders, private, and a registry only where there is none).
ensure_home() { sot_comm ensure-home; }
# A derived handle's pieces: sot_sanitize_component STR [MAX], _sot_handle_host RAW_HOST, sot_slug LABEL.
sot_sanitize_component() { sot_comm name-part component "$1" ${2:+"$2"}; }
_sot_handle_host() { sot_comm name-part handle-host "$1"; }
sot_slug() { sot_comm name-part slug "$1"; }
# sot_raw_host VAR — VAR set to this box's raw host name (hostname -s, case kept; SOT_COMM_TEST_HOST first).
sot_raw_host() { local _sot_rh; _sot_rh="$(sot_comm name-part raw-host)" || return 1; printf -v "$1" '%s' "$_sot_rh"; }
# sot_handoff_line HANDLE — the line naming the session's recovery file, when it exists.
sot_handoff_line() { sot_comm handoff-line "$1"; }

# sot_canonical_path PATH — PATH absolute with every link resolved, or nothing and 1: a spawn's root, which it
# compares with what it wrote itself.
sot_canonical_path() {
    local out
    if command -v realpath >/dev/null 2>&1 && out="$(realpath -- "$1" 2>/dev/null)" && [ -n "$out" ]; then printf '%s\n' "$out"; return 0; fi
    if command -v readlink >/dev/null 2>&1 && out="$(readlink -f -- "$1" 2>/dev/null)" && [ -n "$out" ]; then printf '%s\n' "$out"; return 0; fi
    echo "sot_canonical_path: could not resolve a canonical path for '$1' (no working realpath or readlink -f) — refusing to record a relative/unresolved project root" >&2
    return 1
}
