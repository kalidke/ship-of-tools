#!/usr/bin/env bash
# test-temp-files.sh — no agents script leaves a file in its temporary folders ($TMPDIR and $XDG_RUNTIME_DIR), on
# success, on failure, or killed, and a signal ends `sot-gh-auth.sh poll`. The method is comm/tests/lib-temp-files.sh;
# the messaging scripts' rows are comm/tests/test-temp-files.sh.
#
# HERMETIC: a temp $SOT_COMM_HOME, a stand-in daemon on a socket of the suite's own, stand-in curl, gh and sleep for
# the sign-in, the scripts from the staged bin.
#
# Usage: agents/tests/test-temp-files.sh
# Exit: the count of failed rows.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-wait.sh" || exit 2
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-temp-files.sh" || exit 2

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-temp-files-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
guard_fresh_home "$WORK"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
tf_setup "$WORK" || exit 2
# The comm home's path carries the mark, so comm-probe.sh's row root (under it) does.
export SOT_COMM_HOME="$WORK/home-$TF_MARK" SOT_COMM_TEST_HOST=testhost
guard_refuse_live_home "$SOT_COMM_HOME"
trap 'tf_daemon_stop; rm -rf "${WORK:?}"' EXIT
tf_daemon_start || exit 2
cd "$WORK" || exit 2
FE=("$SCRIPTS_DIR/sot-fe")
EP=(--endpoint "unix:$TF_SOCK")

echo "test-temp-files.sh (agents): $(bash --version | head -n 1)"

# --- sot-fe: each free-text value, ok, fail, kill --------------------------------------------------------------------
tf_table '{"pty.input":{"ok":true,"bytes":1,"runtime":"capsule","enter":"sent"},
           "fe.command.send":{"ok":true,"delivered_to":1,"resolved_target":"fe@testhost"},
           "repl.execute":{"outcome":"ok","run_id":1,"elapsed_ms":1},
           "repl.run_file":{"project_dir":"/p","project_source":"test","accepted":true}}'
for p in ok fail kill; do
    tf_row "A1 sot-fe type (the text)" "$p" "${FE[@]}" type ws-1 "$TF_SENT" --origin o "${EP[@]}"
    tf_row "A2 sot-fe notify" "$p" "${FE[@]}" notify "$TF_SENT" "${EP[@]}"
    tf_row "A3 sot-fe docs (the path)" "$p" "${FE[@]}" docs ws-1 "$TF_SENT" "${EP[@]}"
    tf_row "A4 sot-fe preview (the caption)" "$p" "${FE[@]}" preview ws-1 rel/a.png --caption "$TF_SENT" "${EP[@]}"
    tf_row "A5 sot-fe repl run (the path)" "$p" "${FE[@]}" repl run ws-1 "$TF_SENT" "${EP[@]}"
    tf_row "A6 sot-fe repl run --fresh (the path)" "$p" "${FE[@]}" repl run ws-1 "$TF_SENT" --fresh "${EP[@]}"
    tf_row "A7 sot-fe repl eval (the code)" "$p" "${FE[@]}" repl eval ws-1 --code "$TF_SENT" "${EP[@]}"
    tf_row "A8 sot-fe repl eval (the origin)" "$p" "${FE[@]}" repl eval ws-1 --code x --origin "$TF_SENT" "${EP[@]}"
    tf_row "A9 sot-fe type (the origin)" "$p" "${FE[@]}" type ws-1 x --origin "$TF_SENT" "${EP[@]}"
done

# --- the row CLIs ------------------------------------------------------------------------------------------------------
# comm-spawn.sh: a root that carries the mark (the provisional row), then a label that does (the create request).
tf_table '{"version.query":{"daemon":{"host":"testhost"}},"workspace.list":{"workspaces":[]}}'
mkdir -p "$WORK/$TF_MARK/repo" "$WORK/repo2"
for p in fail kill; do
    tf_row "A10a comm-spawn (a root that carries the mark)" "$p" "$SCRIPTS_DIR/comm-spawn.sh" "$WORK/$TF_MARK/repo" \
        --agent none "${EP[@]}"
    tf_row "A10b comm-spawn (a label that carries the mark)" "$p" "$SCRIPTS_DIR/comm-spawn.sh" "$WORK/repo2" \
        --agent none --label "repo2-$TF_MARK" "${EP[@]}"
done

# comm-probe.sh up: the row root, then the typed line (sot_pty_input).
tf_table '{"workspace.list":{"workspaces":[]},"workspace.create":{"workspace_id":"ws-9"},
           "pty.input":{"ok":true,"bytes":1,"runtime":"capsule","enter":"sent"}}'
for p in ok fail kill; do tf_row "A11 comm-probe up (the row root)" "$p" env SOT_SOCKET="$TF_SOCK" "$SCRIPTS_DIR/comm-probe.sh" up; done

# comm-bootstrap.sh: the nudge it types (sot_pty_input) carries the suggested name.
env SOT_COMM_SELF_FILE="$WORK/self-boot" "$SCRIPTS_DIR/comm-join.sh" --name t-boot > /dev/null 2>&1 \
    || { echo "FATAL: could not join t-boot" >&2; exit 2; }
tf_table '{"workspace.list":{"workspaces":[{"workspace_id":"ws-1","slug":"t1","label":"t1","agent":"claude"}]},
           "pty.input":{"ok":true,"bytes":1,"runtime":"capsule","enter":"sent"}}'
for p in ok fail kill; do
    tf_row "A12 comm-bootstrap (the nudge)" "$p" env SOT_COMM_SELF_FILE="$WORK/self-boot" SOT_SPAWN_ENDPOINT="unix:$TF_SOCK" \
        "$SCRIPTS_DIR/comm-bootstrap.sh" t1 "tf$TF_MARK"
done

# --- a request waiting on the daemon, ended by a signal ----------------------------------------------------------------
tf_table '{"pty.screen":"silent"}'
for s in TERM HUP INT; do tf_signal "A13 sot-fe screen (a request the daemon never answers)" "$s" "${FE[@]}" screen ws-1 "${EP[@]}"; done

# --- sot-gh-auth.sh poll -----------------------------------------------------------------------------------------------
# Stand-ins: gh is never signed in; sleep returns at once; curl answers the device-code request with the row's
# interval and expiry, and on a token call records how it got the device code, then blocks (TF_GH=block: the daemon's
# silent mark is its signal) or fails as a network error does (TF_GH=down).
GH="$WORK/gh-stub"; mkdir -p "$GH"
TF_SLEEP="$(command -v sleep)" && export TF_SLEEP || exit 2
printf '#!/bin/sh\ncase "$*" in *"auth status"*) exit 1 ;; esac\nexit 0\n' > "$GH/gh"
printf '#!/bin/sh\nexit 0\n' > "$GH/sleep"
cat > "$GH/curl" <<'CURL'
#!/usr/bin/env bash
url="" dc=""
for a in "$@"; do case "$a" in https://*) url="$a" ;; device_code@*) dc="${a#device_code@}" ;; esac; done
case "$url" in
    */login/device/code)
        printf '{"device_code":"DC-FAKE-0000","user_code":"ABCD-1234","verification_uri":"https://example.invalid/device","interval":1,"expires_in":3}' ;;
    */login/oauth/access_token)
        echo x >> "$TF_DIR/token-calls"
        if [ "$dc" = - ]; then cat > "$TF_DIR/dc-stdin"; else printf '%s\n' "$dc" > "$TF_DIR/dc-file"; fi
        case "$TF_GH" in
            block) : > "$TF_DIR/silent"; exec "$TF_SLEEP" 600 ;;
            *) exit 7 ;;
        esac ;;
esac
CURL
chmod +x "$GH"/*
gh_request() {
    env PATH="$GH:$PATH" bash "$SCRIPTS_DIR/sot-gh-auth.sh" request > /dev/null 2>&1 \
        || { echo "FATAL: sot-gh-auth.sh request failed" >&2; exit 2; }
    rm -f -- "$TF_DIR/token-calls" "$TF_DIR/dc-stdin" "$TF_DIR/dc-file"
}
gh_check() {  # the state file is gone, one token call per lap counted by the caller, the code came on stdin whole
    local bad=0
    [ ! -e "$SOT_COMM_HOME/gh-device-auth.json" ] || { echo "    the state file is still there"; bad=1; }
    [ "$(cat "$TF_DIR/dc-stdin" 2>/dev/null)" = DC-FAKE-0000 ] \
        || { echo "    curl did not read the device code whole from stdin (a file: $(cat "$TF_DIR/dc-file" 2>/dev/null))"; bad=1; }
    [ "$(wc -l < "$TF_DIR/token-calls")" -eq "$1" ] \
        || { echo "    $(wc -l < "$TF_DIR/token-calls" | tr -d ' ') token calls, want $1"; bad=1; }
    return "$bad"
}
for s in TERM HUP INT; do
    gh_request
    tf_signal "G1 sot-gh-auth poll (a signal ends it)" "$s" env PATH="$GH:$PATH" TF_GH=block bash "$SCRIPTS_DIR/sot-gh-auth.sh" poll
    gh_check 1 || { echo "FAIL: G1 sot-gh-auth poll (a signal ends it) $s: after the signal"; TF_FAILED=$((TF_FAILED + 1)); }
done
gh_request
g2_check() {
    grep -q "timed out" "$TF_DIR/err" || { echo "    no 'timed out' line"; return 1; }
    gh_check 3
}
TF_WANT_RC=7 TF_CHECK=g2_check tf_row "G2 sot-gh-auth poll (the network down)" ok env PATH="$GH:$PATH" TF_GH=down \
    bash "$SCRIPTS_DIR/sot-gh-auth.sh" poll

tf_report
