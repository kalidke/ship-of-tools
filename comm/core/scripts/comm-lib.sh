#!/usr/bin/env bash
# comm-lib.sh — shared helpers for sot-comm. SOURCED, not executed.
# Implements the v1 protocol (see comm/PROTOCOL.md). Runtime data lives under
# $SOT_COMM_HOME (default ~/.sot-comm).

PROTOCOL_VERSION=1

COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"
INBOX_DIR="$COMM_HOME/inbox"
SELF_DIR="$COMM_HOME/self"
READ_DIR="$COMM_HOME/read"
LOCKDIR="$COMM_HOME/.registry.lock"

now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# _sot_secure_dir DIR — create-or-verify DIR as ours EXCLUSIVELY. Mirrors
# paths.rs::secure_private_dir EXACTLY (security review, F1): the old
# `mkdir -p`+`chmod` sequence trusted whatever was already at DIR — a
# hostile local user who can write into DIR's parent (`/tmp`, or a shared
# runtime dir) could pre-create DIR, or plant a SYMLINK there, as their
# own, and `mkdir -p` (no-op on an existing path) + `chmod` (follows a
# symlink to its target) would have accepted either without complaint —
# landing this user's tmux socket inside a directory the attacker
# controls. Prints nothing; returns 0 only if DIR is now verified private,
# 1 (with a reason on stderr) otherwise. Callers MUST treat a nonzero
# return as FATAL — no silent fallback to an unverified dir.
#   - absent  -> `mkdir -m 700` (no `-p`: DIR's parent — $XDG_RUNTIME_DIR,
#     /run/user/<uid>, or /tmp — is assumed to already exist, same
#     assumption the Rust side makes). Plain `mkdir` maps to a single
#     `mkdir(2)`, which fails atomically with EEXIST if anything (dir,
#     file, symlink) is already there — no create-then-chmod race window.
#   - present -> verified via `[ -L ]` (reject a symlink outright, checked
#     BEFORE any `-d`/`-e` test since those follow symlinks) then `stat`
#     for owner (`%u` must equal `id -u`) and mode (`%a` must be
#     owner-only, `mode & 0077 == 0`). Any failed check is a hard reject.
_sot_secure_dir() {
    local dir="$1"
    if [ -L "$dir" ]; then
        echo "sot_tmux_socket: refusing $dir — it's a symlink (possible hijack by another local user)" >&2
        return 1
    fi
    if [ -e "$dir" ]; then
        if [ ! -d "$dir" ]; then
            echo "sot_tmux_socket: refusing $dir — not a directory" >&2
            return 1
        fi
        local owner; owner="$(stat -c '%u' "$dir" 2>/dev/null || true)"
        if [ -z "$owner" ] || [ "$owner" != "$(id -u)" ]; then
            echo "sot_tmux_socket: refusing $dir — owned by uid '${owner:-?}' (expected $(id -u); possible hijack)" >&2
            return 1
        fi
        local mode; mode="$(stat -c '%a' "$dir" 2>/dev/null || true)"
        if [ -z "$mode" ] || [ $((0$mode & 0077)) -ne 0 ]; then
            echo "sot_tmux_socket: refusing $dir — mode '${mode:-?}' is group/other-accessible" >&2
            return 1
        fi
        return 0
    fi
    if ! mkdir -m 700 "$dir" 2>/dev/null; then
        echo "sot_tmux_socket: could not create private dir $dir" >&2
        return 1
    fi
    return 0
}

# sot_tmux_socket — resolve the daemon's PRIVATE per-user tmux server socket
# (security review, ADR: tmux-socket isolation). Before this, every comm
# script talked to tmux's default server, but the Rust daemon (`sotd`)
# creates workspace sessions on a private, non-default socket
# (`paths::tmux_socket_path`) — a comm script targeting the default server
# would silently miss those sessions (`tmux has-session` false, `tmux
# send-keys` into nothing). ALWAYS resolve through this before any `tmux`
# call in a script that might touch a daemon-created session; a caller-set
# `$SOT_TMUX_SOCK` (e.g. a test harness) is honoured as-is, unchecked — the
# caller owns that responsibility.
#
# Prefers querying `sotd` directly (`sotd tmux-socket-path`) — the single
# source of truth for the resolution logic, so it can never drift from the
# Rust side. Falls back to a shell mirror of the EXACT same tiers, in the
# same order, only when `sotd` isn't on `$PATH` or the query fails:
#   1. $XDG_RUNTIME_DIR/sot/tmux.sock — set, existing, NOT a symlink,
#      owned by us, and owner-only (mode & 0077 == 0) — the same
#      symlink-rejection + ownership + mode posture the Rust side's
#      `is_private_dir` applies.
#   2. /run/user/<uid>/sot/tmux.sock — same convention by well-known path,
#      for a shell that didn't inherit the env var.
#   3. /tmp/sot-<uid>/tmux.sock — last resort; /tmp is always a LOCAL mount
#      (unlike $HOME, which is NFS-shared across this lab's boxes and where
#      a unix-domain socket doesn't work).
# The socket's parent dir is then created-or-verified via `_sot_secure_dir`
# (mirrors the Rust side's `secure_private_dir`) — NOT a blind
# `mkdir -p`+`chmod`. On failure this returns 1 and prints NOTHING to
# stdout (the reason goes to stderr via `_sot_secure_dir`); callers MUST
# check the exit status, not just emptiness, and treat failure as fatal.
sot_tmux_socket() {
    if [ -n "${SOT_TMUX_SOCK:-}" ]; then
        printf '%s\n' "$SOT_TMUX_SOCK"
        return 0
    fi
    local sock="" sotd_bin
    sotd_bin="$(command -v sotd 2>/dev/null || true)"
    if [ -n "$sotd_bin" ]; then
        sock="$("$sotd_bin" tmux-socket-path 2>/dev/null || true)"
    fi
    if [ -z "$sock" ]; then
        local uid; uid="$(id -u)"
        if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ ! -L "$XDG_RUNTIME_DIR" ] && [ -d "$XDG_RUNTIME_DIR" ]; then
            local xowner xmode
            xowner="$(stat -c '%u' "$XDG_RUNTIME_DIR" 2>/dev/null || true)"
            xmode="$(stat -c '%a' "$XDG_RUNTIME_DIR" 2>/dev/null || true)"
            if [ -n "$xowner" ] && [ "$xowner" = "$uid" ] \
               && [ -n "$xmode" ] && [ $((0$xmode & 0077)) -eq 0 ]; then
                sock="$XDG_RUNTIME_DIR/sot/tmux.sock"
            fi
        fi
        if [ -z "$sock" ] && [ -d "/run/user/$uid" ]; then
            sock="/run/user/$uid/sot/tmux.sock"
        fi
        [ -z "$sock" ] && sock="/tmp/sot-$uid/tmux.sock"
    fi
    if ! _sot_secure_dir "$(dirname "$sock")"; then
        return 1
    fi
    printf '%s\n' "$sock"
}

# sot_daemon_endpoint [EXPLICIT] — resolve the control socket endpoint used by
# comm relay/spawn/FE commands. Explicit endpoints keep their old behavior; the
# socket-only default is discovered by asking sotd for the label-derived socket.
sot_daemon_endpoint() {
    local explicit="${1:-}"
    [ -n "$explicit" ] && { printf '%s\n' "$explicit"; return 0; }
    [ -n "${SOT_SOCKET:-}" ] && { printf 'unix:%s\n' "$SOT_SOCKET"; return 0; }

    # Keep compatibility with development daemons launched with explicit
    # transport flags.
    local line
    while IFS= read -r line; do
        case "$line" in
            *comm-relay*|*comm-spawn*|*comm-despawn*|*comm-listen*|*comm-watch*|*comm-poll*|*sot-fe*|*sot-nav*)
                continue
                ;;
        esac
        if [[ "$line" =~ --tcp[[:space:]]+([^[:space:]]+) ]]; then
            printf 'tcp:%s\n' "${BASH_REMATCH[1]}"
            return 0
        fi
        if [[ "$line" =~ --socket[[:space:]]+([^[:space:]]+) ]]; then
            printf 'unix:%s\n' "${BASH_REMATCH[1]}"
            return 0
        fi
    done < <(pgrep -af 'sotd' 2>/dev/null || true)

    # Normal socket-only mode: the daemon may have only --label on argv, so
    # there is no transport flag to scrape. Query the same binary family the
    # installer/launcher uses. The default label is the product backend label;
    # override with SOT_BACKEND_LABEL for a non-default session.
    local label="${SOT_BACKEND_LABEL:-sot}"
    local bin sock
    _try_sotd_socket_bin() {
        local candidate="$1"
        [ -n "$candidate" ] || return 1
        [ -x "$candidate" ] || return 1
        sock="$("$candidate" session-socket-path "$label" 2>/dev/null || true)"
        [ -n "$sock" ] && [ -S "$sock" ] || return 1
        printf 'unix:%s\n' "$sock"
        return 0
    }

    _try_sotd_socket_bin "${SOTD_BIN:-}" && return 0
    bin="$(command -v sotd 2>/dev/null || true)"
    _try_sotd_socket_bin "$bin" && return 0
    _try_sotd_socket_bin "$HOME/.local/share/sot/bin/sotd" && return 0
    _try_sotd_socket_bin "$HOME/.local/bin/sotd" && return 0

    while IFS= read -r line; do
        local pid="${line%% *}"
        case "$pid" in ''|*[!0-9]*) continue ;; esac
        [ -r "/proc/$pid/exe" ] || continue
        bin="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
        _try_sotd_socket_bin "$bin" && return 0
    done < <(pgrep -af 'sotd' 2>/dev/null || true)

    return 1
}

ensure_home() {
    mkdir -p "$COMM_HOME" "$INBOX_DIR" "$SELF_DIR" "$READ_DIR"
    if [ ! -f "$REGISTRY" ]; then
        printf '{"protocol_version": %s, "agents": {}}\n' "$PROTOCOL_VERSION" > "$REGISTRY"
    fi
}

# with_lock CMD [ARGS...] — run CMD holding the registry lock (mkdir spinlock).
# CMD may be a shell function defined in this sourced lib.
with_lock() {
    local tries=0
    while ! mkdir "$LOCKDIR" 2>/dev/null; do
        tries=$((tries + 1))
        if [ "$tries" -gt 200 ]; then
            echo "WARN: forcing stale lock $LOCKDIR" >&2
            rmdir "$LOCKDIR" 2>/dev/null || true
            mkdir "$LOCKDIR" 2>/dev/null || true
            break
        fi
        sleep 0.05
    done
    "$@"
    local rc=$?
    rmdir "$LOCKDIR" 2>/dev/null || true
    return $rc
}

# --- registry mutators (call inside with_lock) ---
registry_put() {  # name objJSON
    jq --arg n "$1" --argjson o "$2" '.agents[$n] = $o' "$REGISTRY" \
        > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
}
registry_del() {  # name
    jq --arg n "$1" 'del(.agents[$n])' "$REGISTRY" \
        > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
}
registry_touch() {  # name — bump last_seen if present
    local ts; ts="$(now_iso)"
    jq --arg n "$1" --arg t "$ts" \
        'if .agents[$n] then .agents[$n].last_seen = $t else . end' \
        "$REGISTRY" > "$REGISTRY.tmp" && mv "$REGISTRY.tmp" "$REGISTRY"
}

# --- derived-handle disambiguation (ADR 0028 addendum: "derived vs
# explicit") --- single home for the algorithm; comm-join.sh and
# comm-spawn.sh both call sot_derive_handle. This is ONLY for a name that
# comes from DERIVATION (the default <basename>-<host>): a caller must never
# route an explicit --name, $SOT_COMM_NAME, or an already-joined self-file
# identity through here — those stay verbatim, unconditionally.

_SOT_NO_ENTRY="__sot_no_entry__"

# sot_canonical_path PATH — best-effort absolute, symlink-resolved path (the
# "canonical project root" the disambiguation compares). Falls back through
# realpath -> readlink -f -> the raw path so a minimal environment still gets
# a stable-enough string, just not de-symlinked.
sot_canonical_path() {
    local p="$1"
    if command -v realpath >/dev/null 2>&1; then
        realpath "$p" 2>/dev/null && return 0
    fi
    if command -v readlink >/dev/null 2>&1; then
        readlink -f "$p" 2>/dev/null && return 0
    fi
    printf '%s\n' "$p"
}

# sot_hash6 STR — first 6 hex chars of sha256(STR); stable per input, used
# only for the last-resort hash-qualified handle tier. Falls back to cksum
# (POSIX, always present) on a system with neither sha256sum nor shasum —
# still stable per input, just not sha256; that gap only matters for
# cross-machine collision odds at a scale this feature doesn't operate at.
sot_hash6() {
    if command -v sha256sum >/dev/null 2>&1; then
        printf '%s' "$1" | sha256sum | cut -c1-6
    elif command -v shasum >/dev/null 2>&1; then
        printf '%s' "$1" | shasum -a 256 | cut -c1-6
    else
        printf '%s' "$1" | cksum | cut -d' ' -f1
    fi
}

# sot_registry_entry_root NAME — the registry row's `root` for NAME, or the
# sentinel $_SOT_NO_ENTRY when NAME has no row at all. The distinction
# matters: a row that PREDATES this feature has no `root` key at all, which
# reads back as an EMPTY string here — that counts as "unknown root", a
# COLLISION for the algorithm below, same as a confirmed different root.
# Only a genuinely absent row, or a row whose root equals mine, claims a
# candidate outright.
sot_registry_entry_root() {
    jq -r --arg n "$1" --arg none "$_SOT_NO_ENTRY" \
        'if (.agents | has($n)) then (.agents[$n].root // "") else $none end' \
        "$REGISTRY" 2>/dev/null
}

# sot_derive_handle ROOT HOST — the derived-name join algorithm (see the ADR
# 0028 addendum in docs/adr/ for the full contract). Escalates through three
# tiers, each checked against the CURRENT registry. Liveness is deliberately
# NOT consulted here — a stale registry row still holds its claim until
# existing cleanup paths remove it; this keeps the rule one-dimensional (root
# comparison only) and prevents handle flip-flop between two projects
# depending on who happens to be running.
#   1. <basename>-<host>             — claim if free, or already mine (same
#                                       root) — today's reclaim/rejoin path
#   2. <basename>-<parentdir>-<host> — parentdir = basename(dirname(ROOT));
#                                       same free-or-mine check
#   3. <basename>-<hash6>-<host>     — hash6 = sot_hash6(canonical ROOT);
#                                       claimed unconditionally
# Prints ONE tab-separated line: "<handle>\t<qualifier>" (qualifier is empty
# at tier 1, "<parentdir>" at tier 2, "<hash6>" at tier 3). A caller that only
# wants the handle: `IFS=$'\t' read -r NAME _ <<< "$(sot_derive_handle ...)"`.
# When escalating past tier 1, writes one explanatory line to stderr naming
# the bare handle, who holds it, and what was joined instead.
sot_derive_handle() {
    local root="$1" host="$2" base parent hash6 tier1 tier2 tier3
    local held1 held2 shown1 shown2
    root="$(sot_canonical_path "$root")"
    base="$(basename "$root")"

    tier1="${base}-${host}"
    held1="$(sot_registry_entry_root "$tier1")"
    if [ "$held1" = "$_SOT_NO_ENTRY" ] || [ "$held1" = "$root" ]; then
        printf '%s\t\n' "$tier1"
        return 0
    fi
    shown1="$held1"; [ -z "$shown1" ] && shown1="an unknown project"

    parent="$(basename "$(dirname "$root")")"
    tier2="${base}-${parent}-${host}"
    held2="$(sot_registry_entry_root "$tier2")"
    if [ "$held2" = "$_SOT_NO_ENTRY" ] || [ "$held2" = "$root" ]; then
        echo "comm: '@$tier1' is already held by $shown1 — joining as '@$tier2' instead" >&2
        printf '%s\t%s\n' "$tier2" "$parent"
        return 0
    fi
    shown2="$held2"; [ -z "$shown2" ] && shown2="an unknown project"

    hash6="$(sot_hash6 "$root")"
    tier3="${base}-${hash6}-${host}"
    echo "comm: '@$tier1' (held by $shown1) and '@$tier2' (held by $shown2) are both taken — joining as '@$tier3' instead" >&2
    printf '%s\t%s\n' "$tier3" "$hash6"
}

# --- atomic derive + claim (closes the read-then-write race) -----------
# sot_derive_handle above only DECIDES a name by reading the registry; a
# caller that derives and only LATER locks to registry_put leaves a window
# between the two where a second, concurrent derived join (a DIFFERENT
# root, same basename+host) can observe that exact same "still free" state
# and also decide on tier 1 — whichever registry_put lands second then
# silently clobbers the first. That is the aliasing bug this whole feature
# exists to close, so it must not survive as a race window. The window is
# not theoretical: comm-spawn.sh is driven programmatically for bulk
# workspace bring-up, joining many sessions back-to-back.
#
# claim_derived_handle ROOT HOST OBJ_JSON — derive AND registry_put the
# result as ONE critical section under the registry lock, so no other
# claim can observe registry state in between. Sets globals CLAIMED_NAME
# and CLAIMED_QUALIFIER (mirrors sot_derive_handle's two outputs) for the
# caller to read after this returns. Both comm-join.sh (a plain join) and
# comm-spawn.sh (the provisional row) route a derived name through this —
# one shared locked claim path, not two copies of "derive, then lock to
# write" that could each get this wrong.
#
# _sot_claim_derived_handle is the with_lock callee; it must NEVER be
# invoked directly, and never as `X=$(with_lock ...)`. with_lock runs its
# command directly ("$@", no subshell) specifically so a callee's global
# assignment survives past its return — capturing it via command
# substitution would fork a subshell and lose CLAIMED_NAME exactly the way
# the test harness's own next_self_file() lost its counter (see
# comm/core/tests/test-join-disambiguation.sh) — the same lesson, twice.
_sot_claim_derived_handle() {  # ROOT HOST OBJ_JSON — call only via with_lock
    local root="$1" host="$2" obj="$3" qualifier
    IFS=$'\t' read -r CLAIMED_NAME qualifier <<< "$(sot_derive_handle "$root" "$host")"
    CLAIMED_QUALIFIER="$qualifier"
    registry_put "$CLAIMED_NAME" "$obj"
}
claim_derived_handle() {  # ROOT HOST OBJ_JSON
    with_lock _sot_claim_derived_handle "$1" "$2" "$3"
}


# sot_oneshot_request FRAME OP — one-shot request/response on a fresh daemon
# connection: send hello + FRAME, return (stdout) the first COMPLETE line
# whose op matches OP. Hardened after a live intermittent failure
# (2026-08-22, a peer session's targeted fe.command) and a codex review of
# the first hardening round:
#   - the WRITER lingers for the whole read window (some nc variants quit on
#     stdin EOF, racing the reply — the original bug);
#   - nc drains into a TEMP FILE we poll for the matching op line (fresh
#     connections receive ALL broadcast evt traffic — multi-MB repl frames
#     queued ahead of the res just stream past);
#   - a match is accepted only when jq parses the line (an op match can be
#     an UNTERMINATED line still being appended — op precedes payload);
#   - teardown kills the KNOWN pid only (never `kill %%`/`wait <member>`:
#     the jobspec can resolve to an unrelated background job in a caller
#     that backgrounds other work, and waiting any pipeline member waits
#     the whole job — measured as a linger-long floor per call). The
#     writer's sleep is left to die alone — bounded by the window, writes
#     nothing, holds nothing.
# Read window: SOT_SEND_TIMEOUT, else the caller's SEND_TIMEOUT (sot-fe's
# repl paths set --timeout up to minutes — the window MUST honor it), else
# 10s. Uses ENDPOINT (unix:/path or tcp:host:port) from the caller's scope.
sot_oneshot_request() {
    local frame="$1" op="$2"
    local timeout_s="${SOT_SEND_TIMEOUT:-${SEND_TIMEOUT:-10}}"
    local tmp ncpid line="" deadline
    tmp="$(mktemp "${XDG_RUNTIME_DIR:-/tmp}/sot-oneshot-XXXXXX")" || return 1
    case "$ENDPOINT" in
        unix:*)
            command -v nc >/dev/null 2>&1 || {
                echo "ERROR: nc not found and endpoint is a unix socket (needs nc -U)" >&2
                rm -f "$tmp"; return 1; }
            { _sot_hello; printf '%s\n' "$frame"; sleep "$timeout_s"; } \
                | timeout "$timeout_s" nc -U "${ENDPOINT#unix:}" > "$tmp" 2>/dev/null &
            ncpid=$!
            ;;
        tcp:*)
            local hp="${ENDPOINT#tcp:}" host port
            host="${hp%:*}"; port="${hp##*:}"
            if command -v nc >/dev/null 2>&1; then
                { _sot_hello; printf '%s\n' "$frame"; sleep "$timeout_s"; } \
                    | timeout "$timeout_s" nc "$host" "$port" > "$tmp" 2>/dev/null &
                ncpid=$!
            else
                # /dev/tcp fallback: the fd stays open for the whole window,
                # so the EOF race does not exist here — plain bounded read.
                (
                    exec 9<>"/dev/tcp/$host/$port" || exit 1
                    { _sot_hello; printf '%s\n' "$frame"; } >&9
                    timeout "$timeout_s" cat <&9
                    exec 9<&- 9>&- 2>/dev/null || true
                ) > "$tmp" 2>/dev/null &
                ncpid=$!
            fi
            ;;
        *) rm -f "$tmp"; return 1 ;;
    esac
    # Accept only a COMPLETE res line: op precedes payload on the wire, so a
    # grep hit can be a line nc is still appending. jq gates acceptance when
    # available; without jq (minimal envs) fall back to requiring that the
    # file's last byte is a newline OR more bytes follow the match.
    _sot_line_ok() {
        if command -v jq >/dev/null 2>&1; then
            printf '%s' "$1" | jq -e . >/dev/null 2>&1
        else
            case "$1" in *"}"* ) return 0 ;; * ) return 1 ;; esac
        fi
    }
    deadline=$(( $(date +%s) + timeout_s ))
    while [ "$(date +%s)" -le "$deadline" ]; do
        line="$(grep -m1 "\"op\":\"$op\"" "$tmp" 2>/dev/null || true)"
        if [ -n "$line" ] && _sot_line_ok "$line"; then
            break
        fi
        line=""
        kill -0 "$ncpid" 2>/dev/null || {
            # transport exited — one final scan for a reply that landed last
            line="$(grep -m1 "\"op\":\"$op\"" "$tmp" 2>/dev/null || true)"
            _sot_line_ok "$line" || line=""
            break; }
        sleep 0.1
    done
    kill "$ncpid" 2>/dev/null || true
    rm -f "$tmp"
    [ -n "$line" ] && printf '%s\n' "$line"
}
