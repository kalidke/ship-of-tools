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
