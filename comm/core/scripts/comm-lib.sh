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
sot_registry_entry_status() {
    jq -r --arg n "$1" \
        'if (.agents | has($n)) then "present\t" + (.agents[$n].root // "") else "absent\t" end' \
        "$REGISTRY" 2>/dev/null
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
#             erase a LIVE agent's tmux/pane/status fields when spawning a
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
