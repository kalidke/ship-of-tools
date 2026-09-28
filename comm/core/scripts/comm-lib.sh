#!/usr/bin/env bash
# comm-lib.sh — shared helpers for sot-comm. SOURCED, not executed.
# Implements the v1 protocol (see comm/PROTOCOL.md). Runtime data lives under
# $SOT_COMM_HOME (default ~/.sot-comm).

# _sot_is_windows — the ONE shared platform test (Codex review, PR1 round 2
# finding 6: Windows-specific defaults/guards must live HERE, not duplicated
# per-caller — a caller-side workaround dies with that process, so a later,
# separately-invoked script (e.g. a retried `comm-listen.sh --selftest`) never
# sees it and falls through to Linux-only logic that has no role on Windows).
# comm-session-skill.sh and comm-watch.sh keep their own tiny copies (they
# don't source this file, by design — comm-watch.sh in particular stays
# dependency-free); every OTHER script that already sources comm-lib.sh should
# call this one instead of re-deriving it.
_sot_is_windows() {
    case "${OS:-}" in Windows_NT) return 0 ;; esac
    case "${OSTYPE:-}" in msys*|cygwin*|win32) return 0 ;; esac
    case "$(uname -s 2>/dev/null || true)" in MINGW*|MSYS*|CYGWIN*) return 0 ;; esac
    return 1
}

PROTOCOL_VERSION=1

COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
REGISTRY="$COMM_HOME/registry.json"
INBOX_DIR="$COMM_HOME/inbox"
SELF_DIR="$COMM_HOME/self"
READ_DIR="$COMM_HOME/read"
LOCKDIR="$COMM_HOME/.registry.lock"

now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# _sot_windows_local_pipe — the LOCAL daemon's named pipe, resolved and
# proven live (ADR 0042 amendment, decision 5, corrected 2026-09-07): asks
# the daemon binary itself for its pipe path — the SAME query
# scripts/sot-local-daemon.ps1 makes (`sotd.exe session-socket-path local`)
# — so this can never derive a different name than the one the launcher's
# own daemon binds. Then proves it live with a bounded connect-then-close
# probe, mirroring that script's Test-SotPipeOpen exactly: a pipe NAME can
# persist under \\.\pipe\ while a dead client still holds a handle to it,
# so a resolvable name alone is not evidence anything is listening. Prints
# the \\.\pipe\... path and returns 0 only when both checks pass; nothing
# printed, nonzero return otherwise. Windows-only — callers gate with
# _sot_is_windows first.
# _sot_windows_sotd_exe — the sotd executable on a Windows box: SOTD_BIN when
# set, else the RUNNING daemon's own path (an installed %LOCALAPPDATA%\sot\bin\
# sotd.exe or a dev build under a checkout's target dir -- ask the OS, not a
# fixed install path), else the install path. Prints it; 1 when none exists.
_sot_windows_sotd_exe() {
    local daemon_exe="${SOTD_BIN:-}"
    if [ -z "$daemon_exe" ] || [ ! -f "$daemon_exe" ]; then
        daemon_exe="$(powershell.exe -NoProfile -NonInteractive -Command \
            "(Get-Process -Name sotd -ErrorAction SilentlyContinue | Select-Object -First 1).Path" 2>/dev/null \
            | tr -d '\r' | head -n1)"
        [ -n "$daemon_exe" ] || daemon_exe="${LOCALAPPDATA:-}/sot/bin/sotd.exe"
    fi
    [ -f "$daemon_exe" ] || return 1
    printf '%s\n' "$daemon_exe"
}
_sot_windows_local_pipe() {
    command -v powershell.exe >/dev/null 2>&1 || return 1
    local daemon_exe
    daemon_exe="$(_sot_windows_sotd_exe)" || return 1
    local raw
    raw="$("$daemon_exe" session-socket-path local 2>/dev/null | head -n1 | tr -d '\r')"
    case "$raw" in
        '\\'*'pipe'*) : ;;
        *) return 1 ;;
    esac
    local name="${raw##*\\}"
    [ -n "$name" ] || return 1
    # The name is interpolated into a PowerShell single-quoted literal:
    # refuse anything outside the daemon's own charset rather than escape it.
    case "$name" in *[!A-Za-z0-9._-]*) return 1 ;; esac
    powershell.exe -NoProfile -NonInteractive -Command "
        \$c = New-Object System.IO.Pipes.NamedPipeClientStream('.', '$name', [System.IO.Pipes.PipeDirection]::InOut)
        try { \$c.Connect(500); exit 0 } catch { exit 1 } finally { \$c.Dispose() }
    " >/dev/null 2>&1 || return 1
    printf '%s\n' "$raw"
}

# sot_relay_endpoint [EXPLICIT] — the endpoint for comm RELAY traffic (send,
# ask, listen, selftest): where the HANDLES live. A handle is registered live
# on the BACKEND daemon by its listener connection, so on a Windows box this
# is the SSH tunnel to the backend — never the local daemon's pipe, which has
# no route to a handle on another host and drops the frame without a word
# (2026-09-08: every cross-host send from a Windows session went dark the
# day discovery became pipe-first). Workspace ops, spawn and sot-fe keep
# sot_daemon_endpoint's pipe-first order: those really do target the local
# daemon. An explicit endpoint always wins, as everywhere else.
#
# Topology plan (lane D): the launcher derives this box's relay endpoint
# from `sotd topology plan --self <host>` (the hub's own socket on the
# hub, its forward tunnel elsewhere — see relay_endpoint in
# rust/protocol/src/topology.rs) and exports/persists it as
# SOT_RELAY_ENDPOINT (launch-sot.ps1). Take that when a launcher has set
# it — no more hardcoded `tcp:127.0.0.1:18743` guess, which was wrong on
# any box not literally tunneling the hub on the default port (the laptop
# fix: those sessions' sends went nowhere). A session spawned by an OLDER
# launcher has no SOT_RELAY_ENDPOINT in its env, so before guessing, ask
# the daemon itself (`sotd topology relay-endpoint`, one exec, always the
# plan's answer -- 2026-09-18, a box whose sends went to 18743 while its
# tunnel sat on another port). The hardcoded guess remains the last
# fallback for a box with no plan yet (no launcher run, or a sotd too old
# to have one).
sot_relay_endpoint() {
    local explicit="${1:-}"
    [ -n "$explicit" ] && { printf '%s\n' "$explicit"; return 0; }
    if _sot_is_windows; then
        [ -n "${SOT_RELAY_ENDPOINT:-}" ] && { printf '%s\n' "$SOT_RELAY_ENDPOINT"; return 0; }
        local exe planned
        if exe="$(_sot_windows_sotd_exe)"; then
            planned="$("$exe" topology relay-endpoint 2>/dev/null | head -n1 | tr -d '\r')"
            [ -n "$planned" ] && { printf '%s\n' "$planned"; return 0; }
        fi
        printf 'tcp:127.0.0.1:%s\n' "${SOT_PORT:-18743}"
        return 0
    fi
    sot_daemon_endpoint
}

# sot_daemon_endpoint [EXPLICIT] — resolve the control socket endpoint used by
# comm relay/spawn/FE commands. Explicit endpoints keep their old behavior; the
# socket-only default is discovered by asking sotd for the label-derived socket.
sot_daemon_endpoint() {
    local explicit="${1:-}"
    [ -n "$explicit" ] && { printf '%s\n' "$explicit"; return 0; }
    [ -n "${SOT_SOCKET:-}" ] && { printf 'unix:%s\n' "$SOT_SOCKET"; return 0; }

    # ADR 0042 amendment (2026-09-07): on a Windows box the LOCAL daemon
    # only ever listens on its named pipe — the box's loopback port is the
    # SSH tunnel OUT to the backend, never a second local listener — so
    # discovery asks for the pipe FIRST. On a probe miss (no local daemon
    # running) the tunnel to the backend is the default (Codex review
    # finding 6 on the session-start rewrite), decided HERE before the
    # pgrep-based sotd scrape below, which has no role on Windows.
    # SOT_PORT keeps its EXISTING default (18743) — never a new fixed port.
    if _sot_is_windows; then
        local pipe_path
        if pipe_path="$(_sot_windows_local_pipe)"; then
            printf 'pipe:%s\n' "$pipe_path"
            return 0
        fi
        printf 'tcp:127.0.0.1:%s\n' "${SOT_PORT:-18743}"
        return 0
    fi

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

    if ! _sot_is_windows; then
        while IFS= read -r line; do
            local pid="${line%% *}"
            case "$pid" in ''|*[!0-9]*) continue ;; esac
            [ -r "/proc/$pid/exe" ] || continue
            bin="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
            _try_sotd_socket_bin "$bin" && return 0
        done < <(pgrep -af 'sotd' 2>/dev/null || true)
    fi

    # LAST resort — development daemons launched with explicit transport
    # flags. Below the canonical session socket on purpose (2026-09-08): a
    # lane's test daemon (`--socket /tmp/sotrt-*/...`) scraped from argv
    # hijacked every comm script's discovery while the real daemon sat on
    # its label-derived socket, so despawn "found no workspace" and the
    # row survived. A scratch daemon is targeted explicitly (SOT_RELAY_ENDPOINT
    # / --endpoint), never by luck of process order. pgrep is not on a stock
    # git-bash PATH and must never be reached for on Windows.
    if ! _sot_is_windows; then
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
    fi

    return 1
}

ensure_home() {
    mkdir -p "$COMM_HOME" "$INBOX_DIR" "$SELF_DIR" "$READ_DIR"
    if [ ! -f "$REGISTRY" ]; then
        printf '{"protocol_version": %s, "agents": {}}\n' "$PROTOCOL_VERSION" > "$REGISTRY"
    fi
}

# with_lock CMD [ARGS...] — run CMD holding the registry lock (mkdir
# spinlock). CMD may be a shell function defined in this sourced lib.
#
# Bounded wait, then FAIL CLOSED (Codex review, PR #148 F2): a stale lock
# used to be force-broken after ~10s regardless of whether the replacement
# `mkdir` actually succeeded — a second waiter could enter right behind the
# "stale" holder if it was merely slow (an NFS pause, a stopped process),
# resurrecting the exact concurrent-derive/write clobber this lock exists
# to prevent, and risking a corrupt `registry.json.tmp` from two writers.
# There is no safe automatic recovery from "the lock might still be held";
# refusing and naming the lock path + holder age lets a human decide.
#
# Release is TRAP-based, not a plain post-command `rmdir` (Codex review F2
# second half / F7): a caller's `set -e` aborts the WHOLE SCRIPT the moment
# `"$@"` fails, at that exact statement — skipping every line after it in
# this function, including a plain `rmdir` written below the call. That
# leaked the lock forever on any callee failure (a corrupt registry.json
# making `registry_put`'s jq fail, for example). An EXIT trap still fires
# on that abort, so the lock comes off either way.
#
# The PRIOR EXIT trap (if any) is saved and restored, not just cleared:
# bash has one EXIT trap per shell, not a stack, and a caller may already
# have its own (e.g. comm-spawn.sh's provisional-row rollback) active
# around a with_lock call — blindly clearing it here would silently
# disarm the caller's cleanup for the rest of the script. This restore now
# runs on EVERY path, including a directly-failing "$@" (Codex review PR
# #148 round 2, finding 4): a bare `"$@"` statement under the caller's
# `set -e` used to abort the WHOLE SCRIPT right there, skipping every line
# below it in this function — the lock still came off (its own release
# trap fired on that abort), but the restore of the CALLER's prior trap
# never ran, silently losing it for the rest of the script. Capturing the
# callee's status via `if "$@"; then :; else rc=$?; fi` — the standard
# idiom for "run this and don't let -e kill us on failure" — means release
# and restore always execute before this function returns, on every path.
SOT_LOCK_MAX_TRIES=200   # ~10s at the 0.05s poll below
with_lock() {
    local tries=0
    # Test seam (F10): let a test PROVE a background waiter has reached its
    # first lock attempt, instead of racing it with a sleep. Touched once,
    # right before that attempt; unset (the default) this is a no-op.
    #
    # `touch --`, NOT `: > FILE` (Codex review round 2, finding 6): a bare
    # `>` redirect TRUNCATES whatever already sits at that path — if this
    # var ever leaked into a production environment pointed at a real
    # file, every with_lock call would zero it. `touch` only updates/
    # creates, and unlike `>` it doesn't attempt to OPEN-FOR-WRITE (which
    # would block forever against a FIFO with no reader, right here in the
    # lock's own hot path) — and `|| true` keeps a bad path from tripping
    # this function's own `set -e`-sensitive callers.
    [ -n "${SOT_COMM_TEST_LOCK_BARRIER:-}" ] && { touch -- "$SOT_COMM_TEST_LOCK_BARRIER" 2>/dev/null || true; }
    while ! mkdir "$LOCKDIR" 2>/dev/null; do
        tries=$((tries + 1))
        if [ "$tries" -gt "$SOT_LOCK_MAX_TRIES" ]; then
            local age="unknown" mtime
            mtime="$(stat -c '%Y' "$LOCKDIR" 2>/dev/null || true)"
            [ -n "$mtime" ] && age="$(( $(date +%s) - mtime ))s"
            echo "ERROR: registry lock $LOCKDIR still held after ~10s (holder age: $age) — refusing to force it: a forced takeover can let two writers corrupt registry.json.tmp, and reopens the exact clobber race this lock exists to close. If the holder is confirmed dead, remove $LOCKDIR by hand and retry." >&2
            return 1
        fi
        sleep 0.05
    done
    # Lock acquired — guarantee release via EXIT trap (see header comment),
    # preserving whatever EXIT trap the caller already had.
    local prev_trap rc=0
    prev_trap="$(trap -p EXIT)"
    trap 'rmdir "$LOCKDIR" 2>/dev/null || true' EXIT
    if "$@"; then
        :
    else
        rc=$?
    fi
    rmdir "$LOCKDIR" 2>/dev/null || true
    if [ -n "$prev_trap" ]; then
        eval "$prev_trap"
    else
        trap - EXIT
    fi
    return $rc
}

# --- registry mutators (call inside with_lock) ---
registry_put() {  # name objJSON
    # F7 (Codex review): never write an empty/blank handle — a derivation
    # bug or a corrupt-registry jq failure upstream is an ERROR, not a
    # claim of "". Last line of defense regardless of how a caller got here.
    if [ -z "$1" ]; then
        echo "registry_put: refusing to write an empty/blank handle" >&2
        return 1
    fi
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

# sot_fmt_age SECONDS -> "12s"/"6m"/"32m"/"8h"/"3d" (no "ago" suffix -- every
# caller supplies its own wording, since the same figure reads differently in
# a success line vs a timeout message). A negative age (clock skew) floors to
# 0 rather than printing garbage.
sot_fmt_age() {
    local s="$1"
    [ "$s" -lt 0 ] 2>/dev/null && s=0
    if   [ "$s" -lt 60 ];    then echo "${s}s"
    elif [ "$s" -lt 3600 ];  then echo "$((s / 60))m"
    elif [ "$s" -lt 86400 ]; then echo "$((s / 3600))h"
    else                          echo "$((s / 86400))d"
    fi
}

# sot_recipient_note HANDLE — one factual clause about a registry entry's
# recipient, for a sender's ack line (messaging ruling, 2026-09-26: "a sender
# cannot tell working from waiting-on-its-human from gone"). Reads ONLY the
# entry the sender already has open (.agents[$1] in $REGISTRY) -- no second
# file, no daemon call. Prints nothing and returns 1 the instant a needed
# fact is absent or unparseable: an annotation is a courtesy, never a guess.
#
# A STALE heartbeat (last_seen older than SOT_COMM_STALE_SECS, default 600 --
# the same threshold comm-list.sh already uses) overrides every other clause,
# because a stamp from a dead session is the misleading one.
#
# Absent that, `state` decides (comm-status.sh's own reduction, ADR 0044
# amendment): `working` gets the turn-boundary wording; the state that
# actually means "stopped, waiting on its own human to answer" is `blocked`
# (a `question` is set and `floor` is absent) -- NOT `floor == "user"`, which
# instead co-occurs with `working` (floor present => state "working"; the
# value only records who started the live turn -- comm-status.sh:97-112, the
# ADR 0044 amendment table). Every other state prints its own word
# unembellished (idle, done, waiting, or a future state this helper has never
# heard of) -- silence beats a confident wrong summary, but a real word beats
# no word.
sot_recipient_note() {
    local h="$1" row state summary status_at last_seen now
    row="$(jq -c --arg n "$h" '.agents[$n] // empty' "$REGISTRY" 2>/dev/null)" || row=""
    [ -n "$row" ] && [ "$row" != "null" ] || return 1
    state="$(printf '%s' "$row" | jq -r 'if (.state|type)=="string" then .state else empty end' 2>/dev/null)" || state=""
    [ -n "$state" ] || return 1
    summary="$(printf '%s' "$row" | jq -r 'if (.summary|type)=="string" then .summary else empty end' 2>/dev/null)" || summary=""
    status_at="$(printf '%s' "$row" | jq -r 'if (.status_at|type)=="string" then .status_at else empty end' 2>/dev/null)" || status_at=""
    last_seen="$(printf '%s' "$row" | jq -r 'if (.last_seen|type)=="string" then .last_seen else empty end' 2>/dev/null)" || last_seen=""
    now="$(date -u +%s)"

    if [ -n "$last_seen" ]; then
        local seens hb_age
        seens="$(date -u -d "$last_seen" +%s 2>/dev/null)" || seens=""
        if [ -n "$seens" ] && [ "$seens" -gt 0 ] 2>/dev/null; then
            hb_age=$((now - seens))
            if [ "$hb_age" -ge "${SOT_COMM_STALE_SECS:-600}" ]; then
                echo "no heartbeat for $(sot_fmt_age "$hb_age") -- may be gone"
                return 0
            fi
        fi
    fi

    [ -n "$status_at" ] || return 1
    local sats sat_age
    sats="$(date -u -d "$status_at" +%s 2>/dev/null)" || sats=""
    [ -n "$sats" ] && [ "$sats" -gt 0 ] 2>/dev/null || return 1
    sat_age="$(sot_fmt_age $((now - sats)))"

    case "$state" in
        working)
            echo "working, stamped ${sat_age} ago -- reply expected at its turn boundary" ;;
        blocked)
            local q; q="$(printf '%s' "$summary" | jq -Rr '.[0:60]' 2>/dev/null)" || q=""
            echo "needs its own user, stamped ${sat_age} ago: \"${q}\"" ;;
        *)
            echo "${state}, stamped ${sat_age} ago" ;;
    esac
}

# registry_del_if_provisional NAME WANT_ROOT WANT_NONCE — conditionally
# delete NAME's row, but ONLY if it's STILL provably the exact provisional
# row identified by WANT_ROOT + WANT_NONCE (status "spawning" is implied —
# a provisional row is always spawning; a real join or an explicit
# claimant always overwrites both root and status/removes the nonce as it
# writes a normal row). Call under with_lock. Exists so a spawn's rollback
# can never delete a NEWER row that has since replaced the provisional one
# (Codex review PR #148 round 2, finding 1 — reproduced by the reviewer:
# an unconditional `registry_del "$NAME"` deleted a live `status:"idle"`
# row the child had already written for real, turning a successful join
# into `null`). Returns:
#   0 — deleted (it was still ours)
#   1 — deletion itself failed (registry_del's jq/mv step)
#   2 — NOT deleted: the row no longer matches what was claimed (or
#       WANT_NONCE/NAME is empty) — left untouched; this is the common,
#       expected outcome once a real join has happened, not an error
registry_del_if_provisional() {
    local name="$1" want_root="$2" want_nonce="$3"
    local cur_status cur_root cur_nonce
    [ -n "$name" ] && [ -n "$want_nonce" ] || return 2
    cur_status="$(jq -r --arg n "$name" '.agents[$n].status // ""' "$REGISTRY" 2>/dev/null)"
    cur_root="$(jq -r --arg n "$name" '.agents[$n].root // ""' "$REGISTRY" 2>/dev/null)"
    cur_nonce="$(jq -r --arg n "$name" '.agents[$n].nonce // ""' "$REGISTRY" 2>/dev/null)"
    if [ "$cur_status" != "spawning" ] || [ "$cur_root" != "$want_root" ] || [ "$cur_nonce" != "$want_nonce" ]; then
        return 2
    fi
    registry_del "$name"
}

# --- self-file writer (shared by comm-join.sh and comm-context.sh's
# read-side self-heal) ---

# sot_write_self_file SELF_FILE NAME REPO ROOT — write the v2 self-file
# format (identity line, then `repo=`, then `root=`) to SELF_FILE via a
# same-directory temp file + checked `mv`, never an in-place `>`
# truncation (Codex review round-1 finding 3: the old in-place write left a
# read-only self-file silently unwritten — the redirection failed, nothing
# checked its exit status, and the caller went on to print a success
# message anyway — and could leave a torn/zero-byte file if interrupted
# mid-write). The temp file lives NEXT TO SELF_FILE so the final `mv` is a
# same-filesystem rename: atomic, no partial-write window a concurrent
# reader could observe.
#
# Both write sites — this self-heal path in comm-context.sh and the
# ordinary join-write in comm-join.sh — route through this ONE function so
# there is a single place that gets the atomicity right, rather than two
# copies that could drift.
#
# No cross-process lock: the self-file's "nopane" slot is deliberately
# SHARED across every no-workspace shell on a host (see
# comm-context.sh's nopane note) and is last-writer-wins BY DESIGN — every
# read of it is independently re-validated (against root=, or against
# repo=/the registry for a legacy file), so a slot two shells raced to
# write is caught on its next read rather than silently trusted either
# way. Serializing the write here would only slow down an already-safe
# race, not close a real hazard.
#
# Returns 0 only once SELF_FILE has been VERIFIABLY replaced with the new
# content; nonzero (with a reason on stderr) otherwise, and the original
# SELF_FILE is left untouched (the failed temp file is cleaned up, never
# left as e.g. a stray `.tmp.*` sibling). Callers MUST treat a nonzero
# return as "did not persist" — never report success on it. A refusal by the
# agreement guard below is return 3 specifically, so a caller can tell "this
# slot belongs to another project" from "the disk said no".
#
# sot_self_file_project_conflict SELF_FILE REPO ROOT — 0 (and one line on
# stderr naming the incumbent) when SELF_FILE already holds an identity
# claimed for a DIFFERENT project than (REPO, ROOT), 1 otherwise. This is the
# WRITE side of the read-side staleness matrix in comm-context.sh: the slot
# path is keyed by the workspace row in the ENVIRONMENT while the identity
# written into it is derived from the shell's CWD, the two derivations are
# never compared, and so any process whose $SOT_WORKSPACE_ID names one row
# while its cwd sits in another row's repo used to write that repo's handle
# into the other row's slot — after which the other session reads a handle
# that is not its own and directed mail is filed for the wrong reader (the
# inbox is keyed by handle, so the read-side matrix catches it only AFTER the
# misdelivery). `root=` decides when the slot has one; `repo=` decides for a
# legacy slot; a slot with neither (the ancient one-line format) is no
# evidence and never a conflict. The SHARED `$HOST__nopane.txt` slot is
# exempt: it is last-writer-wins BY DESIGN (see the note above), every
# no-pane shell on the host writes it whatever project it sits in, and a
# guard there would refuse ordinary use.
sot_self_file_project_conflict() {
    local self_file="$1" repo="$2" root="$3"
    case "$self_file" in *__nopane.txt) return 1 ;; esac
    [ -f "$self_file" ] || return 1
    local -a lines; mapfile -t lines < "$self_file" 2>/dev/null || return 1
    local name="${lines[0]:-}" repo_line="${lines[1]:-}" root_line="${lines[2]:-}" claim=""
    [ -n "$name" ] || return 1
    case "$root_line" in root=?*) claim="root='${root_line#root=}'"
        [ "${root_line#root=}" = "$root" ] && return 1 ;;
    esac
    if [ -z "$claim" ]; then
        case "$repo_line" in repo=?*) claim="repo='${repo_line#repo=}' (legacy slot, no root=)"
            [ "${repo_line#repo=}" = "$repo" ] && return 1 ;;
        esac
    fi
    [ -n "$claim" ] || return 1
    echo "the identity slot '$self_file' already names @$name, claimed for $claim" >&2
    return 0
}
sot_write_self_file() {
    local self_file="$1" name="$2" repo="$3" root="$4" repin="${5:-0}" tmp
    if [ "$repin" != 1 ] && sot_self_file_project_conflict "$self_file" "$repo" "$root"; then
        echo "sot_write_self_file: REFUSING to write '$name' (repo='$repo', root='$root') over it — a slot keyed by one row must not come to name another project's session, or that row reads mail addressed to this one. If this row really is '$repo' now, re-run the join with --repin." >&2
        return 3
    fi
    tmp="$(mktemp "${self_file}.tmp.XXXXXX" 2>/dev/null)" || {
        echo "sot_write_self_file: could not create a temp file next to '$self_file' (directory missing or not writable?)" >&2
        return 1
    }
    if ! printf '%s\nrepo=%s\nroot=%s\n' "$name" "$repo" "$root" > "$tmp" 2>/dev/null; then
        echo "sot_write_self_file: write to temp file '$tmp' failed (disk full? permissions?)" >&2
        rm -f "$tmp" 2>/dev/null
        return 1
    fi
    if ! mv -f "$tmp" "$self_file" 2>/dev/null; then
        echo "sot_write_self_file: could not move '$tmp' into place at '$self_file'" >&2
        rm -f "$tmp" 2>/dev/null
        return 1
    fi
    return 0
}

# --- relay bridge (started by comm-listen.sh; checked by comm-join.sh's
# stranding guard and comm-listen.sh's --status/--stop/start-check) ---
#
# The bridge is the reconnect loop `comm-relay.sh bridge --name <handle>`,
# run as a plain background child of the session's own process tree. It
# ends with the row's scope (workspace.destroy, or the run ending) — not
# with any one leg — so it survives a leg restart and is reused through
# its pidfile rather than restarted. Its loop pid is recorded in a
# pidfile under the comm state dir; "running" means
# that pid is alive AND is our loop for this handle (argv checked field by
# field, so a reused pid never counts). There is never a bridge on Windows
# (the frontend files inbound frames itself).
#
# The loop is started with a fixed argv shape — `bash -c <script>
# sot-bridge <comm-relay.sh> <handle>` — so identification is an exact
# argv comparison, not a regex over a command line.
BRIDGE_ARGV0="sot-bridge"
# $1 relay, $2 handle, $3 the OWNING agent's pid (empty = untethered), $4 the
# pidfile. The relay child blocks for as long as the connection holds, so the
# owner check cannot live at the top of the loop only: it polls alongside the
# child, and when the owner is gone it kills the child, drops the pidfile and
# exits — an ownerless bridge must not survive as a named receiver the daemon
# still counts (the false-receiver defect). argv indices 3 and 5 are unchanged
# by the two extra arguments, so sot_bridge_pid_for still identifies the loop.
BRIDGE_LOOP='while :; do
    "$1" bridge --name "$2" & _c=$!
    while kill -0 "$_c" 2>/dev/null; do
        if [ -n "${3:-}" ] && ! kill -0 "$3" 2>/dev/null; then
            kill "$_c" 2>/dev/null; rm -f "${4:-}" 2>/dev/null; exit 0
        fi
        sleep 2
    done
    if [ -n "${3:-}" ] && ! kill -0 "$3" 2>/dev/null; then rm -f "${4:-}" 2>/dev/null; exit 0; fi
    sleep 2
done'

# --- owned lifetimes: who a watcher or a bridge belongs to -------------------
#
# sot_owner_pid — the pid of the nearest ancestor whose command is `claude` or
# `codex`, walking up from $PPID. Lives HERE, not in one caller, because every
# process that outlives a turn has to end with the agent it serves, and a flag
# a caller can forget leaves exactly one leg ownerless (the Codex watch leg was
# that leg). A caller still directly attached to the agent may pass the pid it
# already knows; anything spawned without one discovers it the same way here.
# `ps -o comm=` is tried at each hop, then /proc; if neither answers the walk
# stops and prints nothing (rc 1) — which is a REFUSAL at the call site, never
# an untethered process.
sot_owner_pid() {
    local pid="${PPID:-}" comm ppid
    while [ -n "$pid" ] && [ "$pid" != "1" ]; do
        comm="$(ps -o comm= -p "$pid" 2>/dev/null | tr -d ' ')"
        if [ -z "$comm" ] && [ -r "/proc/$pid/comm" ]; then
            comm="$(tr -d ' \t\n' < "/proc/$pid/comm" 2>/dev/null)"
        fi
        [ -n "$comm" ] || return 1
        case "$comm" in
            claude|codex) printf '%s\n' "$pid"; return 0 ;;
        esac
        ppid="$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')"
        if [ -z "$ppid" ] && [ -r "/proc/$pid/status" ]; then
            ppid="$(awk '/^PPid:/{print $2}' "/proc/$pid/status" 2>/dev/null)"
        fi
        [ -n "$ppid" ] && [ "$ppid" != "$pid" ] || break
        pid="$ppid"
    done
    return 1
}

# sot_bridge_owner_pid [PID] — the pid a BRIDGE is tethered to, decided HERE
# because two callers start bridges (comm-listen.sh and the bootstrap's
# `_ensure_bridge`), and a second copy of the decision is exactly how the
# bootstrap kept reporting `down` after the listen path learned to accept an
# agentless start (CI's hermetic leg, 2026-09-26). Three tiers, in order:
#   1. an explicit pid from a caller with the better vantage,
#   2. the nearest claude/codex ancestor,
#   3. the invoking shell ($PPID — a bash builtin, so it holds on the macOS
#      and git-bash legs too, unlike a session id, which is a pid on Linux and
#      a kernel address on BSD).
# An agentless start IS legitimate — the bridge only FILES, and a human at a
# prompt is a reader — so the absence of an agent must not refuse; tier 3 fails
# SHORT, never immortal, since a caller that exits early leaves a bridge that
# dies within one poll and a `--status` that says so out loud, which is the
# property this ownership rule exists to protect. The agent tier stays FIRST:
# reverse them and every session's bridge silently shortens to the life of
# whatever launched it. Prints nothing (rc 1) only when orphaned to PPID 1 — a
# refusal at the call site, never an untethered bridge. One edge is documented
# rather than guarded: a parent that outlives everything (a systemd unit
# starting a bridge directly) would own it forever, which is the harm this rule
# exists to prevent. Nothing starts one that way today — every caller is a
# session script, a hook or a test. Add the guard the day a unit starts one.
# A WATCHER is not a bridge: it types into a pty, so an agent ancestor is its
# only correct owner and it keeps calling `sot_owner_pid` directly.
sot_bridge_owner_pid() {
    local owner="${1:-}"
    [[ "$owner" =~ ^[0-9]+$ ]] || owner="$(sot_owner_pid || true)"
    [[ "$owner" =~ ^[0-9]+$ ]] || owner="${PPID:-}"
    [[ "$owner" =~ ^[0-9]+$ ]] && [ "$owner" != 1 ] || return 1
    printf '%s\n' "$owner"
}

# sot_watcher_pid_for HANDLE — the live watcher pid recorded in
# state/<handle>.watch, verified BY IDENTITY, else nothing (rc 1). `kill -0`
# alone is not enough to act on: these markers live on a shared home and
# survive reboots, so a REUSED pid would let a teardown kill an unrelated
# process and let the start-time mutex refuse a legitimate watcher forever. The
# recorded pid must still BE a watcher for this handle — its command line names
# one of the watcher scripts and the handle itself. Anything else (gone,
# different command, no way to read one) means the marker is STALE: rc 1, and
# the caller proceeds as if it were absent.
sot_watcher_pid_for() {
    local handle="$1" pid args
    pid="$(sed -n '1p' "$COMM_HOME/state/$handle.watch" 2>/dev/null)"
    [[ "$pid" =~ ^[0-9]+$ ]] || return 1
    kill -0 "$pid" 2>/dev/null || return 1
    if [ -r "/proc/$pid/cmdline" ]; then
        args="$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)"
    else
        args="$(ps -o args= -p "$pid" 2>/dev/null)"
    fi
    [ -n "$args" ] || return 1
    case "$args" in
        *comm-wake.sh*|*comm-watch.sh*|*codex-watch.sh*) ;;
        *) return 1 ;;
    esac
    case "$args" in *"$handle"*) ;; *) return 1 ;; esac
    printf '%s\n' "$pid"
}

# --- the read cursor: a LINE OFFSET into inbox/<handle>.jsonl ----------------
#
# read/<handle>.cursor holds the NUMBER of inbox lines the recipient has been
# shown. It used to hold the newest-shown `ts`, and those stamps are
# second-resolution while every comparison was strictly-greater: a frame filed
# in the same second as one already read was never shown and never announced,
# while the sender printed a success line — a false-positive acknowledgement,
# the one thing this design may not produce. A count cannot lose a frame that
# way.
#
# sot_cursor_offset HANDLE — that offset. Three rules, each one a way this could
# otherwise go silently deaf:
#
#   * A LEGACY ts cursor is converted in memory (never written here — only a real
#     comm-poll.sh advances the cursor) by counting the lines BEFORE THE FIRST
#     one whose ts is greater than it. Not "every line at or below it": stamps
#     are only in order if every sender's clock agrees, and with a skewed clock
#     across hosts (or two frames in one second) that count would step PAST an
#     unread frame already on disk and it would never be shown. Stopping at the
#     first greater line inherits no loss at all.
#   * An unparseable line counts as read and never as a boundary. A torn append
#     is realistic on a shared filesystem, and one must not be able to freeze
#     the cursor: that makes a handle permanently deaf while its senders keep
#     printing a success line.
#   * An offset PAST the end of the inbox is 0. Production only appends, so this
#     means the file was cleared, truncated or restored by hand — exactly the
#     moment nobody suspects the cursor, and left as-is the handle never sees
#     another message.
#
# Anything unreadable yields 0. On doubt this biases LOW: showing a frame twice
# is tolerable where dropping one is not.
sot_cursor_offset() {
    local handle="$1" cur n
    # $COMM_HOME, not the source-time $READ_DIR: comm-wake.sh re-derives its
    # home inside its own main, and a helper reading a different one than its
    # caller is a silently wrong answer.
    cur="$(cat "$COMM_HOME/read/$handle.cursor" 2>/dev/null || true)"
    [ -n "$cur" ] || { printf '0\n'; return 0; }
    case "$cur" in
        ''|*[!0-9]*) ;;
        *) _sot_clamp_offset "$handle" "$cur"; return 0 ;;
    esac
    n="$(jq -Rrs --arg cur "$cur" '
        [ split("\n")[] | select(length > 0)
          | (((fromjson? // {}) | (.ts // "")) > $cur) ] as $past
        | ($past | index(true)) // ($past | length)' \
        "$COMM_HOME/inbox/$handle.jsonl" 2>/dev/null)" || n=0
    [[ "$n" =~ ^[0-9]+$ ]] || n=0
    _sot_clamp_offset "$handle" "$n"
}

# _sot_clamp_offset HANDLE N — N, or 0 when it points past the end of the inbox.
_sot_clamp_offset() {
    local total; total="$(sot_inbox_lines "$1")"
    if [ "$2" -gt "$total" ]; then printf '0\n'; else printf '%s\n' "$2"; fi
}

# sot_inbox_lines HANDLE — the inbox's line count (0 when absent).
sot_inbox_lines() {
    local n
    n="$(wc -l < "$COMM_HOME/inbox/$1.jsonl" 2>/dev/null | tr -d ' ')"
    [[ "$n" =~ ^[0-9]+$ ]] || n=0
    printf '%s\n' "$n"
}

# --- the WINDOWS FRONTEND inbox: a second file, a second cursor --------------
#
# On Windows nothing writes inbox/<handle>.jsonl — there is no listener there:
# the frontend files every inbound relay frame into its own fe-inbox.jsonl
# (comm-listen.sh's header, comm-watch.sh's Windows branch), so THAT file is the
# mail. Every reader was blind to it, and a Windows session went an hour without
# seeing two messages while comm-poll.sh printed "No new messages" from a
# per-handle file three weeks stale (field report, 2026-09-27). Two facts make it
# a different file rather than the same one under another name:
#
#   * TWO SCHEMAS. A frontend line carries `.text`; a per-handle line carries
#     `.msg`. Rewriting at this boundary is what keeps every reader downstream
#     single-schema instead of learning both.
#   * ONE FILE PER BOX, SHARED BY EVERY HANDLE. The frontend appends every frame
#     from every connection whatever its `to`, so admission is `.to == this
#     handle` STRICTLY — unlike the per-handle inbox, where a legacy line with no
#     `.to` at all is treated as directed, because that file is already this
#     handle's alone.
#
# The cursor is a LINE OFFSET like the per-handle one but in its OWN file,
# read/<handle>.fe.cursor: the two inboxes have unrelated line counts, so one
# shared cursor would silence whichever file is shorter. It has no legacy `ts`
# form to migrate — it was born a count.
#
# The platform test lives HERE and nowhere else (review, 2026-09-27: stop adding
# a Windows branch to one more reader). A reader asks for the path; off Windows
# it gets nothing and its frontend arm is simply inert.

# sot_fe_inbox_path — the frontend inbox, or NOTHING on every non-Windows box.
# Mirrors comm-watch.sh's Windows branch exactly, including the fallback chain
# for a box with no %LOCALAPPDATA%.
sot_fe_inbox_path() {
    _sot_is_windows || return 0
    printf '%s/sot/fe-inbox.jsonl\n' "${LOCALAPPDATA:-${XDG_STATE_HOME:-$HOME/.local/state}}"
}

# sot_fe_inbox_lines — the frontend inbox's line count (0 when absent, and off
# Windows).
sot_fe_inbox_lines() {
    local fe n
    fe="$(sot_fe_inbox_path)"
    [ -n "$fe" ] || { printf '0\n'; return 0; }
    n="$(wc -l < "$fe" 2>/dev/null | tr -d ' ')"
    [[ "$n" =~ ^[0-9]+$ ]] || n=0
    printf '%s\n' "$n"
}

# _sot_fe_cursor_offset HANDLE — this handle's frontend read offset. 0 when
# unset, unreadable, non-numeric, or PAST the end of the file: production only
# appends, so an offset past the end means the file was cleared, truncated or
# restored by hand — exactly the moment nobody suspects the cursor, and left
# as-is this handle never sees another frontend frame.
_sot_fe_cursor_offset() {
    local cur total
    cur="$(cat "$COMM_HOME/read/$1.fe.cursor" 2>/dev/null || true)"
    [[ "$cur" =~ ^[0-9]+$ ]] || cur=0
    total="$(sot_fe_inbox_lines)"
    [ "$cur" -gt "$total" ] && cur=0
    printf '%s\n' "$cur"
}

# sot_fe_unread_lines HANDLE — the frontend lines past HANDLE's frontend cursor
# that are addressed to HANDLE, one compact JSON object per line, rewritten into
# the per-handle schema (`.text` -> `.msg`). Nothing off Windows.
#
# An unparseable line is SKIPPED, never fatal: a torn append is realistic and the
# reader advances its cursor past it, so it counts as read exactly like a torn
# per-handle line — one bad line must not be able to pin a cursor and leave a
# handle permanently deaf. Provenance filters (self-echo, __selftest__) stay with
# the readers, which already apply them to the per-handle schema this output now
# shares.
sot_fe_unread_lines() {
    local handle="$1" fe pos total
    fe="$(sot_fe_inbox_path)"
    [ -n "$fe" ] && [ -r "$fe" ] || return 0
    pos="$(_sot_fe_cursor_offset "$handle")"
    total="$(sot_fe_inbox_lines)"
    [ "$total" -gt "$pos" ] || return 0
    sed -n "$((pos + 1)),${total}p" "$fe" 2>/dev/null \
        | jq -Rc --arg me "$handle" '
            (fromjson? // empty) | select(type == "object")
            | select((.to // "") == $me)
            | . + {msg: (.msg // .text // "")}' 2>/dev/null || true
}

sot_bridge_pidfile() { printf '%s/state/bridge-%s.pid\n' "$COMM_HOME" "$1"; }

# sot_bridge_pid_for NAME — print the live loop pid for NAME (rc 0), or
# nothing (rc 1) when the pidfile is absent, stale, or names another process.
sot_bridge_pid_for() {
    local name="$1" pid
    _sot_is_windows && return 1
    pid="$(cat "$(sot_bridge_pidfile "$name")" 2>/dev/null)" || return 1
    [ -n "$pid" ] && [ -r "/proc/$pid/cmdline" ] || return 1
    local -a argv=()
    mapfile -d '' argv < "/proc/$pid/cmdline" 2>/dev/null
    [ "${argv[3]:-}" = "$BRIDGE_ARGV0" ] && [ "${argv[5]:-}" = "$name" ] || return 1
    printf '%s\n' "$pid"
}

sot_bridge_running_for() { sot_bridge_pid_for "$1" >/dev/null; }

# _sot_bridge_pattern NAME — end-anchored pgrep -f pattern matching every
# bridge process for NAME under this uid: the loop's relay child, a bridge
# someone ran by hand, the loop of a bridge a previous release wrapped in a
# tmux session (its shell quoted the handle, hence the optional quotes), and
# the current loop itself — `bash -c ... sot-bridge <relay> NAME`, which
# never contains "comm-relay.sh bridge --name" as one substring so it needs
# its own alternative. Every non-alphanumeric character of NAME is escaped.
_sot_bridge_pattern() {
    local escaped; escaped="$(printf '%s' "$1" | sed 's/[^A-Za-z0-9]/\\&/g')"
    printf "(comm-relay\\\\.sh'? bridge --name '?%s'?(;|\$)|%s [^ ]+ %s( |\$))" \
        "$escaped" "$BRIDGE_ARGV0" "$escaped"
}

# sot_bridge_pids_for NAME — pids (this uid only) of every bridge process
# for NAME OTHER than the recorded loop: the loop's own relay child plus any
# stray. comm-listen.sh kills these on --stop and before starting a fresh
# loop, so a bridge from before the pidfile (or one whose pidfile was lost)
# never files the same frame twice next to the new one.
sot_bridge_pids_for() {
    local name="$1"
    _sot_is_windows && return 1
    pgrep -u "$(id -u)" -f "$(_sot_bridge_pattern "$name")" 2>/dev/null
}

# sot_bridge_stop NAME — stop the loop (first, so it cannot respawn its
# child), then every bridge process for NAME, and drop the pidfile.
sot_bridge_stop() {
    local name="$1" pid
    if pid="$(sot_bridge_pid_for "$name")"; then kill "$pid" 2>/dev/null || true; fi
    # shellcheck disable=SC2046
    kill $(sot_bridge_pids_for "$name") 2>/dev/null || true
    rm -f "$(sot_bridge_pidfile "$name")"
}

# sot_bridge_start NAME RELAY_SH [OWNER_PID] — stop any stray first, then start the
# loop detached from this shell's stdio (a caller's pipe must never be held
# open by it) and record its pid. Output goes to state/bridge-NAME.log,
# truncated at each start.
sot_bridge_start() {
    local name="$1" relay="$2" owner="${3:-}" log lockdir held=0 spins=0
    mkdir -p "$COMM_HOME/state"
    # Codex review (PR 254): stop-then-start is a read-modify-write over
    # one pidfile, and two bootstraps racing it (a session's own and a
    # hook, say) each cleared the strays and each started a loop -- every
    # inbound frame filed twice, and the loop that lost the pidfile race
    # left behind as an unkillable stray. `mkdir` is the portable atomic
    # test-and-set (bash 3.2 on macOS has no `{fd}` allocation, and the
    # home is NFS): whoever creates the directory owns the start.
    #
    # After ~5s we proceed WITHOUT the lock rather than refuse to start:
    # a crashed holder must never be able to make a session permanently
    # deaf, and starting unlocked is exactly the behaviour this had
    # before the lock existed.
    lockdir="$COMM_HOME/state/bridge-$name.lock.d"
    while :; do
        if mkdir "$lockdir" 2>/dev/null; then held=1; break; fi
        spins=$((spins + 1))
        [ "$spins" -ge 50 ] && break
        sleep 0.1
    done
    sot_bridge_stop "$name"
    log="$COMM_HOME/state/bridge-$name.log"
    : > "$log"
    bash -c "$BRIDGE_LOOP" "$BRIDGE_ARGV0" "$relay" "$name" "$owner" \
        "$(sot_bridge_pidfile "$name")" </dev/null >>"$log" 2>&1 &
    printf '%s\n' "$!" > "$(sot_bridge_pidfile "$name")"
    [ "$held" = 1 ] && rmdir "$lockdir" 2>/dev/null
    return 0
}

# --- live delivery into a workspace row (comm-send.sh, comm-bootstrap.sh,
# codex-watch.sh) ---
#
# sot_pty_input WORKSPACE_ID DATA_B64 — one `pty.input` request (enter:true)
# to the daemon at ENDPOINT (caller's scope); prints the response line.
# The daemon types the text into the row's capsule and appends Enter, and
# reports `enter_sent`. This is the only live-delivery path: a message
# reaches a session by its workspace row or stays in the durable inbox.
sot_pty_input() {
    local wsid="$1" data="$2" frame
    # base64 can begin with "/" (MSYS2 path conversion): --rawfile, never --arg.
    local _data_file; _data_file="$(sot_jq_rawfile "$data")" || return 1
    frame="$(jq -nc --arg w "$wsid" --rawfile d "$_data_file" \
        '{v:1,id:1,kind:"req",op:"pty.input",payload:{workspace_id:$w,data_b64:$d,enter:true}}')"
    local rc=$?
    rm -f "$_data_file"
    [ "$rc" -eq 0 ] || return 1
    # ~18s is the daemon's own worst case for one enter=true write.
    SOT_SEND_TIMEOUT="${SOT_SEND_TIMEOUT:-20}" sot_oneshot_request "$frame" "pty.input"
}

# sot_pty_screen WORKSPACE_ID — one `pty.screen` request (no scrollback,
# current screen only) to the daemon at ENDPOINT (caller's scope); prints
# the response line. Lifted out of sot-fe's send_pty_screen (ADR 0042
# amendment) so comm-wake.sh's prompt-free gate and sot-fe share the one
# implementation instead of two frame-builders drifting apart.
sot_pty_screen() {
    local wsid="$1" frame
    frame="$(jq -nc --arg w "$wsid" '{v:1,id:1,kind:"req",op:"pty.screen",payload:{workspace_id:$w}}')"
    # No local default here (fixed 2026-09-17): sot_oneshot_request's own
    # fallback chain is SOT_SEND_TIMEOUT -> SEND_TIMEOUT -> 10. Presetting
    # SOT_SEND_TIMEOUT=10 here shadowed a caller-set SEND_TIMEOUT (sot-fe
    # screen --timeout N exports SEND_TIMEOUT, then its own error text quotes
    # SEND_TIMEOUT), so the flag was silently ignored. Let the callee's own
    # fallback apply unmangled.
    sot_oneshot_request "$frame" "pty.screen"
}

# sot_pty_input_gated WORKSPACE_ID DATA_B64 — sot_pty_input, but only into a
# row that is sitting at a FREE prompt. Typing plus Enter submits a turn, so a
# row with a dialog, a menu or a half-typed draft on screen must never be
# typed into: the keystrokes would land in whatever is open. The test is
# `sot_prompt_free` above — the CURSOR's position, not the line's text — so the
# sender's poke (comm-send.sh) and the ping watcher share ONE implementation
# instead of two that drift. Returns 0 typed, 1 screen read but NOT free (also an
# accepted-but-not-ok row — nothing was typed either way), 2 no reply at all
# (transport error/empty response, kept distinct so a caller can tell a busy
# row from a dead daemon).
# sot_prompt_free SCREEN_JSON — 0 when the row is sitting at an EMPTY input
# line with the cursor at its start. THE prompt test: both the sender's poke
# (sot_pty_input_gated) and the ping watcher's gate read it, so "free prompt"
# cannot come to mean two different things.
#
# It is the CURSOR that decides, not the text. pty.screen returns plain text
# with every attribute stripped (capsule_workspace.rs maps the vt100 rows
# through trim_end), so a grey PROMPT SUGGESTION — ghost text Claude Code
# draws after the insertion point on an empty input — is byte-identical to a
# half-typed human draft. Matching the text alone therefore held every wake
# for as long as a suggestion sat on screen: a row went deaf for a day with a
# live watcher and no signal (2026-09-25). The cursor separates them with no
# new protocol: ghost text leaves the cursor AT the input start, typed text
# pushes it past what was typed. Anchoring on the cursor's own line also
# closes the opposite hole — a bare `❯` anywhere on screen (in output, or
# behind a permission dialog) used to open the gate.
#
# FREE means all of: a cursor is present; its row indexes a real line; that
# line carries `❯` with nothing but spaces before it; and the cursor column
# is at the insertion point. That last part does NOT guess which convention
# the renderer uses, because guessing is unsound in both directions. A renderer
# that draws a separator (`❯ text` — the measured one: a live row showed the
# glyph at column 0 and the cursor at column 2, with a NON-BREAKING space
# between) puts an empty input's cursor at glyph+2; one that draws none
# (`❯text`) puts it at glyph+1, and puts a ONE-CHARACTER DRAFT at glyph+2.
# So glyph+2 alone is not "empty" and neither is the pair: accept both columns
# blindly and a one-character draft on a no-separator prompt reads FREE and
# gets submitted, which is the exact harm this gate exists to prevent.
# The screen already carries the answer, so READ it instead: glyph+1 is always
# the insertion point, and glyph+2 is the insertion point only when the cell
# between glyph and cursor is a separator — a space, a non-breaking space, a
# tab, or ABSENT (the backend trims trailing whitespace, so an empty prompt
# under the separator convention arrives as a bare `❯` with nothing at
# glyph+1). A typed character there is not a separator, and the gate holds.
# The all-spaces prefix test above is what makes the byte offset `index`
# returns safe to reuse as a codepoint index here: an all-ASCII prefix has
# both the same. Everything else is NOT free: no
# cursor (never happens on a healthy capsule row — the field has existed
# since the op was born, and the only other runtime answers with an error
# payload and no lines at all), a row out of range, an error payload, a
# malformed reply, or an empty string. The glyph is written `❯` so the
# jq PROGRAM stays pure ASCII across the git-bash leg's native jq.
sot_prompt_free() {
    # jq exits 0 on EMPTY stdin, which would read as "free" — a screen we
    # never saw is never a free prompt.
    [ -n "$1" ] || return 1
    printf '%s' "$1" | jq -e '
        (.payload // {}) as $p
        | ($p.lines // []) as $L
        | ($p.cursor // {}) as $k
        | ($k.row // -1) as $r
        | ($k.col // -1) as $c
        | if ($r|type) != "number" or ($c|type) != "number"
             or $r < 0 or $r >= ($L|length) then false
          else ($L[$r]) as $line
             | ($line | index("\u276f")) as $g
             | ($g != null)
               and (($line[0:$g] | test("[^ ]")) | not)
               and ( $c == $g + 1
                     or ( $c == $g + 2
                          and ( $line[$g+1:$g+2]
                                | . == "" or . == " " or . == "\u00a0" or . == "\t" ) ) )
          end
    ' >/dev/null 2>&1
}

sot_pty_input_gated() {
    local wsid="$1" data="$2" screen resp
    screen="$(sot_pty_screen "$wsid" 2>/dev/null)"
    [ -n "$screen" ] || return 2
    sot_prompt_free "$screen" || return 1
    resp="$(sot_pty_input "$wsid" "$data" 2>/dev/null || true)"
    [ -n "$resp" ] || return 2
    printf '%s' "$resp" | jq -e '.payload.ok == true' >/dev/null 2>&1 || return 1
    return 0
}

# sot_capsule_workspace_id — print the calling shell's capsule row id, or
# print nothing and return 1 when this isn't a capsule row. $SOT_WORKSPACE_ID
# wins when set; otherwise it's read out of $SOT_COMM_SELF_FILE's basename
# (comm-context.sh names it "<host>__<workspace_id>.txt" — a real capsule
# row today has the self file pinned but not the id itself in its env, so
# the basename is the only place it survives). "nopane" is the literal
# placeholder comm-context.sh writes for a non-capsule shell, never a real
# id — treated the same as absent. Shared by comm-wake.sh (its own startup
# gate, rule: exit 3 when this fails) and comm-session-start.sh (deciding
# whether to auto-start a ping watcher at all).
sot_capsule_workspace_id() {
    if [ -n "${SOT_WORKSPACE_ID:-}" ]; then
        printf '%s\n' "$SOT_WORKSPACE_ID"
        return 0
    fi
    local base="${SOT_COMM_SELF_FILE:-}"
    [ -n "$base" ] || return 1
    base="$(basename "$base")"
    case "$base" in
        *__*.txt) ;;
        *) return 1 ;;
    esac
    local id="${base#*__}"
    id="${id%.txt}"
    [ -n "$id" ] && [ "$id" != "nopane" ] || return 1
    printf '%s\n' "$id"
}
# --- MSYS2 argv-conversion guard for jq values that can legitimately
# start with "/" ---
#
# capsule-comm-identity fix, field-measured on a Windows git-bash box: when
# a NATIVE (non-MSYS) jq.exe is invoked, MSYS2's argv-to-Windows-path
# conversion rewrites any ARGV ELEMENT that starts with "/" into a Windows
# path before jq ever sees it — verified through the real relay, a message
# beginning "/sot-session-start ..." arrived in the peer's inbox mangled
# into a filesystem path. The rule is exact and easy to miss in ad hoc
# testing: only the FIRST character of the whole argument matters (a bare
# "/" corrupts too); "./", "~/", "//server", and every MID-string slash
# are untouched; on a MULTI-LINE value only the FIRST line is mangled —
# every later line survives intact, which is why this read as an isolated
# typo rather than a systematic corruption. `--arg NAME "$value"` passes
# $value as its own argv element, so any of the THREE values in this
# codebase that can genuinely start with "/" — a message body
# (comm-send.sh, comm-relay.sh) and a project root (comm-join.sh) — must
# never go through `--arg`. Every OTHER `--arg` (handles, hosts, ids,
# timestamps) is drawn from a restricted charset that can never start
# with "/" and is untouched by this fix.
#
# sot_jq_rawfile VALUE — writes VALUE verbatim (no added newline) to a
# fresh temp file and prints its path, for use as `jq --rawfile NAME
# <path> ...` in place of `--arg NAME "$VALUE"`: the risky VALUE now lives
# in the file's CONTENT, read by jq via fread — never an argv element, so
# never subject to the conversion. The file PATH argument itself is left
# as an ordinary argument and SHOULD still convert when it starts with
# "/" (that's the well-behaved half of the same MSYS2 mechanism — it's
# what lets a native jq.exe find the file at all), so no
# MSYS2_ARG_CONV_EXCL or similar exclusion is involved. The caller owns
# the returned path and MUST `rm -f` it once jq has run.
sot_jq_rawfile() {
    local f
    f="$(mktemp "${TMPDIR:-/tmp}/sot-comm-jq.XXXXXX" 2>/dev/null)" || {
        echo "sot_jq_rawfile: could not create a temp file for a jq --rawfile value" >&2
        return 1
    }
    if ! printf '%s' "$1" > "$f" 2>/dev/null; then
        echo "sot_jq_rawfile: write to temp file '$f' failed" >&2
        rm -f "$f" 2>/dev/null
        return 1
    fi
    printf '%s' "$f"
}

# --- sender identity: NAME resolved is not enough, it must be ROUTABLE ---
#
# Codex review round-2 finding 4/C: comm-send.sh, comm-relay.sh, and
# comm-bootstrap.sh each carried their OWN "resolved identity" check, and
# each treated a merely NONEMPTY $NAME as good enough. That's insufficient:
# a self-file can resolve NAME locally (it passed comm-context.sh's own
# root=/repo= validation) while the registry itself has no matching row
# for it at all (evicted, or a failed registry write) or — worse — a row
# whose root belongs to a DIFFERENT project (this handle was reclaimed
# elsewhere). Either way, sending under it stamps a from-handle a reply
# can't route back to, or routes it to the wrong session. "Resolved" now
# means ROUTABLE: NAME nonempty AND a registry row for it exists AND (that
# row's root is empty — a legacy row, allowed during the migration window
# — OR it matches this project's canonical root).
#
# ONE helper, called by all three scripts, replacing three separately
# drifting diagnostic essays with the invariant plus the exact recovery
# command. Must be called BEFORE any endpoint/socket/transport resolution
# (Codex review round-2 SHOULD-FIX 2) so an unresolved sender always sees
# THIS refusal, never an unrelated daemon/socket error.
#
# Prints nothing and returns 0 if routable. Prints ONE refusal line and
# returns 1 otherwise. Depends on NAME/PROJECT_ROOT already being set by
# `eval "$(comm-context.sh)"` — call after that, never before.
sot_require_routable_identity() {
    if [ -z "${NAME:-}" ]; then
        echo "ERROR: your sot-comm identity did not resolve — refusing to send with no verifiable from-handle (a reply would silently misroute). Join first: comm-join.sh --name <canonical-handle> (never a bare comm-join.sh if you previously held one — see the sot-session-start skill's recovery recipe)." >&2
        return 1
    fi
    local reg_status reg_root qname qdir
    IFS=$'\t' read -r reg_status reg_root <<< "$(sot_registry_entry_status "$NAME")"
    # %q-quote the handle AND the executable path (Codex review round-3
    # finding 7): a raw $NAME/$SCRIPT_DIR interpolation produces a wrong
    # or unsafe copy-paste command for a handle or install path containing
    # spaces/metacharacters. $SCRIPT_DIR is this HELPER's caller's own
    # directory (every caller sources comm-lib.sh after setting it).
    qname="$(printf '%q' "$NAME")"
    qdir="$(printf '%q' "${SCRIPT_DIR:-.}")"
    if [ "$reg_status" != "present" ]; then
        echo "ERROR: your sot-comm identity '@$NAME' has no registry row — refusing to send with an unroutable from-handle (a reply would silently misroute). Reclaim it: $qdir/comm-join.sh --name $qname" >&2
        return 1
    fi
    if [ -n "$reg_root" ] && [ "$reg_root" != "${PROJECT_ROOT:-}" ]; then
        echo "ERROR: your sot-comm identity '@$NAME' is registered to a DIFFERENT project's root ('$reg_root') — refusing to send with a misrouting from-handle. Reclaim it: $qdir/comm-join.sh --name $qname" >&2
        return 1
    fi
    return 0
}

# --- derived-handle disambiguation (ADR 0028 addendum: "derived vs
# explicit") --- single home for the algorithm; comm-join.sh and
# comm-spawn.sh both call sot_derive_handle. This is ONLY for a name that
# comes from DERIVATION (the default <basename>-<host>): a caller must never
# route an explicit --name, $SOT_COMM_NAME, or an already-joined self-file
# identity through here — those stay verbatim, unconditionally.

# sot_canonical_path PATH — absolute, symlink-resolved path (the
# "canonical project root" the disambiguation compares), or NOTHING on
# stdout plus a nonzero return if one can't be established (Codex review
# F8). NEVER falls back to an unresolved/relative path: two callers that
# each `cd` into a differently-spelled relative path (or the SAME literal
# "./foo" from two different directories) would otherwise compare as
# identical roots, defeating the whole disambiguation.
sot_canonical_path() {
    local p="$1" out
    if command -v realpath >/dev/null 2>&1; then
        if out="$(realpath -- "$p" 2>/dev/null)" && [ -n "$out" ]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    if command -v readlink >/dev/null 2>&1; then
        if out="$(readlink -f -- "$p" 2>/dev/null)" && [ -n "$out" ]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    echo "sot_canonical_path: could not resolve a canonical path for '$p' (no working realpath or readlink -f) — refusing to record a relative/unresolved project root" >&2
    return 1
}

# sot_hash6 STR — first 6 hex chars of sha256(STR); stable per input, used
# only for the last-resort hash-qualified handle tier. NO fallback when
# sha256sum/shasum are both missing (Codex review F8 / simplicity audit):
# the earlier `cksum` fallback was a variable-length decimal CRC, not a
# hash6 — installing sha256sum later would silently change that root's
# tier-3 handle. Fail loudly instead; the caller surfaces this as a hard
# error asking for an explicit --name.
#
# Both the pipeline's exit status AND the shape of its output are checked
# (Codex review PR #148 round 2, finding 5): the previous version ran
# `return 0` unconditionally after each pipeline, so an INSTALLED-but-
# FAILING sha256sum (confirmed: one that exits 23) still "succeeded" with
# an EMPTY hash, producing a `<base>--<host>` handle instead of a loud
# failure. `rc=$?` right after the pipeline reflects its real exit status
# under this shell's `pipefail` (both callers set it); the regex is the
# stronger, direct check — it also catches a tool that exits 0 but emits
# garbage, which an exit-code check alone would miss.
sot_hash6() {
    local out rc
    if command -v sha256sum >/dev/null 2>&1; then
        out="$(printf '%s' "$1" | sha256sum | cut -c1-6)"; rc=$?
        if [ "$rc" -eq 0 ] && [[ "$out" =~ ^[0-9a-f]{6}$ ]]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    if command -v shasum >/dev/null 2>&1; then
        out="$(printf '%s' "$1" | shasum -a 256 | cut -c1-6)"; rc=$?
        if [ "$rc" -eq 0 ] && [[ "$out" =~ ^[0-9a-f]{6}$ ]]; then
            printf '%s\n' "$out"
            return 0
        fi
    fi
    echo "sot_hash6: no working sha256sum/shasum produced a valid 6-hex-character digest — cannot compute a stable tier-3 handle qualifier" >&2
    return 1
}

# sot_slug LABEL — bash mirror of rust/backend/src/paths.rs::slug (Codex
# review PR #148 round 2, finding 3): lowercase; '.' -> '_' BEFORE the
# keep-check; a RUN of characters outside [a-z0-9_-] collapses to a single
# '-' (a LITERAL '-'/'_'/alnum in the input is pushed as-is and never
# collapsed, even if repeated — matching Rust's keep/else branch split
# exactly, not a blanket dash-collapse); trailing '-' trimmed; empty ->
# "default". Verified against every example in that function's own doc
# comment (MyPackage.jl -> mypackage_jl, "Foo Bar" -> foo-bar, /abs/path
# -> abs-path, "  " -> default) plus literal-repeated-dash and leading-
# junk cases. Needed because workspace.create's same-slug path is an
# intentional metadata-refresh idempotence, not an error — two labels
# that only differ by case, or by a dot vs underscore, resolve to the
# SAME workspace and must be caught as a collision too, not just a
# byte-identical label match.
sot_slug() {
    local label="$1" out="" last_dash=false i len ch c
    len=${#label}
    for (( i = 0; i < len; i++ )); do
        ch="${label:i:1}"
        c="$(printf '%s' "$ch" | tr '[:upper:]' '[:lower:]')"
        [ "$c" = "." ] && c="_"
        case "$c" in
            [a-z0-9_-])
                out="${out}${c}"
                if [ "$c" = "-" ]; then last_dash=true; else last_dash=false; fi
                ;;
            *)
                if [ "$last_dash" = false ] && [ -n "$out" ]; then
                    out="${out}-"
                    last_dash=true
                fi
                ;;
        esac
    done
    while [[ "$out" == *- ]]; do out="${out%-}"; done
    [ -z "$out" ] && out="default"
    printf '%s\n' "$out"
}

# sot_sanitize_component STR [MAXLEN=20] — reduce STR to the
# workspace.create charset [A-Za-z0-9._-] (every other byte becomes '-',
# runs of '-' collapse, leading/trailing '-' trimmed) and clamp it to
# MAXLEN (Codex review F4): a repo/parentdir basename can contain spaces,
# Unicode, or shell metacharacters, none of which workspace.create's name
# validator (`rust/backend/src/handlers.rs`, `valid_name`) accepts — and an
# unsanitized basename reaching the `--no-workspace` launcher string is a
# shell-injection vector. Applied to EVERY raw piece (basename, parentdir,
# host) BEFORE composing a candidate, never to the assembled candidate
# afterward, so composed separators can't be reintroduced or hidden behind
# a length overflow. MAXLEN defaults to 20 so the worst-case composed
# candidate (20 + "-" + 20 + "-" + 20 = 62) stays inside workspace.create's
# 64-char limit without per-tier budget arithmetic.
sot_sanitize_component() {
    local s="$1" max="${2:-20}"
    s="$(printf '%s' "$s" | tr -c 'A-Za-z0-9._-' '-')"
    while [[ "$s" == *--* ]]; do s="${s//--/-}"; done
    s="${s#-}"; s="${s%-}"
    s="${s:0:$max}"
    s="${s%-}"
    [ -z "$s" ] && s="x"
    printf '%s' "$s"
}

# sot_registry_entry_status NAME — tagged status of the registry row for
# NAME (Codex review simplicity audit: replaces a magic sentinel string
# with tagged output, so "no row" and "row present but root unknown" can
# never be confused with each other or with an actual, if empty, root
# value):
#   "absent\t"          — NAME has no row at all
#   "present\t<root>"   — NAME has a row; <root> is "" for a legacy row
#                         that predates this feature (unknown root)
#   "error\t"           — the registry could not be read/parsed (Codex
#                         review round-3 finding 1): jq failing (malformed
#                         JSON, unreadable file, an NFS hiccup) used to
#                         print NOTHING, which read back as an empty
#                         string indistinguishable from "absent" to every
#                         caller — letting a pane-keyed legacy self-file
#                         self-heal on a basename match with the registry
#                         effectively unconsultable. Callers MUST treat
#                         "error" as NO EVIDENCE, never as "absent".
sot_registry_entry_status() {
    local out
    out="$(jq -r --arg n "$1" \
        'if (.agents | has($n)) then "present\t" + (.agents[$n].root // "") else "absent\t" end' \
        "$REGISTRY" 2>/dev/null)"
    if [ $? -ne 0 ] || [ -z "$out" ]; then
        printf 'error\t\n'
        return 0
    fi
    printf '%s\n' "$out"
}

# _sot_tier_claimable MODE ROOT STATUS HELD_ROOT — true if a tier whose
# registry status is STATUS/HELD_ROOT (from sot_registry_entry_status) can
# be claimed under MODE:
#   reclaim — unclaimed, OR already held by MY OWN root (today's
#             comm-join rejoin/reclaim behavior).
#   fresh   — unclaimed ONLY. comm-spawn creates a NEW agent; an existing
#             row for the resolved name — even one sharing my root — is
#             someone/something else's from spawn's point of view and must
#             never be silently absorbed (Codex review F3: this used to
#             erase a LIVE agent's workspace_id/status fields when spawning a
#             second time against the same project root).
_sot_tier_claimable() {
    local mode="$1" root="$2" status="$3" held="$4"
    [ "$status" = "absent" ] && return 0
    [ "$mode" = "reclaim" ] && [ "$held" = "$root" ] && return 0
    return 1
}

# sot_derive_handle MODE ROOT HOST — the derived-name algorithm (ADR 0028
# addendum; see docs/adr/0028-remote-comm-autoconnect.md). MODE is
# "reclaim" (comm-join) or "fresh" (comm-spawn) — see _sot_tier_claimable.
# Liveness is deliberately NOT consulted — a stale registry row still
# holds its claim until existing cleanup paths remove it; this keeps the
# rule one-dimensional (root comparison only) and prevents handle
# flip-flop between two projects depending on who happens to be running.
#
# ROOT MUST already be canonical (sot_canonical_path) — canonicalizing
# here would mean filesystem traversal INSIDE the registry lock (this runs
# from claim_derived_handle, under with_lock), which is worse under a
# dead/slow NFS mount and was flagged as redundant (Codex review F8:
# "canonicalize once before entering the lock").
#
# Escalates through three tiers, each checked with _sot_tier_claimable —
# tier 3 is NOT an unconditional overwrite (Codex review F6: it used to be
# claimed regardless of who held it, which needs no hash collision at all
# to hit — an explicit owner of the computed hash-qualified name was
# silently overwritten). If no tier is claimable, this FAILS LOUDLY
# (nonzero return, nothing on stdout, a clear reason on stderr) rather than
# inventing a fourth tier or overwriting anything — the caller must ask
# the user for an explicit --name.
#
# Every raw piece (repo basename, parentdir, host) is sanitized+clamped
# (sot_sanitize_component) BEFORE composing any candidate (Codex review
# F4), so a derived handle can never diverge from what workspace.create
# will accept, and no raw path text reaches a shell command unsanitized.
# HOST gets an extra step (Codex review PR #148 round 2, finding 7): if
# sanitizing/clamping CHANGES it at all — a long or characters-outside-
# charset hostname got truncated/rewritten — a short digest of the RAW
# host is appended. Without this, two DIFFERENT real hosts whose names
# happen to sanitize/truncate to the IDENTICAL string would, if they ever
# shared a root (an NFS-shared repo, exactly this cluster's own shape),
# alias onto one tier-1 "reclaim" — root matches, and the (now-identical)
# host component can no longer tell them apart. An untouched host (the
# overwhelmingly common case: short, already-valid hostnames) gets no
# suffix, so today's handles are unchanged. HOST_RAW_MAX=12 leaves room
# for "-" + a 6-hex digest without the host component threatening
# sot_sanitize_component's 20-char default budget the other components
# still use (12 + 1 + 6 = 19 worst case).
#
# On success, prints THREE lines: handle, qualifier, tier1 (qualifier empty
# at tier 1, "<parentdir>" at tier 2, "<hash6>" at tier 3; tier1 is ALWAYS
# the bare "<base>-<host>" handle, win or lose, so a caller can tell whether
# this call escalated AWAY from it — comm-join.sh's stranding guard needs
# exactly that). NEWLINE-separated, not tab-separated (Codex review round-1
# finding 1): tab is one of bash's IFS-WHITESPACE characters, so `IFS=$'\t'
# read -r a b c` still COLLAPSES adjacent tabs exactly like the default
# space/tab/newline splitting does — an empty qualifier field (the tier-1
# case, `tier1<TAB><TAB>tier1`) vanished entirely instead of reading back as
# "", shifting tier1's value into qualifier and leaving CLAIMED_TIER1 empty.
# Every ordinary tier-1 spawn then misread CLAIMED_QUALIFIER as non-empty
# and comm-spawn.sh synthesized a wrong "qualified" display label. Reading
# one line per `read -r VAR` sidesteps this: with a single destination
# variable there is no splitting to collapse — the whole line, empty or
# not, becomes that variable's value verbatim. A caller that only wants the
# handle: `NAME="$(sot_derive_handle reclaim "$ROOT" "$HOST" | head -n1)"`.
sot_derive_handle() {
    local mode="$1" root="$2" raw_host="$3"
    local base parent hash6 tier1 tier2 tier3 host host_digest
    local status1 held1 status2 held2 status3 held3 shown1 shown2 shown3

    case "$mode" in
        reclaim|fresh) : ;;
        *) echo "sot_derive_handle: invalid mode '$mode' (want reclaim or fresh)" >&2; return 1 ;;
    esac

    base="$(sot_sanitize_component "$(basename "$root")")"
    host="$(sot_sanitize_component "$raw_host" 12)"
    if [ "$host" != "$raw_host" ]; then
        host_digest="$(sot_hash6 "$raw_host")" || return 1
        host="${host}-${host_digest}"
    fi

    tier1="${base}-${host}"
    IFS=$'\t' read -r status1 held1 <<< "$(sot_registry_entry_status "$tier1")"
    if _sot_tier_claimable "$mode" "$root" "$status1" "$held1"; then
        printf '%s\n%s\n%s\n' "$tier1" "" "$tier1"
        return 0
    fi
    shown1="$held1"; [ -z "$shown1" ] && shown1="an unknown project"

    parent="$(sot_sanitize_component "$(basename "$(dirname "$root")")")"
    tier2="${base}-${parent}-${host}"
    IFS=$'\t' read -r status2 held2 <<< "$(sot_registry_entry_status "$tier2")"
    if _sot_tier_claimable "$mode" "$root" "$status2" "$held2"; then
        echo "comm: '@$tier1' is already held by $shown1 — joining as '@$tier2' instead" >&2
        printf '%s\n%s\n%s\n' "$tier2" "$parent" "$tier1"
        return 0
    fi
    shown2="$held2"; [ -z "$shown2" ] && shown2="an unknown project"

    hash6="$(sot_hash6 "$root")" || return 1
    tier3="${base}-${hash6}-${host}"
    IFS=$'\t' read -r status3 held3 <<< "$(sot_registry_entry_status "$tier3")"
    if _sot_tier_claimable "$mode" "$root" "$status3" "$held3"; then
        echo "comm: '@$tier1' (held by $shown1) and '@$tier2' (held by $shown2) are both taken — joining as '@$tier3' instead" >&2
        printf '%s\n%s\n%s\n' "$tier3" "$hash6" "$tier1"
        return 0
    fi
    shown3="$held3"; [ -z "$shown3" ] && shown3="an unknown project"
    echo "comm: every derived handle for this project is already taken — '@$tier1' (held by $shown1), '@$tier2' (held by $shown2), and '@$tier3' (held by $shown3). Pass --name to pick one explicitly." >&2
    return 1
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
# claim_derived_handle MODE ROOT HOST OBJ_JSON — derive AND registry_put
# the result as ONE critical section under the registry lock, so no other
# claim can observe registry state in between. Sets globals CLAIMED_NAME,
# CLAIMED_QUALIFIER, and CLAIMED_TIER1 (mirrors sot_derive_handle's three
# outputs) for the caller to read after this returns; all three cleared to
# "" first, so a failure never leaves a stale value from a PREVIOUS
# successful call for a careless caller to read. CLAIMED_TIER1 is what
# comm-join.sh's stranding guard compares CLAIMED_NAME against — a mismatch
# means this call escalated away from the bare handle. Both comm-join.sh
# (MODE reclaim) and
# comm-spawn.sh (MODE fresh, the provisional row) route a derived name
# through this — one shared locked claim path, not two copies of "derive,
# then lock to write" that could each get this wrong.
#
# On failure (sot_derive_handle exhausted all three tiers, or refused to
# run at all — e.g. no hash function available), returns nonzero and
# writes NOTHING to the registry (Codex review F7): derivation failure is
# an error the caller must surface, never a claim of "".
#
# _sot_claim_derived_handle is the with_lock callee; it must NEVER be
# invoked directly, and never as `X=$(with_lock ...)`. with_lock runs its
# command directly ("$@", no subshell) specifically so a callee's global
# assignment survives past its return — capturing it via command
# substitution would fork a subshell and lose CLAIMED_NAME exactly the way
# the test harness's own next_self_file() lost its counter (see
# comm/core/tests/test-join-disambiguation.sh) — the same lesson, twice.
_sot_claim_derived_handle() {  # MODE ROOT HOST OBJ_JSON — call only via with_lock
    local mode="$1" root="$2" host="$3" obj="$4" line
    CLAIMED_NAME=""
    CLAIMED_QUALIFIER=""
    CLAIMED_TIER1=""
    line="$(sot_derive_handle "$mode" "$root" "$host")" || return 1
    # Three sequential single-var reads off the SAME herestring fd (Codex
    # review round-1 finding 1) — each `read -r VAR` consumes one line and
    # advances the shared position, and with only one destination variable
    # there is no IFS splitting to collapse an empty middle field the way
    # the old tab-delimited `read -r a b c` did. `{ …; } <<< "$line"` (not
    # `( … )`) so the reads run in THIS shell and CLAIMED_* stay set for the
    # caller.
    {
        IFS= read -r CLAIMED_NAME
        IFS= read -r CLAIMED_QUALIFIER
        IFS= read -r CLAIMED_TIER1
    } <<< "$line"
    if [ -z "$CLAIMED_NAME" ]; then
        echo "claim_derived_handle: derivation returned no name — refusing to claim an empty handle" >&2
        return 1
    fi
    registry_put "$CLAIMED_NAME" "$obj"
}
claim_derived_handle() {  # MODE ROOT HOST OBJ_JSON
    with_lock _sot_claim_derived_handle "$1" "$2" "$3" "$4"
}


# sot_json_escape STR — STR as one JSON-quoted string, surrounding quotes
# included (S19: `sot_hello_frame`'s hand-rolled `"%s"` interpolation
# produced invalid JSON for a declared value containing a quote or
# backslash — reproduced against a quoted SOT_SELF_HOST override). `jq
# -Rs .` reads STR as raw text (R), slurps the whole input into one
# string even across embedded newlines (s), and prints it back as a
# single JSON string literal — the general escaping jq's own JSON writer
# already gets right, never a hand-rolled sed/printf substitution.
sot_json_escape() {
    printf '%s' "$1" | jq -Rs .
}

# sot_host — this shell's DECLARED host name for the wire only (ADR 0046
# decision 1, manager review S1/S2): the ONE resolver matching
# sot_log::state_dir::host_name() on the Rust side exactly — `$SOT_SELF_HOST`
# verbatim if set and non-empty (a NEW variable: `SOT_HOST` already means
# the SSH target a remote frontend dials, `scripts/launch-sot.ps1`/
# `launch-sot.sh` — reusing it here would silently rename a frontend's
# declared identity to whatever it dials), else the first `.`-label of
# `hostname -s`, lowercased. Feeds ONLY `sot_hello_frame`'s wire `host`
# field and display/logs — never an address or on-disk namespace: no
# on-disk namespace changes this sprint (S1), so comm-context.sh's own
# `HOST` (the self-file key, handle derivation) does NOT call this;
# `hostname -s` there stays completely independent, exactly as on main.
# Fails loudly (S19) rather than printing empty when neither source
# resolves — a hello with an empty declared host is worse than a hello
# that never sent one at all.
sot_host() {
    if [ -n "${SOT_SELF_HOST:-}" ]; then
        printf '%s\n' "$SOT_SELF_HOST"
        return 0
    fi
    local raw
    if ! raw="$(hostname -s 2>/dev/null || hostname 2>/dev/null)"; then
        echo "sot_host: no SOT_SELF_HOST override and hostname failed -- cannot declare an identity" >&2
        return 1
    fi
    raw="${raw%%.*}"
    # Trim whitespace the same way Rust's host_name() does (`.trim()`
    # after taking the first label) — manager review round 2: the two
    # implementations must apply the SAME rule, not two independently
    # coded near-matches.
    raw="${raw#"${raw%%[![:space:]]*}"}"
    raw="${raw%"${raw##*[![:space:]]}"}"
    if [ -z "$raw" ]; then
        echo "sot_host: no SOT_SELF_HOST override and hostname returned no usable label" >&2
        return 1
    fi
    printf '%s\n' "$raw" | tr '[:upper:]' '[:lower:]'
}

# sot_hello_frame [ROLE] — the ONE hello frame every comm script sends
# before any other op (ADR 0046 decision 1: a connection declares
# `{host, role, name}` once, and the daemon binds it — never recomputed
# downstream). Replaces six pasted copies of this exact literal frame
# (comm-relay.sh, comm-despawn.sh, comm-listen.sh, comm-spawn.sh, sot-fe,
# and the join-disambiguation test's own fixture) that predated `host`/
# `role`/`name` entirely and so declared nothing about the sender.
#
# ROLE overrides the default inference: comm-listen.sh's reconnect-loop
# bridge passes "bridge" explicitly (that loop's own lifetime IS what
# "bridge" means — nothing else in this tree ever is one). Every other
# caller lets this infer "agent" ($SOT_WORKSPACE set — a session running
# inside a daemon-owned workspace) or "cli" (a bare shell invocation, the
# common case for comm-relay.sh/comm-despawn.sh/comm-spawn.sh/sot-fe).
#
# `host`: `sot_host` — works whether or not the caller ran comm-context.sh
# first (comm-despawn.sh doesn't). `name`: `$NAME` when comm-context.sh
# resolved one (empty for a not-yet-joined shell — an anonymous hello,
# exactly today's behavior).
sot_hello_frame() {
    local role="${1:-}"
    if [ -z "$role" ]; then
        if [ -n "${SOT_WORKSPACE:-}" ]; then role="agent"; else role="cli"; fi
    fi
    local tok host
    tok="${SOT_TOKEN:-$(cat "${XDG_CONFIG_HOME:-$HOME/.config}/sot/token" 2>/dev/null || true)}"
    host="$(sot_host)" || return 1
    # JSON-escape every interpolated string (S19, Codex finding S19): an
    # unescaped quote or backslash in a declared host/name/token would
    # otherwise produce invalid JSON the daemon's own parser rejects.
    #
    # The `"protocol":2` literal below is sotd's WIRE protocol
    # (`sot_protocol::PROTOCOL_VERSION`, rust/protocol/src/lib.rs) — not
    # this file's own `$PROTOCOL_VERSION` (registry.json schema version,
    # unrelated). It went stale against a live daemon when the wire
    # protocol bumped 1 -> 2 and nothing here asked the binary; bump it by
    # hand alongside every future `PROTOCOL_VERSION` change until this
    # reads `sotd --version`'s trailing `protocol <N>` instead (see that
    # function's doc comment).
    printf '{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"sot-comm","last_seen_revision":0,"protocol":2,"app_version":"comm","token":%s,"host":%s,"role":%s,"name":%s}}\n' \
        "$(sot_json_escape "$tok")" "$(sot_json_escape "$host")" "$(sot_json_escape "$role")" "$(sot_json_escape "${NAME:-}")"
}

# sot_ping_frame — one `ping` request line (topology plan §F step 2). No
# per-connection state needed (unlike `sot_hello_frame`, this carries no
# payload at all) -- id 2 is fixed and never correlated against a reply on
# this write-only-in-practice path: `comm-relay.sh bridge`'s read side
# (`filter_inbound`) already drops every frame whose op isn't
# `agent.message`, so the daemon's `{"ok":true}` ack is simply ignored,
# same as it ignores its own `hello` reply today.
sot_ping_frame() {
    printf '{"v":1,"id":2,"kind":"req","op":"ping","payload":{}}\n'
}

# sot_ping_interval_s — seconds between `ping` frames a long-lived bridge
# connection sends (topology plan §F step 2, D10) -- a third of the
# daemon's own 90s read deadline (`PING_READ_DEADLINE`, server.rs), so one
# or two missed ticks is noise and three in a row is what actually trips
# the daemon's reaper. `SOT_TEST_PING_INTERVAL_MS` overrides it for tests
# -- same env var name the frontend transport reads for its own ping
# timer, so one override drives both senders in a test. Unset in every
# real deployment.
sot_ping_interval_s() {
    local ms="${SOT_TEST_PING_INTERVAL_MS:-30000}"
    awk -v ms="$ms" 'BEGIN{printf "%.3f", ms/1000}'
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
# 10s. Uses ENDPOINT (unix:/path, tcp:host:port, or pipe:name — the last one
# a Windows-only named-pipe transport, see the pipe: arm below) from the
# caller's scope.
# _sot_oneshot_sender HELLO FRAME TIMEOUT_S PIDFILE — the write side of a
# one-shot request: hello, the frame, then `exec sleep` so the subshell's pid
# (written to PIDFILE first) is the sleep itself and one kill ends it. The
# hello is BUILT BY THE CALLER before the pipeline starts: building it here
# (hostname + four jq spawns, about a second on a Windows box) meant the
# reader had already started on an empty pipe, and PowerShell's
# Console.In.ReadLine never wakes for data that arrives after it began --
# every named-pipe one-shot timed out with no hello logged (2026-09-18).
_sot_oneshot_sender() {
    printf '%s\n' "$BASHPID" > "$4"
    printf '%s\n%s\n' "$1" "$2"
    exec sleep "$3"
}

sot_oneshot_request() {
    local frame="$1" op="$2"
    local timeout_s="${SOT_SEND_TIMEOUT:-${SEND_TIMEOUT:-10}}"
    local tmp ncpid line="" deadline hello
    hello="$(sot_hello_frame)"
    tmp="$(mktemp "${XDG_RUNTIME_DIR:-/tmp}/sot-oneshot-XXXXXX")" || return 1
    case "$ENDPOINT" in
        unix:*)
            command -v nc >/dev/null 2>&1 || {
                echo "ERROR: nc not found and endpoint is a unix socket (needs nc -U)" >&2
                rm -f "$tmp"; return 1; }
            # The sender holds the write side open with a sleep (a half-close
            # via `nc -q` made stub listeners hang up early). It is `exec`'d so
            # the recorded pid IS the sleep, killed the moment the reply
            # matches, and its stderr is detached: a sender that outlived the
            # reply used to hold the CALLER's stderr for the whole timeout, so
            # any pipe or harness reading the caller waited that long
            # (2026-09-17, two boxes).
            _sot_oneshot_sender "$hello" "$frame" "$timeout_s" "$tmp.snd" 2>/dev/null \
                | timeout "$timeout_s" nc -U "${ENDPOINT#unix:}" > "$tmp" 2>/dev/null &
            ncpid=$!
            ;;
        tcp:*)
            local hp="${ENDPOINT#tcp:}" host port
            host="${hp%:*}"; port="${hp##*:}"
            if command -v nc >/dev/null 2>&1; then
                _sot_oneshot_sender "$hello" "$frame" "$timeout_s" "$tmp.snd" 2>/dev/null \
                    | timeout "$timeout_s" nc "$host" "$port" > "$tmp" 2>/dev/null &
                ncpid=$!
            else
                # /dev/tcp fallback: the fd stays open for the whole window,
                # so the EOF race does not exist here — plain bounded read.
                (
                    exec 9<>"/dev/tcp/$host/$port" || exit 1
                    printf '%s\n%s\n' "$hello" "$frame" >&9
                    timeout "$timeout_s" cat <&9
                    exec 9<&- 9>&- 2>/dev/null || true
                ) > "$tmp" 2>/dev/null &
                ncpid=$!
            fi
            ;;
        pipe:*)
            # ADR 0042 amendment (2026-09-07): a Windows box's LOCAL daemon
            # only listens on a named pipe, which git-bash cannot open
            # itself — comm-pipe-request.ps1 is the transport, invoked
            # exactly the way nc is above (hello + frame piped to its
            # stdin, never argv). Accepts either the full \\.\pipe\<name>
            # form sot_daemon_endpoint prints or a bare pipe:<name> — both
            # reduce to the trailing NAME (NamedPipeClientStream never
            # takes the \\.\pipe\ prefix itself).
            local pipename="${ENDPOINT#pipe:}"
            pipename="${pipename##*\\}"
            command -v powershell.exe >/dev/null 2>&1 || {
                echo "ERROR: powershell.exe not found and endpoint is a named pipe (pipe: needs PowerShell)" >&2
                rm -f "$tmp"; return 1; }
            local ps1="${SCRIPT_DIR:-.}/comm-pipe-request.ps1"
            [ -f "$ps1" ] || {
                echo "ERROR: comm-pipe-request.ps1 not found next to the comm scripts (looked in ${SCRIPT_DIR:-.})" >&2
                rm -f "$tmp"; return 1; }
            _sot_oneshot_sender "$hello" "$frame" "$timeout_s" "$tmp.snd" 2>/dev/null \
                | timeout "$timeout_s" powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
                    -File "$ps1" -PipeName "$pipename" -Mode Oneshot -Op "$op" -TimeoutSec "$timeout_s" \
                    > "$tmp" 2>/dev/null &
            ncpid=$!
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
    [ -r "$tmp.snd" ] && kill "$(cat "$tmp.snd" 2>/dev/null)" 2>/dev/null
    rm -f "$tmp" "$tmp.snd"
    [ -n "$line" ] && printf '%s\n' "$line"
}
