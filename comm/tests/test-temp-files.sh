#!/usr/bin/env bash
# test-temp-files.sh — no messaging script or library call leaves a file in its temporary folders ($TMPDIR and
# $XDG_RUNTIME_DIR), on success, on failure, or killed. A script that made a temporary file for a value and removed
# it by a later `rm` left it on every path that skipped the `rm`: a jq failure under `set -e`, a SIGKILL, a signal
# that ended a call waiting on the daemon. Each row drives one script through those paths (lib-temp-files.sh) and
# fails naming what was left. The agents scripts' rows are agents/tests/test-temp-files.sh.
#
# HERMETIC: a temp $SOT_COMM_HOME, a stand-in daemon on a socket of the suite's own, the scripts from the staged bin.
#
# Usage: comm/tests/test-temp-files.sh
# Exit: the count of failed rows.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/lib-wait.sh" || exit 2
. "$(dirname "${BASH_SOURCE[0]}")/lib-temp-files.sh" || exit 2

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-temp-files-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"; guard_refuse_live_home "$WORK/home2"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
tf_setup "$WORK" || exit 2
trap 'tf_daemon_stop; rm -rf "${WORK:?}"' EXIT
tf_daemon_start || exit 2
cd "$WORK" || exit 2

# Two comm homes. In `home` this box takes the inbox lock itself (the record a daemon writes at startup, under a
# findmnt that reports a local filesystem), so a send appends locally; `home2` has no record, so a send files
# through the daemon (`comm.file`).
mkdir -p "$WORK/findmnt-bin" "$SOT_COMM_HOME/inbox" "$WORK/home2/inbox"
printf '#!/bin/sh\necho "ext4 rw,relatime /dev/fake"\n' > "$WORK/findmnt-bin/findmnt"
chmod +x "$WORK/findmnt-bin/findmnt"
export PATH="$WORK/findmnt-bin:$PATH"
bash -c 'source "$1"; sot_inbox_lock_identity "$INBOX_DIR"' _ "$SCRIPTS_DIR/comm-lib.sh" > "$SOT_COMM_HOME/inbox-lock-manager"
for home in "$SOT_COMM_HOME" "$WORK/home2"; do
    # The peer lives on another host, so a send never types into its row.
    env SOT_COMM_HOME="$home" SOT_COMM_SELF_FILE="$home/self-peer" SOT_COMM_TEST_HOST=otherbox \
            "$SCRIPTS_DIR/comm-join.sh" --name t-peer > /dev/null 2>&1 \
        && env SOT_COMM_HOME="$home" SOT_COMM_SELF_FILE="$home/self-sender" SOT_COMM_TEST_HOST=testhost \
            "$SCRIPTS_DIR/comm-join.sh" --name t-sender > /dev/null 2>&1 \
        || { echo "FATAL: could not join the two rows in $home" >&2; exit 2; }
done
SENDER=(env SOT_COMM_SELF_FILE="$SOT_COMM_HOME/self-sender" SOT_COMM_TEST_HOST=testhost)
SENDER2=(env SOT_COMM_HOME="$WORK/home2" SOT_COMM_SELF_FILE="$WORK/home2/self-sender" SOT_COMM_TEST_HOST=testhost)

echo "test-temp-files.sh (messaging): $(bash --version | head -n 1)"

# --- a value that carries the mark: ok, fail, kill -----------------------------------------------------------------
c1_check() {
    jq -e --arg m "$TF_SENT" 'select(.msg == $m)' "$SOT_COMM_HOME/inbox/t-peer.jsonl" > /dev/null \
        || { echo "    the inbox holds no line whose msg is the value"; return 1; }
}
for p in ok fail kill; do TF_CHECK=c1_check tf_row "C1 comm-send (local append)" "$p" "${SENDER[@]}" "$SCRIPTS_DIR/comm-send.sh" @t-peer "$TF_SENT"; done

tf_table '{"comm.file":{"ok":true}}'
for p in ok fail kill; do tf_row "C2 comm-relay send (the filing)" "$p" "${SENDER[@]}" "$SCRIPTS_DIR/comm-relay.sh" send @t-stranger "$TF_SENT"; done

c3_check() {
    jq -e --arg r "$C3_ROOT" '.agents["t-join-ok"].root == $r' "$SOT_COMM_HOME/registry.json" > /dev/null \
        || { echo "    the registry row's root is not the project root"; return 1; }
}
mkdir -p "$WORK/$TF_MARK/proj"
C3_ROOT="$(cd -P "$WORK/$TF_MARK/proj" && pwd)"
for p in ok fail kill; do
    TF_CHECK=c3_check tf_row "C3 comm-join (a root that carries the mark)" "$p" sh -c 'cd "$1" && shift && exec "$@"' _ \
        "$C3_ROOT" env SOT_COMM_SELF_FILE="$WORK/self-join-$p" SOT_COMM_TEST_HOST=testhost "$SCRIPTS_DIR/comm-join.sh" \
        --name "t-join-$p"
done

c4_check() {
    jq -e --arg q "$TF_SENT" '.agents["t-sender"].question == $q' "$SOT_COMM_HOME/registry.json" > /dev/null \
        || { echo "    the row's question is not the value, byte for byte"; return 1; }
}
for p in ok fail kill; do
    cp "$SOT_COMM_HOME/registry.json" "$WORK/registry.before"
    TF_CHECK=c4_check tf_row "C4 comm-status (the summary)" "$p" "${SENDER[@]}" "$SCRIPTS_DIR/comm-status.sh" blocked "$TF_SENT"
    if [ "$p" = fail ] && ! cmp -s "$WORK/registry.before" "$SOT_COMM_HOME/registry.json"; then
        echo "FAIL: C4 comm-status (the summary) fail changed the registry"; TF_FAILED=$((TF_FAILED + 1))
    fi
done

# The Stop hook's two findings blocks: the auditor's words are free text. The hook runs from a folder of its own
# whose comm-turn-auditor.sh stand-in prints a finding that carries the mark; each path has its own joined row, which
# a closing marker stamps (C5a) or which is idle with no marker (C5b), and a one-turn transcript.
mkdir -p "$WORK/hooks"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
ln -s "$SCRIPTS_DIR/comm-status-idle.sh" "$WORK/hooks/comm-status-idle.sh"
printf 'badge %s' "$TF_SENT" > "$WORK/findings"
printf '#!/bin/sh\ncat "%s"\n' "$WORK/findings" > "$WORK/hooks/comm-turn-auditor.sh"
chmod +x "$WORK/hooks/comm-turn-auditor.sh"
for h in a b; do
    env SOT_COMM_SELF_FILE="$WORK/self-hook-$h" SOT_COMM_TEST_HOST=testhost "$SCRIPTS_DIR/comm-join.sh" --name "t-hook-$h" \
        > /dev/null 2>&1 || { echo "FATAL: could not join t-hook-$h" >&2; exit 2; }
done
c5_check() {
    jq -e --arg m "$TF_SENT" '.decision == "block" and (.reason | contains($m))' "$TF_DIR/out" > /dev/null \
        || { echo "    the hook printed no block that carries the finding: $(cat "$TF_DIR/out")"; return 1; }
}
for h in a b; do
    case "$h" in
        a) what="a closing marker's artifact audit" text=$'SITREP: wrote the brief\n\nDone.' ;;
        *) what="the turn audit" text="all set" ;;
    esac
    for p in ok fail kill; do
        # A transcript per run: the hook keys its feedback record on the transcript's name, and a block it records
        # carries the finding, which a later run would read before it reaches the audit.
        { jq -nc '{type:"user",message:{content:"go"}}'
          jq -nc --arg t "$text" '{type:"assistant",message:{content:[{type:"text",text:$t}]}}'; } > "$WORK/turn-$h-$p.jsonl"
        jq -nc --arg p "$WORK/turn-$h-$p.jsonl" '{transcript_path:$p, stop_hook_active:false}' > "$WORK/stop-$h-$p.json"
        TF_CHECK=c5_check tf_row "C5$h comm-status-idle ($what)" "$p" sh -c 'f="$1"; shift; exec "$@" < "$f"' _ \
            "$WORK/stop-$h-$p.json" env SOT_COMM_SELF_FILE="$WORK/self-hook-$h" SOT_COMM_TEST_HOST=testhost \
            bash "$WORK/hooks/comm-status-idle.sh"
    done
done

# --- a call that waits on the daemon, ended by a signal --------------------------------------------------------------
tf_table '{"comm.file":"silent"}'
for s in TERM HUP INT; do tf_signal "C6 comm-send (comm.file through the daemon)" "$s" env SOT_SOCKET="$TF_SOCK" "${SENDER2[@]}" "$SCRIPTS_DIR/comm-send.sh" @t-peer x; done

tf_table '{"comm.file":{"error":"not here","code":"not_here"},"agent.send":"silent"}'
for s in TERM HUP INT; do tf_signal "C7 comm-relay send (the agent.send leg)" "$s" "${SENDER[@]}" "$SCRIPTS_DIR/comm-relay.sh" send @t-stranger x; done

# The heartbeat's context capture: two files in the comm home's state/, held while comm-context.sh runs under the
# bound. The hook runs from a folder of its own whose comm-context.sh stand-in touches the silent mark and waits; each
# run has its own session key, so the hook's 10 s throttle never skips one. Ended through its process group, and by
# TERM to the hook alone, as a harness ends a hook (the bound, 2 s there, then ends the stand-in).
mkdir -p "$WORK/hb"
ln -s "$SCRIPTS_DIR/comm-status-heartbeat.sh" "$WORK/hb/comm-status-heartbeat.sh"
printf '#!/bin/sh\n: > "%s"\nexec "%s" 600\n' "$TF_DIR/silent" "$(command -v sleep)" > "$WORK/hb/comm-context.sh"
chmod +x "$WORK/hb/comm-context.sh"
printf '{"tool_name":"Bash"}' > "$WORK/hb/envelope.json"
c8_check() {
    local f n=0
    for f in "$SOT_COMM_HOME"/state/.hb-ctx-*; do
        [ -e "$f" ] || continue
        echo "    left: ${f##*/} in the comm home's state"; rm -rf -- "${f:?}"; n=$((n + 1))
    done
    [ "$n" -eq 0 ]
}
for s in TERM HUP INT; do
    TF_CHECK=c8_check tf_signal "C8 heartbeat context (the hook's group)" "$s" sh -c 'f="$1"; shift; exec "$@" < "$f"' _ \
        "$WORK/hb/envelope.json" env CLAUDE_CODE_SESSION_ID="tf-group-$s" SOT_HB_CTX_TIMEOUT_TICKS=1200 \
        bash "$WORK/hb/comm-status-heartbeat.sh"
done
TF_CHECK=c8_check TF_SIGNAL_TARGET=leader tf_signal "C8 heartbeat context (the hook alone)" TERM sh -c 'f="$1"; shift; exec "$@" < "$f"' _ \
    "$WORK/hb/envelope.json" env CLAUDE_CODE_SESSION_ID=tf-leader SOT_HB_CTX_TIMEOUT_TICKS=40 \
    bash "$WORK/hb/comm-status-heartbeat.sh"

tf_report
