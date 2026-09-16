#!/usr/bin/env bash
# comm-listen.sh — start a DURABLE relay listener so this machine receives
# cross-machine sot-comm relay messages into its local inbox.
#
# WHY: the relay is live-only (the daemon broadcasts to connected clients; no
# queue). A CLI session that isn't holding a connection misses broadcasts. This
# starts a reconnect-loop bridge (a background child of this session, pid
# recorded under the comm state dir) that stays connected and files inbound
# messages into ~/.sot-comm/inbox/<name>.jsonl.
#
# WINDOWS: this is a no-op there (prints the receive path and exits 0) — the
# frontend already files every inbound frame into its own fe-inbox.jsonl, and
# a bridge process has no role and is actively harmful (see the in-body
# comment next to the Windows check for why). --status/--selftest report the
# same; there is nothing to --stop.
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

# Mode/name parsed FIRST so the Windows short-circuit right below can act
# on it without a resolved handle.
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
# Resolved here — before the Windows short-circuit below, which (for
# --selftest) needs NAME too, not just the Linux path further down.
[ -n "$WANT_NAME" ] && NAME="$WANT_NAME"

# On a Windows host the frontend already files every inbound relay frame
# into its own fe-inbox.jsonl (gpu.rs::append_agent_message) and the
# session's Monitor tails that — see /sot-session-start. A durable
# reconnect-loop bridge has no receive role there, and starting one is
# actively harmful: its `while true; do comm-relay.sh bridge …; done` loop
# never exits, so bash keeps this script's own file open for the life of
# the daemon-spawned session, which on Windows blocks `update_comm`'s
# remove-then-copy replace of it (delete-pending) — the box goes send-deaf
# to every subsequent converge. So: no bridge, ever, here.
#
# Shared _sot_is_windows (comm-lib.sh) — this script already sources it, so
# it uses the ONE canonical platform test rather than its own copy.
#
if _sot_is_windows; then
    # Mirrors comm-session-skill.sh's fe_inbox construction exactly
    # (gpu.rs::sot_state_dir): %LOCALAPPDATA%\sot on Windows. Built from
    # that one resolver, never a bare "/"-rooted guess.
    fe_inbox="${LOCALAPPDATA:-${XDG_STATE_HOME:-$HOME/.local/state}}/sot/fe-inbox.jsonl"
    echo "comm-listen: this host receives through the FE inbox ($fe_inbox); no relay bridge is started"
    if [ "$MODE" = "selftest" ]; then
        # There is no bridge to restart here — the receive path is
        # daemon -> broadcast -> native FE -> fe_inbox append, so the proof is
        # a real injected frame surviving that exact path, not a bridge
        # liveness check. Inject a synthetic `agent.message` addressed to
        # ourselves over $SOT_RELAY_ENDPOINT (sot_relay_endpoint defaults
        # this to the local tunnel on Windows — comm-lib.sh, finding 6) and
        # poll the inbox for it. Same exit-code contract as the Linux branch
        # below: 0 receive path OK, 1 daemon unreachable/rejected, 3 daemon
        # up but no inbox line yet (benign — a cold FE relaunch, or the
        # tunnel just came up).
        [ -z "$NAME" ] && { echo "ERROR: no handle — run comm-join.sh first or pass --name" >&2; exit 1; }
        mkdir -p "$(dirname "$fe_inbox")" 2>/dev/null || true
        : >> "$fe_inbox"
        EP="$(sot_relay_endpoint "${SOT_RELAY_ENDPOINT:-${SOT_SPAWN_ENDPOINT:-}}")" \
            || { echo "selftest @$NAME: no daemon endpoint found (set SOT_RELAY_ENDPOINT=tcp:HOST:PORT — the local tunnel to the remote socket)" >&2; exit 1; }
        case "$EP" in
            tcp:*) hp="${EP#tcp:}"; SH="${hp%:*}"; SP="${hp##*:}" ;;
            *) echo "selftest @$NAME: endpoint '$EP' is not tcp — a Windows FE always relays over its local tunnel" >&2; exit 1 ;;
        esac

        # _win_probe_once NONCE -- ONE connect+hello+send+read attempt.
        # Prints exactly one of: ok | rejected | silent | unreachable.
        # "rejected" means the daemon EXPLICITLY answered hello or
        # agent.send with ok!=true (bad token, protocol mismatch) — retrying
        # will NOT fix that, unlike "silent" (no reply within the window,
        # e.g. a cold bridge) — Codex review finding 14: the old version
        # discarded the reply entirely (`cat >/dev/null`), so a real
        # rejection looked identical to a benign cold-start retry.
        _win_probe_once() {
            exec 8<>"/dev/tcp/$SH/$SP" 2>/dev/null || { echo unreachable; return 0; }
            sot_hello_frame >&8
            printf '%s\n' "{\"v\":1,\"id\":1,\"kind\":\"req\",\"op\":\"agent.send\",\"payload\":{\"from\":\"__selftest__\",\"to\":\"$NAME\",\"text\":\"$1\"}}" >&8
            local out; out="$(cat <&8 2>/dev/null)"
            exec 8<&- 8>&- 2>/dev/null || true
            [ -n "$out" ] || { echo silent; return 0; }
            local hello_line send_line
            hello_line="$(printf '%s' "$out" | grep -m1 '"op":"hello"')"
            send_line="$(printf '%s' "$out" | grep -m1 '"op":"agent.send"')"
            if [ -n "$hello_line" ] && ! printf '%s' "$hello_line" | jq -e '.payload.ok == true' >/dev/null 2>&1; then
                echo rejected; return 0
            fi
            if [ -n "$send_line" ]; then
                printf '%s' "$send_line" | jq -e '.payload.ok == true' >/dev/null 2>&1 && echo ok || echo rejected
                return 0
            fi
            echo silent
        }
        # The WHOLE attempt (connect, write, read) is time-bounded, not just
        # the read half (finding 14: an unbounded /dev/tcp connect to a
        # black-holed address used to hang this indefinitely). `export -f`
        # hands the function to a fresh `bash -c` under `timeout` — the
        # connect is a shell builtin (/dev/tcp), so an external `timeout`
        # can only bound it by wrapping a whole bash process, not the
        # builtin directly.
        # sot_hello_frame's own dependencies (S13, Codex finding S13) must
        # travel with it: sot_host (the declared-host resolver) and
        # sot_json_escape (S19's JSON-safe interpolation) — a child bash
        # missing either produces an empty-host hello (reproduced).
        export -f _win_probe_once sot_hello_frame sot_host sot_json_escape
        export SH SP NAME SOT_TOKEN XDG_CONFIG_HOME SOT_SELF_HOST HOST SOT_WORKSPACE
        _win_probe() {
            local r; r="$(timeout 5 bash -c '_win_probe_once "$1"' _ "$1" 2>/dev/null)"
            printf '%s' "${r:-unreachable}"
        }

        NONCE="selftest-$$-$RANDOM"
        result="$(_win_probe "$NONCE")"
        if [ "$result" = rejected ]; then
            echo "selftest @$NAME: daemon rejected the connection (bad token or protocol mismatch) -- check \$SOT_TOKEN / the config token file, not the tunnel" >&2
            exit 1
        fi
        for i in $(seq 1 8); do
            grep -q "$NONCE" "$fe_inbox" 2>/dev/null && { echo "selftest @$NAME: receive path OK"; exit 0; }
            if [ "$i" = 4 ]; then
                result="$(_win_probe "$NONCE")"
                [ "$result" = rejected ] && { echo "selftest @$NAME: daemon rejected the connection (bad token or protocol mismatch)" >&2; exit 1; }
            fi
            sleep 1
        done
        if [ "$result" = ok ] || [ "$(_win_probe "selftest-reach-$$-$RANDOM")" != unreachable ]; then
            echo "selftest @$NAME: daemon reachable but no inbox line yet (FE relaunch, or the tunnel just came up) -- re-run comm-listen.sh --selftest shortly" >&2
            exit 3
        fi
        echo "selftest @$NAME: daemon unreachable at $EP -- check the local tunnel / launcher" >&2
        exit 1
    fi
    exit 0
fi

[ -z "$NAME" ] && { echo "ERROR: no handle — run comm-join.sh first or pass --name" >&2; exit 1; }

RELAY="$COMM_HOME/bin/comm-relay.sh"
# The bridge's liveness, start and stop are comm-lib.sh's sot_bridge_*
# helpers (also used by comm-join.sh's stranding guard): a pidfile under
# the comm state dir names the loop, checked by exact argv. A loop from a
# previous release (tmux-wrapped) or a hand-started bridge is a stray:
# found by process pattern and killed before a fresh loop starts, so no
# two bridges ever file the same frame twice.
bridge_running() { sot_bridge_running_for "$NAME"; }

case "$MODE" in
    status)
        if bridge_running; then echo "relay listener for @$NAME: RUNNING (pid $(sot_bridge_pid_for "$NAME"))"
        else echo "relay listener for @$NAME: not running"; fi
        ;;
    stop)
        sot_bridge_stop "$NAME"
        echo "stopped relay listener for @$NAME"
        ;;
    start)
        if bridge_running; then
            echo "relay listener for @$NAME already running — good."
        else
            # A reconnect loop (the relay can drop; this re-establishes it),
            # a background child of this session. Files inbound into
            # inbox/<name>.jsonl.
            sot_bridge_start "$NAME" "$RELAY"
            echo "started relay listener for @$NAME (pid $(sot_bridge_pid_for "$NAME" || echo '?'))"
        fi
        echo "NEXT (required for the session to actually react): arm a persistent harness Monitor"
        echo "that POLLS your inbox so new messages wake you — POLL, not 'tail -F' (the inbox is on"
        echo "NFS, where inotify silently misses/delays writes). Monitor command:"
        echo "  comm-watch.sh $NAME"
        echo "(see /sot-session-start). Inbox it watches:"
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
        EP="$(sot_relay_endpoint "${SOT_RELAY_ENDPOINT:-${SOT_SPAWN_ENDPOINT:-}}")" \
            || { echo "selftest @$NAME: no daemon endpoint found (set SOT_RELAY_ENDPOINT=unix:/path or tcp:HOST:PORT)" >&2; exit 1; }
        SH=""; SP=""; SU=""
        case "$EP" in
            tcp:*)  hp="${EP#tcp:}"; SH="${hp%:*}"; SP="${hp##*:}" ;;
            unix:*) SU="${EP#unix:}" ;;
            *) echo "selftest @$NAME: bad daemon endpoint '$EP'" >&2; exit 1 ;;
        esac
        _selftest_frames() {
            sot_hello_frame
            printf '%s\n' "{\"v\":1,\"id\":1,\"kind\":\"req\",\"op\":\"agent.send\",\"payload\":{\"from\":\"__selftest__\",\"to\":\"$NAME\",\"text\":\"receive-path self-test\"}}"
        }
        # A one-shot direct connection to the daemon (bypassing NAME's own
        # bridge entirely) that sends the self-test frame and prints its
        # `agent.send` ack line on stdout — nothing on failure. The connect
        # MUST live in a subshell: `exec` with redirections only EXITS a
        # non-interactive shell on a failed redirect — a bare `|| return 1`
        # after it never runs, and the whole selftest used to die silently
        # between the probe and the DOWN diagnostics (the unidentified kill
        # site from the 2026-06-11 fresh-join report). In a subshell the
        # death is contained and surfaces as ordinary empty output instead.
        _inject() {
            if [ -n "$SH" ]; then
                (
                    exec 8<>"/dev/tcp/$SH/$SP" 2>/dev/null || exit 1
                    _selftest_frames >&8
                    timeout 3 cat <&8 2>/dev/null
                    exec 8<&- 8>&- 2>/dev/null || true
                ) 2>/dev/null | grep -m1 '"op":"agent.send"'
                return
            fi
            command -v nc >/dev/null 2>&1 || return 1
            _selftest_frames | timeout 3 nc -U "$SU" 2>/dev/null | grep -m1 '"op":"agent.send"'
        }
        # Is `$NAME` in the ack's `receivers` (honest-send fix) — i.e. is
        # the bridge actually attached and subscribed right now? Reads the
        # response `_inject` printed, passed as $1; empty input reads "no".
        _attached() {
            [ -n "$1" ] || return 1
            printf '%s' "$1" | jq -e --arg n "$NAME" '(.payload.receivers // []) | any(. == $n)' >/dev/null 2>&1
        }
        # One inject + classify, with an inbox-growth wait once attached —
        # the ack alone proves the daemon saw the frame and knows who's
        # subscribed, but it can't see the WRITE half (daemon push -> bridge
        # -> file); only watching the inbox grow proves that. Re-injects
        # once mid-wait in case the first frame raced a bridge that hadn't
        # finished subscribing yet. Return codes:
        #   0 = OK: bridge attached, inbox line landed
        #   1 = daemon unreachable (no ack at all -- real outage)
        #   2 = not proven: bridge not attached yet, or attached but the
        #       inbox never grew -- both benign/retry-shortly outcomes
        _probe() {
            local base cur i resp
            base="$(wc -l < "$INBOX" 2>/dev/null || echo 0)"
            resp="$(_inject || true)"
            [ -n "$resp" ] || return 1
            _attached "$resp" || return 2
            for i in $(seq 1 12); do
                cur="$(wc -l < "$INBOX" 2>/dev/null || echo 0)"
                if [ "$cur" -gt "$base" ]; then return 0; fi
                [ "$i" = 6 ] && resp="$(_inject || true)"   # re-inject once mid-wait
                sleep 1
            done
            return 2
        }
        # Distinct exit codes so a caller (and the skill) can tell apart:
        #   0  = receive path OK / recovered
        #   1  = daemon unreachable (real outage — "check the daemon")
        #   3  = daemon reachable but bridge still connecting (cold start, benign)
        bridge_running || sot_bridge_start "$NAME" "$RELAY"
        _probe; rc=$?
        if [ "$rc" -eq 0 ]; then echo "selftest @$NAME: receive path OK"; exit 0; fi
        if [ "$rc" -eq 1 ]; then
            echo "selftest @$NAME: daemon unreachable at $EP -- check the daemon" >&2
            exit 1
        fi
        echo "selftest @$NAME: receive path not yet proven -- restarting listener..." >&2
        sot_bridge_stop "$NAME"
        sleep 1
        sot_bridge_start "$NAME" "$RELAY"
        sleep 2
        _probe; rc=$?
        if [ "$rc" -eq 0 ]; then echo "selftest @$NAME: RECOVERED after restart"; exit 0; fi
        if [ "$rc" -eq 1 ]; then
            echo "selftest @$NAME: daemon unreachable at $EP -- check the daemon" >&2
            exit 1
        fi
        echo "selftest @$NAME: bridge still connecting (cold start, give it 2-5s) -- re-run comm-listen.sh --selftest shortly" >&2
        exit 3
        ;;
esac
