# lib-temp-files.sh — what the two temp-file suites share (comm/tests/test-temp-files.sh and
# agents/tests/test-temp-files.sh; test code only, never deployed). A suite sources it after lib-home-guard.sh and
# lib-wait.sh, and calls `tf_setup WORK` right after `guard_fresh_home WORK`: the guard's TMPDIR and XDG_RUNTIME_DIR,
# fresh and empty, are the two temporary folders every row checks, and nothing of the suite's own goes in them.
# Functions only; sourcing runs nothing.
#
#   tf_setup WORK            TF_MARK (a random word), TF_SENT ("/tf-sent <mark>", a value that starts with "/"), the
#                            jq stand-in, and SOTD_BIN: a stand-in `sotd` that answers `topology relay-endpoint`
#                            with the stand-in daemon's socket and carries `stdio-bridge` to it (guard_bridge_stub)
#   tf_daemon_start          the stand-in daemon on TF_SOCK; every request op is answered from the table
#   tf_table JSON            the table: an op's payload, or "silent" (no answer; the request touches TF_DIR/silent);
#                            an op the table does not name is answered {"ok":true}
#   tf_row ROW PATH CMD...   one row: CMD with the jq stand-in in PATH's mode, then nothing may be left; on path ok,
#                            the function TF_CHECK names (when set) checks the value arrived whole
#   tf_signal ROW SIG CMD... one row: CMD until the daemon sees a silent op, SIG to its group (or its leader), then
#                            nothing may be left; with TF_HOLD, SIG reaches the group a second time while a cleanup
#                            trap's rm of a TF_HOLD file runs
#   tf_report                the summary line; returns the count of failed rows
#
# The jq stand-in fails (path `fail`) or hangs (path `kill`) a call whose value carries TF_MARK, in a `--rawfile`
# file or on the raw input of `-R`, as written or base64-encoded; it runs the real jq for every other call, and for
# every call on path `ok`. A row on path `kill` waits for the hang, then SIGKILLs the row's process group: what
# a script holds only on a path that a kill skips is then left behind. CMD runs in a process group of its own with
# INT and QUIT at their defaults (a background job of a script starts with them ignored), stdin empty.
#
# The held rm: with TF_HOLD set (a glob), an `rm` first on CMD's PATH, once the row has armed it (just before its
# signal), stops at an operand whose name matches TF_HOLD, marks TF_DIR/held and waits for TF_DIR/release before it
# removes; every other rm is rm. A shell that dies of a hang-up hangs up its group again, and a second Ctrl-C or TERM
# is ordinary: the hold puts that second signal exactly where it does harm, while a cleanup trap's rm runs. The test
# process's own PATH never changes.

TF_PASSED=0 TF_FAILED=0 TF_PID="" TF_RC="" TF_DAEMON_PID=""

tf_setup() {  # WORK
    TF_DIR="$1/tf"
    mkdir -p "$TF_DIR/bin" "$TF_DIR/in" "$TF_DIR/s" || return 1
    TF_REAL_JQ="$(command -v jq)" || { echo "lib-temp-files: no jq" >&2; return 1; }
    TF_MARK="tfmark${RANDOM}${RANDOM}${RANDOM}"
    TF_SENT="/tf-sent $TF_MARK"
    TF_SOCK="$TF_DIR/s/d.sock"
    TF_REAL_RM="$(command -v rm)" && TF_REAL_SLEEP="$(command -v sleep)" || return 1
    export TF_DIR TF_REAL_JQ TF_MARK TF_REAL_RM TF_REAL_SLEEP
    _tf_write_jq && _tf_write_daemon && _tf_write_rm || return 1
    local bridge
    bridge="$(guard_bridge_stub "$TF_DIR/bridge")" || return 1
    printf '#!/bin/sh\nif [ "$1" = topology ] && [ "$2" = relay-endpoint ]; then echo "unix:%s"; exit 0; fi\nexec "%s" "$@"\n' \
        "$TF_SOCK" "$bridge" > "$TF_DIR/bin/sotd" && chmod +x "$TF_DIR/bin/sotd" || return 1
    export SOTD_BIN="$TF_DIR/bin/sotd"
    tf_table '{}'
}

_tf_write_jq() {
    cat > "$TF_DIR/bin/jq" <<'JQ' && chmod +x "$TF_DIR/bin/jq"
#!/usr/bin/env bash
# The jq stand-in of lib-temp-files.sh.
hit() { grep -qF -- "$TF_MARK" "$1" 2>/dev/null || base64 -d < "$1" 2>/dev/null | grep -qF -- "$TF_MARK"; }
orig=("$@") raw=0 null=0 found=0 in="" filter=0 files=()
while [ $# -gt 0 ]; do
    case "$1" in
        --arg|--argjson|--slurpfile) shift 2 ;;
        --rawfile) [ $# -ge 3 ] && hit "$3" && found=1; shift 2 ;;
        --indent|-L) shift ;;
        -f|--from-file) filter=1; shift ;;
        --raw-input) raw=1 ;;
        --null-input) null=1 ;;
        --*) ;;
        -[A-Za-z]*) case "$1" in *R*) raw=1 ;; esac; case "$1" in *n*) null=1 ;; esac ;;
        *) if [ "$filter" = 1 ]; then files+=("$1"); else filter=1; fi ;;
    esac
    shift
done
# Raw input is the files named after the filter, else stdin (which a -n call does not read).
if [ "$raw" = 1 ]; then
    if [ "${#files[@]}" -gt 0 ]; then
        for f in "${files[@]}"; do hit "$f" && found=1; done
    elif [ "$null" = 0 ]; then
        in="$TF_DIR/in/$$"; cat > "$in"; hit "$in" && found=1
    fi
fi
if [ "$found" = 1 ]; then
    case "${TF_JQ_MODE:-ok}" in
        fail) : > "$TF_DIR/jq-hit"; exit 5 ;;
        kill) : > "$TF_DIR/jq-hit"; : > "$TF_DIR/silent"; exec sleep 600 ;;
    esac
fi
[ -n "$in" ] || exec "$TF_REAL_JQ" "${orig[@]}"
"$TF_REAL_JQ" "${orig[@]}" < "$in"; rc=$?
rm -f -- "$in"; exit "$rc"
JQ
}

_tf_write_daemon() {
    cat > "$TF_DIR/daemon.py" <<'PY'
# The stand-in daemon of lib-temp-files.sh: SOCKET TABLE SILENT READY.
import json, socket, sys, threading
sock_path, table_path, silent_path, ready_path = sys.argv[1:5]
srv = socket.socket(socket.AF_UNIX)
srv.bind(sock_path)
srv.listen(64)
open(ready_path, "w").close()

def serve(conn):
    try:
        for raw in conn.makefile("rb"):
            try:
                req = json.loads(raw)
            except ValueError:
                continue
            if not isinstance(req, dict) or req.get("kind") != "req":
                continue
            op = req.get("op")
            try:
                with open(table_path) as t:
                    table = json.load(t)
            except (OSError, ValueError):
                table = {}
            payload = table.get(op, {"ok": True})
            if payload == "silent":
                open(silent_path, "a").close()
                continue
            res = {"v": 1, "id": req.get("id", 1), "kind": "res", "op": op, "payload": payload}
            conn.sendall((json.dumps(res, separators=(",", ":")) + "\n").encode())
    except OSError:
        pass
    finally:
        conn.close()

while True:
    c, _ = srv.accept()
    threading.Thread(target=serve, args=(c,), daemon=True).start()
PY
}

_tf_write_rm() {
    mkdir -p "$TF_DIR/hold" && cat > "$TF_DIR/hold/rm" <<'RM' && chmod +x "$TF_DIR/hold/rm"
#!/bin/sh
# The held rm of lib-temp-files.sh.
[ -e "$TF_DIR/armed" ] || exec "$TF_REAL_RM" "$@"
for a in "$@"; do
    case "${a##*/}" in
        $TF_HOLD) : > "$TF_DIR/held"
            while [ ! -e "$TF_DIR/release" ]; do "$TF_REAL_SLEEP" 0.05; done
            break ;;
    esac
done
exec "$TF_REAL_RM" "$@"
RM
}

tf_table() {  # JSON
    printf '%s\n' "$1" > "$TF_DIR/table.json"
}

tf_daemon_start() {
    rm -f -- "$TF_DIR/ready"
    python3 "$TF_DIR/daemon.py" "$TF_SOCK" "$TF_DIR/table.json" "$TF_DIR/silent" "$TF_DIR/ready" &
    TF_DAEMON_PID=$!
    await test -e "$TF_DIR/ready" || { echo "lib-temp-files: the stand-in daemon never listened" >&2; return 1; }
}

tf_daemon_stop() {
    [ -n "$TF_DAEMON_PID" ] || return 0
    kill "$TF_DAEMON_PID" 2>/dev/null
    wait "$TF_DAEMON_PID" 2>/dev/null
    TF_DAEMON_PID=""
}

# _tf_start MODE CMD... — CMD in its own process group (TF_PID is its pid and the group), the jq stand-in first on
# PATH in MODE; its output in TF_DIR/out and TF_DIR/err.
_tf_start() {
    local mode="$1"; shift
    rm -f -- "$TF_DIR/jq-hit" "$TF_DIR/silent" "$TF_DIR/armed" "$TF_DIR/held" "$TF_DIR/release"
    env PATH="${TF_HOLD:+$TF_DIR/hold:}$TF_DIR/bin:$PATH" TF_JQ_MODE="$mode" TF_HOLD="${TF_HOLD:-}" \
        perl -e '$SIG{INT} = $SIG{QUIT} = "DEFAULT"; setpgrp(0, 0) or die "setpgrp: $!"; exec { $ARGV[0] } @ARGV or die "exec: $!"' \
        "$@" < /dev/null > "$TF_DIR/out" 2> "$TF_DIR/err" &
    TF_PID=$!
}

_tf_group_gone() { ! kill -0 -- "-$TF_PID" 2>/dev/null; }

# _tf_end [SIG [leader]] — SIG (if given) to the row's group, or to its leader alone; TF_PID's status in TF_RC; then a
# wait until the group is empty.
# The hang guard is a watch that SIGKILLs the group when it has not emptied within await's 30 s.
_tf_end() {
    local watch
    rm -f -- "$TF_DIR/guard"
    if [ "${2:-}" = leader ]; then kill -"$1" "$TF_PID" 2>/dev/null
    elif [ -n "${1:-}" ]; then kill -"$1" -- "-$TF_PID" 2>/dev/null; fi
    ( await _tf_group_gone || { : > "$TF_DIR/guard"; kill -KILL -- "-$TF_PID" 2>/dev/null; } ) &
    watch=$!
    wait "$TF_PID" 2> /dev/null; TF_RC=$?
    await _tf_group_gone
    wait "$watch"
    [ ! -e "$TF_DIR/guard" ] || { echo "    did not end within the 30 s guard"; return 1; }
}

# _tf_clean — 0 when both temporary folders are empty; else names and removes what is there, and 1.
_tf_clean() {
    local d f n=0
    for d in "$TMPDIR" "$XDG_RUNTIME_DIR"; do
        for f in "$d"/* "$d"/.[!.]*; do
            [ -e "$f" ] || [ -L "$f" ] || continue
            echo "    left: ${f#"$d"/} in ${d##*/}"
            rm -rf -- "${f:?}"
            n=$((n + 1))
        done
    done
    [ "$n" -eq 0 ]
}

_tf_verdict() {  # ROW PATH OK
    if [ "$3" = 0 ]; then
        echo "PASS: $1 $2"; TF_PASSED=$((TF_PASSED + 1))
    else
        echo "FAIL: $1 $2"; TF_FAILED=$((TF_FAILED + 1))
        sed 's/^/    stderr: /' "$TF_DIR/err" | tail -n 5
    fi
}

# tf_row ROW PATH CMD... — PATH ok: CMD runs to its end with the real jq and exits 0 (or TF_WANT_RC), and the function
# TF_CHECK names, when set, passes. fail: CMD runs to its end with
# the call that carries the mark failing; the call must be reached. kill: CMD is SIGKILLed while that call hangs.
# Each then leaves nothing in the temporary folders.
tf_row() {
    local row="$1" path="$2" bad=0; shift 2
    _tf_start "$path" "$@"
    case "$path" in
        kill)
            if await test -e "$TF_DIR/jq-hit"; then _tf_end KILL || bad=1
            else echo "    the call that carries the mark was never reached"; _tf_end KILL; bad=1; fi ;;
        *)
            _tf_end || bad=1
            case "$path" in
                ok) [ "$TF_RC" = "${TF_WANT_RC:-0}" ] || { echo "    exited $TF_RC, want ${TF_WANT_RC:-0}"; bad=1; }
                    [ -z "${TF_CHECK:-}" ] || "$TF_CHECK" || bad=1 ;;
                fail) [ -e "$TF_DIR/jq-hit" ] || { echo "    the call that carries the mark was never reached"; bad=1; } ;;
            esac ;;
    esac
    _tf_clean || bad=1
    _tf_verdict "$row" "$path" "$bad"
}

# tf_signal ROW SIG CMD... — CMD runs until TF_DIR/silent appears (the stand-in daemon touches it on an op the table
# makes silent; a row's own stand-in may too, and so does the jq stand-in when TF_SIGNAL_PATH is kill and the call
# that carries the mark hangs); then SIG goes to its group, or to CMD alone when TF_SIGNAL_TARGET is leader. With
# TF_HOLD, SIG goes to the group again once a cleanup trap's rm of a TF_HOLD file is held, and then the rm is released.
# CMD must end by that signal (status 128 + its number), leave nothing in the temporary folders, and pass the function
# TF_CHECK names, when set.
tf_signal() {
    local row="$1" sig="$2" bad=0 want; shift 2
    case "$sig" in HUP) want=129 ;; INT) want=130 ;; TERM) want=143 ;; *) want="" ;; esac
    _tf_start "${TF_SIGNAL_PATH:-ok}" "$@"
    if await test -e "$TF_DIR/silent"; then
        if [ -n "${TF_HOLD:-}" ]; then
            {   # stderr: the shell's own notice that the row's leader died of the signal
                : > "$TF_DIR/armed"
                kill -"$sig" -- "-$TF_PID"
                if await test -e "$TF_DIR/held"; then kill -"$sig" -- "-$TF_PID"
                else echo "    no rm of a $TF_HOLD file ran after the $sig"; bad=1; fi
                : > "$TF_DIR/release"
                _tf_end || bad=1
            } 2>/dev/null
        else
            _tf_end "$sig" "${TF_SIGNAL_TARGET:-}" || bad=1
        fi
        [ "$TF_RC" = "$want" ] || { echo "    exited $TF_RC, want $want (ended by $sig)"; bad=1; }
        [ -z "${TF_CHECK:-}" ] || "$TF_CHECK" || bad=1
    else
        echo "    the daemon never received the silent request"; _tf_end KILL; bad=1
    fi
    _tf_clean || bad=1
    _tf_verdict "$row" "$sig" "$bad"
}

tf_report() {
    echo "temp files: $TF_PASSED passed, $TF_FAILED failed"
    return "$TF_FAILED"
}
