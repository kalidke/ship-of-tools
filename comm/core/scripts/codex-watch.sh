#!/usr/bin/env bash
# codex-watch.sh <handle> <tmux-pane> — pane-injection wake for CODEX
# sessions (ADR 0031).
#
# Codex has no harness-Monitor primitive, so an idle codex session cannot be
# woken by an inbox write alone. This daemon POLLS the handle's inbox (~2s;
# NFS — inotify silently misses writes there, hence poll, same reason the CC
# Monitor polls) and TYPES each new directed frame into the codex pane via
# `tmux send-keys`, followed by Enter — codex processes it as a user turn.
#
# Filter mirrors comm-watch.sh: own echoes never inject; broadcasts (to:"")
# file silently for comm-poll on the next natural turn; directed frames and
# legacy no-`to` lines inject. Selftest frames DO inject (they prove this
# exact path).
#
# Lifecycle: started by ccx with the pane id; exits when the pane vanishes
# (checked each cycle) so it can't leak past its session.
set -uo pipefail
HANDLE="${1:?usage: codex-watch.sh <handle> <tmux-pane>}"
PANE="${2:?usage: codex-watch.sh <handle> <tmux-pane>}"
COMM_HOME="${SOT_COMM_HOME:-$HOME/.sot-comm}"
INBOX="$COMM_HOME/inbox/$HANDLE.jsonl"
POS_DIR="$COMM_HOME/state"; mkdir -p "$POS_DIR"
POS_FILE="$POS_DIR/codex-watch-$HANDLE.pos"

# _sot_secure_dir / sot_tmux_socket — inline copy of the secure-dir checks from
# comm-lib.sh. codex-watch also verifies candidate sockets against the target
# pane: Codex sessions may live on tmux's default server, while daemon-spawned
# sessions may live on the SOT private socket.
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
sot_tmux_socket() {
    local uid sock sotd_bin
    local -a candidates=()
    uid="$(id -u)"

    [ -n "${SOT_TMUX_SOCK:-}" ] && candidates+=("$SOT_TMUX_SOCK")
    [ -n "${TMUX:-}" ] && candidates+=("${TMUX%%,*}")

    sotd_bin="$(command -v sotd 2>/dev/null || true)"
    if [ -n "$sotd_bin" ]; then
        sock="$("$sotd_bin" tmux-socket-path 2>/dev/null || true)"
        [ -n "$sock" ] && candidates+=("$sock")
    fi

    if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ ! -L "$XDG_RUNTIME_DIR" ] && [ -d "$XDG_RUNTIME_DIR" ]; then
        local xowner xmode
        xowner="$(stat -c '%u' "$XDG_RUNTIME_DIR" 2>/dev/null || true)"
        xmode="$(stat -c '%a' "$XDG_RUNTIME_DIR" 2>/dev/null || true)"
        if [ -n "$xowner" ] && [ "$xowner" = "$uid" ] \
           && [ -n "$xmode" ] && [ $((0$xmode & 0077)) -eq 0 ]; then
            candidates+=("$XDG_RUNTIME_DIR/sot/tmux.sock")
        fi
    fi
    [ -d "/run/user/$uid" ] && candidates+=("/run/user/$uid/sot/tmux.sock")
    candidates+=("/tmp/sot-$uid/tmux.sock")
    candidates+=("/tmp/tmux-$uid/default")

    local seen=""
    for sock in "${candidates[@]}"; do
        [ -n "$sock" ] || continue
        case " $seen " in
            *" $sock "*) continue ;;
        esac
        seen="$seen $sock"
        _sot_secure_dir "$(dirname "$sock")" || continue
        [ -S "$sock" ] || continue
        if tmux -S "$sock" display-message -t "$PANE" -p '#{pane_id}' >/dev/null 2>&1; then
            printf '%s\n' "$sock"
            return 0
        fi
    done

    echo "sot_tmux_socket: pane $PANE was not found on candidate tmux sockets:$seen" >&2
    return 1
}
SOT_TMUX_SOCK="$(sot_tmux_socket)" \
    || { echo "ERROR: could not resolve a secure tmux socket for pane $PANE — see reason above" >&2; exit 1; }

# Start from the CURRENT end of the inbox — the backlog is comm-poll's job
# (ccx's first-turn brief runs it); injecting history would replay it.
pos=$(wc -l < "$INBOX" 2>/dev/null || echo 0)
echo "$pos" > "$POS_FILE"

while :; do
    sleep 2
    # Pane gone → session over → exit.
    tmux -S "$SOT_TMUX_SOCK" display-message -t "$PANE" -p '#{pane_id}' >/dev/null 2>&1 || exit 0
    [ -f "$INBOX" ] || continue
    total=$(wc -l < "$INBOX" 2>/dev/null || echo 0)
    if [ "$total" -lt "$pos" ]; then pos=0; fi   # inbox rotated/truncated
    [ "$total" -gt "$pos" ] || continue
    tail -n +"$((pos + 1))" "$INBOX" | while IFS= read -r line; do
        from=$(printf '%s' "$line" | jq -r '.from // ""' 2>/dev/null)
        to=$(printf '%s' "$line" | jq -r 'if has("to") then .to else "__legacy__" end' 2>/dev/null)
        text=$(printf '%s' "$line" | jq -r '.text // .message // .msg // ""' 2>/dev/null)
        [ "$from" = "$HANDLE" ] && continue      # own echo
        [ "$to" = "" ] && continue               # broadcast: file silently
        [ -n "$text" ] || continue
        # -l: literal keystrokes (no key-name interpretation); Enter sent
        # separately so codex submits the injected line as a turn.
        tmux -S "$SOT_TMUX_SOCK" send-keys -t "$PANE" -l "[relay] from $from: $text" 2>/dev/null
        tmux -S "$SOT_TMUX_SOCK" send-keys -t "$PANE" Enter 2>/dev/null
    done
    pos="$total"
    echo "$pos" > "$POS_FILE"
done
