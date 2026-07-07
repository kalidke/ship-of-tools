#!/usr/bin/env bash
# comm-listen.sh — start a DURABLE relay listener so this machine receives
# cross-machine sot-comm relay messages into its local inbox.
#
# WHY: the relay is live-only (the daemon broadcasts to connected clients; no
# queue). A CLI session that isn't holding a connection misses broadcasts. This
# starts a reconnect-loop bridge (in a detached tmux session) that stays
# connected and files inbound messages into ~/.sot-comm/inbox/<name>.jsonl.
#
# IMPORTANT — this is only HALF of receiving. A Claude session does NOT act on a
# silent file write. After starting this, the agent must ALSO arm a Monitor on
# its inbox so new messages WAKE it (a harness action a script can't do). The
# Monitor command is comm-watch.sh (poll-based — NOT tail -F, which inotify makes
# unreliable over NFS):
#   comm-watch.sh <name>
# See the sot-session-start SKILL for the full arm-the-Monitor step.
#
# Usage: comm-listen.sh [--name NAME]   # start (default: your joined handle)
#        comm-listen.sh --status
#        comm-listen.sh --stop
#        comm-listen.sh --selftest      # prove the receive path end-to-end (no peer
#                                       # needed); auto-restart the listener once if broken.
#                                       # Exit: 0 OK, 1 daemon unreachable, 3 bridge still
#                                       # connecting (cold start, benign — re-run shortly)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home
# Private tmux socket (security review) — every tmux session this user's
# Ship of Tools tooling creates (daemon workspaces AND this script's own
# relay-bridge session) lives on one private, non-default server so another
# local account sharing this host can't attach to it. Resolved once, used
# on every `tmux` call below via `-S`.
SOT_TMUX_SOCK="$(sot_tmux_socket)" \
    || { echo "ERROR: could not resolve/secure the private tmux socket dir — see reason above" >&2; exit 1; }

MODE="start"; WANT_NAME=""
while [ $# -gt 0 ]; do
    case "$1" in
        --name)     WANT_NAME="$2"; shift 2 ;;
        --stop)     MODE="stop"; shift ;;
        --status)   MODE="status"; shift ;;
        --selftest) MODE="selftest"; shift ;;
        *)          shift ;;
    esac
done
[ -n "$WANT_NAME" ] && NAME="$WANT_NAME"
[ -z "$NAME" ] && { echo "ERROR: no handle — run comm-join.sh first or pass --name" >&2; exit 1; }

BIN="$COMM_HOME/bin"
SESSION="commbridge-$NAME"
bridge_running() { pgrep -f "comm-relay.sh bridge --name $NAME" >/dev/null 2>&1; }

case "$MODE" in
    status)
        if bridge_running; then echo "relay listener for @$NAME: RUNNING (pid $(pgrep -f "comm-relay.sh bridge --name $NAME" | paste -sd, -))"
        else echo "relay listener for @$NAME: not running"; fi
        ;;
    stop)
        tmux -S "$SOT_TMUX_SOCK" kill-session -t "$SESSION" 2>/dev/null || true
        pkill -f "comm-relay.sh bridge --name $NAME" 2>/dev/null || true
        echo "stopped relay listener for @$NAME"
        ;;
    start)
        if bridge_running; then
            echo "relay listener for @$NAME already running — good."
        else
            # Detached tmux session with a reconnect loop (the relay can drop;
            # this re-establishes it). Files inbound into inbox/<name>.jsonl.
            tmux -S "$SOT_TMUX_SOCK" new-session -d -s "$SESSION" \
                "while true; do '$BIN/comm-relay.sh' bridge --name '$NAME'; sleep 2; done" 2>/dev/null \
                || { echo "ERROR: could not start tmux session $SESSION" >&2; exit 1; }
            echo "started relay listener for @$NAME (tmux session '$SESSION')"
        fi
        echo "NEXT (required for the session to actually react): arm a persistent harness Monitor"
        echo "that POLLS your inbox so new messages wake you — POLL, not 'tail -F' (the inbox is on"
        echo "NFS, where inotify silently misses/delays writes). Monitor command:"
        echo "  comm-watch.sh $NAME"
        echo "(see /sot-session-start or /sot-be-session-start). Inbox it watches:"
        echo "  $INBOX_DIR/$NAME.jsonl"
        ;;
    selftest)
        # Prove the receive path daemon->bridge->inbox works, with no peer needed:
        # inject a synthetic relay frame addressed to ourselves (sentinel sender so
        # the bridge's self-echo filter doesn't drop it) and confirm it lands in the
        # inbox. If it doesn't, restart the listener once and retry. The bridge now
        # self-heals on connection drop (see comm-relay.sh nc_hold), so this should
        # only ever need the restart for a wedged/silent-hung connection.
        INBOX="$INBOX_DIR/$NAME.jsonl"
        # Handles joined before comm-join created inboxes (or with a hand-rolled
        # join) may lack the file; the probe's `wc -l <` then spews a redirect
        # error bash can't 2>/dev/null-suppress (it fires before the fd dup) —
        # cosmetic, but it derailed a real first-join diagnosis. Append-touch.
        : >> "$INBOX"
        EP="${SOT_RELAY_ENDPOINT:-}"
        if [ -z "$EP" ]; then
            a="$(pgrep -af 'sotd' 2>/dev/null | grep -v 'grep\|pgrep' | head -1 || true)"
            if [[ "$a" =~ --tcp[[:space:]]+([^[:space:]]+) ]]; then EP="tcp:${BASH_REMATCH[1]}"; fi
        fi
        case "$EP" in
            tcp:*) hp="${EP#tcp:}"; SH="${hp%:*}"; SP="${hp##*:}" ;;
            *) echo "selftest @$NAME: no TCP daemon endpoint found (got '${EP:-none}')" >&2; exit 1 ;;
        esac
        _inject() {
            # The connect MUST live in a subshell: `exec` with redirections
            # only EXITS a non-interactive shell on a failed redirect — the
            # `|| return 1` never runs, and the whole selftest dies silently
            # between the probe and the DOWN diagnostics (the unidentified
            # kill site from the 2026-06-11 fresh-join report). In a subshell
            # the death is contained and surfaces as a normal probe failure,
            # which routes into the restart + diagnostics path as designed.
            (
                exec 8<>"/dev/tcp/$SH/$SP" 2>/dev/null || exit 1
                # Auth the injector connection (ADR 0010 hardening): a token-valid
                # hello must precede the agent.send, else the gated daemon rejects it.
                _tok="${SOT_TOKEN:-$(cat "${XDG_CONFIG_HOME:-$HOME/.config}/sot/token" 2>/dev/null || true)}"
                printf '{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"sot-comm","last_seen_revision":0,"protocol":1,"app_version":"comm","token":"%s"}}\n' "$_tok" >&8
                printf '%s\n' "{\"v\":1,\"id\":1,\"kind\":\"req\",\"op\":\"agent.send\",\"payload\":{\"from\":\"__selftest__\",\"to\":\"$NAME\",\"text\":\"receive-path self-test\"}}" >&8
                timeout 3 cat <&8 >/dev/null 2>&1 || true
                exec 8<&- 8>&- 2>/dev/null || true
            ) 2>/dev/null || return 1
        }
        _estab() { ss -tn 2>/dev/null | awk -v p="127.0.0.1:$SP" '$1=="ESTAB" && $5==p{f=1} END{exit f?0:1}'; }
        # Is the daemon itself reachable? A bare TCP connect to the resolved
        # endpoint, independent of whether our bridge has come up. This is the
        # discriminator: a failing probe with a REACHABLE daemon is a cold-start
        # bridge still connecting (benign, retry shortly); a failing probe with
        # an UNREACHABLE daemon is a real outage. The connect lives in a subshell
        # for the same reason _inject does (a redirections-only `exec` that fails
        # EXITS a non-interactive shell rather than running the `||`).
        _daemon_reachable() {
            ( exec 3<>"/dev/tcp/$SH/$SP" 2>/dev/null || exit 1
              exec 3<&- 3>&- 2>/dev/null || true ) 2>/dev/null
        }
        # _probe injects once and re-injects mid-wait: on a cold start the bridge
        # may not be ESTAB when the first frame is sent, so that frame is lost to
        # the live-only relay. Re-injecting partway through means a frame is in
        # flight once the bridge connects, instead of waiting a whole second cycle.
        _probe() {
            local base cur i
            base="$(wc -l < "$INBOX" 2>/dev/null || echo 0)"
            _inject || return 1
            for i in $(seq 1 12); do
                cur="$(wc -l < "$INBOX" 2>/dev/null || echo 0)"
                if [ "$cur" -gt "$base" ]; then return 0; fi
                [ "$i" = 6 ] && { _inject || true; }   # re-inject once mid-wait
                sleep 1
            done
            return 1
        }
        # Distinct exit codes so a caller (and the skill) can tell apart:
        #   0  = receive path OK / recovered
        #   1  = daemon unreachable (real outage — "check the daemon")
        #   3  = daemon reachable but bridge still connecting (cold start, benign)
        if ! bridge_running; then
            tmux -S "$SOT_TMUX_SOCK" new-session -d -s "$SESSION" \
                "while true; do '$BIN/comm-relay.sh' bridge --name '$NAME'; sleep 2; done" 2>/dev/null || true
        fi
        # Cold-start bridges often need well over 8s to reach ESTAB; the old short
        # wait made the selftest declare DOWN while the daemon was perfectly fine
        # (false alarm that cost 3-5 tool calls in two fresh-boot reports). Wait
        # longer before the first probe.
        for i in $(seq 1 20); do if _estab; then break; fi; sleep 1; done
        if _probe; then echo "selftest @$NAME: receive path OK"; exit 0; fi
        echo "selftest @$NAME: receive path not yet proven -- restarting listener..." >&2
        tmux -S "$SOT_TMUX_SOCK" kill-session -t "$SESSION" 2>/dev/null || true
        pkill -f "comm-relay.sh bridge --name $NAME" 2>/dev/null || true
        sleep 1
        tmux -S "$SOT_TMUX_SOCK" new-session -d -s "$SESSION" \
            "while true; do '$BIN/comm-relay.sh' bridge --name '$NAME'; sleep 2; done" 2>/dev/null || true
        for i in $(seq 1 20); do if _estab; then break; fi; sleep 1; done
        if _probe; then echo "selftest @$NAME: RECOVERED after restart"; exit 0; fi
        # Still no delivery. Discriminate daemon-down from bridge-still-connecting
        # BEFORE telling anyone to "check the daemon" — the cold-start case is
        # benign and self-resolves; only an unreachable daemon is actionable.
        if _daemon_reachable; then
            echo "selftest @$NAME: bridge still connecting (cold start, give it 2-5s) -- re-run comm-listen.sh --selftest shortly" >&2
            exit 3
        fi
        echo "selftest @$NAME: daemon unreachable at $EP -- check the daemon" >&2
        exit 1
        ;;
esac
