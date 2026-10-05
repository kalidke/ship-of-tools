# Part of test-hub-files.sh, sourced by it: the hub stub, wire sends, the faked Windows pipe send, the read window, T6.
# T5 — a stub `nc` stands in for the hub on a unix: endpoint: each connection
# is one run of it. Its `comm.file` answer is the payload in $HUB/answer
# ("" = silence), given after $HUB/wait seconds; an `agent.send` is acked and
# receipted by fe@far. Every frame it reads is logged by op.
HUB="$WORK/hub"
write_hub_stub() {  # PAYLOAD [WAIT]
    rm -rf "${HUB:?}"; mkdir -p "$HUB"
    printf '%s' "$1" > "$HUB/answer"; printf '%s' "${2:-0}" > "$HUB/wait"
    { printf '#!/bin/sh\nd=%s\n' "$HUB"; cat <<'STUB'
while IFS= read -r line; do
    case "$line" in
        *'"op":"comm.file"'*)
            printf '%s\n' "$line" >> "$d/comm-file.log"; sleep "$(cat "$d/wait")"
            [ -s "$d/answer" ] && printf '{"v":1,"id":1,"kind":"res","op":"comm.file","payload":%s}\n' "$(cat "$d/answer")"
            exit 0 ;;
        *'"op":"agent.send"'*)
            printf '%s\n' "$line" >> "$d/agent-send.log"
            id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
            printf '{"v":1,"id":1,"kind":"res","op":"agent.send","payload":{"ok":true,"receivers":["fe@far"],"id":"%s"}}\n' "$id"
            printf '{"v":1,"id":1,"kind":"evt","op":"agent.receipt","payload":{"id":"%s","filer":"fe@far"}}\n' "$id"
            exit 0 ;;
    esac
done
STUB
    } > "$HUB/nc"; chmod +x "$HUB/nc"
}
# wire_send [VAR=VALUE...] — `comm-relay.sh send @t-far`, a handle this box's
# registry does not name, so the send goes to the wire.
wire_send() {
    SEND_OUT="$(cd "$WORK" && PATH="$HUB:$PATH" SOT_COMM_SELF_FILE="$WORK/self-sender.txt" \
        SOT_COMM_TEST_HOST="$HOST_PIN" SOT_RELAY_ENDPOINT="unix:$WORK/hub.sock" \
        env "$@" "$BIN/comm-relay.sh" send @t-far "/to the far box" 2>"$WORK/err.txt")"
    SEND_RC=$?
    SEND_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

# On Windows `_sot_os_user` is the process token's user SID, read by the first of three probes (`whoami` called
# directly, `whoami` through `cmd`, PowerShell) that prints one; a box where none does fails loudly and says what each
# printed. Faked here with OS=Windows_NT and stubs first on PATH.
case_the_windows_account_is_the_sid_from_the_first_probe_that_prints_one() {
    local d="$WORK/sidfake" got err
    sid_of() {  # <stubs...> -- run _sot_os_user with $d as the only probe dir
        got="$(cd "$WORK" && PATH="$d:$PATH" OS=Windows_NT SOT_COMM_TEST_HOST="$HOST_PIN" bash -c '. "$1/comm-lib.sh"; _sot_os_user' _ "$SCRIPTS_DIR" 2>"$WORK/err.txt")" || true
        err="$(cat "$WORK/err.txt")"
    }
    stub() {  # <name> <output...>
        local n="$1"; shift
        printf '#!/bin/sh\nprintf "%%s\\r\\n" %s\n' "$(printf "'%s' " "$@")" > "$d/$n"; chmod +x "$d/$n"
    }
    rm -rf "${d:?}"; mkdir -p "$d"
    stub whoami '"fakehost\\fakeuser","S-1-5-21-1-2-3-1001"'
    sid_of; [ "$got" = "S-1-5-21-1-2-3-1001" ] || { echo "  whoami probe: got '$got' ($err)"; return 1; }
    rm -rf "${d:?}"; mkdir -p "$d"
    stub whoami 'not a sid'
    stub cmd '"fakehost\\fakeuser","S-1-5-21-1-2-3-1002"'
    sid_of; [ "$got" = "S-1-5-21-1-2-3-1002" ] || { echo "  cmd probe: got '$got' ($err)"; return 1; }
    rm -rf "${d:?}"; mkdir -p "$d"
    stub whoami 'not a sid'
    stub cmd 'also not'
    stub powershell.exe 'S-1-5-21-1-2-3-1003'
    sid_of; [ "$got" = "S-1-5-21-1-2-3-1003" ] || { echo "  powershell probe: got '$got' ($err)"; return 1; }
    rm -rf "${d:?}"; mkdir -p "$d"
    stub whoami 'not a sid'
    stub cmd 'also not'
    stub powershell.exe 'nor this'
    sid_of
    [ -z "$got" ] || { echo "  no probe: got '$got'"; return 1; }
    contains "$err" "no probe printed this process's user SID" && contains "$err" "[whoami: not a sid]" && contains "$err" "[cmd: also not]" \
        && contains "$err" "_sot_os_user: this process's OS account is unreadable" || { echo "  err: $err"; return 1; }
    return 0
}

# A daemon that refuses the hello (a second OS account on the host, an older protocol) ends the connection:
# `sot_oneshot_request` says why on stderr, prints no reply and returns 1, never the next op's silence (ADR 0049
# `## User isolation`).
case_a_refused_hello_is_named_by_the_oneshot_request() {
    rm -rf "${HUB:?}"; mkdir -p "$HUB"
    { printf '#!/bin/sh\n'; cat <<'STUB'
IFS= read -r line
case "$line" in
    *'"op":"hello"'*) printf '{"v":1,"id":1,"kind":"res","op":"hello","payload":{"error":"host-a has said hello as more than one OS account","code":"os_user_conflict"}}\n' ;;
esac
STUB
    } > "$HUB/nc"; chmod +x "$HUB/nc"
    local out rc=0 err
    out="$(cd "$WORK" && PATH="$HUB:$PATH" SOT_COMM_TEST_HOST="$HOST_PIN" SOT_SEND_TIMEOUT=5 bash -c '
        . "$1/comm-lib.sh"; ENDPOINT="unix:$2/hub.sock"
        sot_oneshot_request "{\"v\":1,\"id\":1,\"kind\":\"req\",\"op\":\"version.query\",\"payload\":{}}" version.query' _ "$SCRIPTS_DIR" "$WORK" 2>"$WORK/err.txt")" || rc=$?
    err="$(cat "$WORK/err.txt" 2>/dev/null)"
    [ "$rc" -eq 1 ] || { echo "  rc $rc, want 1 (out: $out err: $err)"; return 1; }
    [ -z "$out" ] || { echo "  a refused hello printed a reply: $out"; return 1; }
    [ "$err" = "sot_oneshot_request: hello refused: host-a has said hello as more than one OS account" ] \
        || { echo "  err: $err"; return 1; }
    return 0
}

# A refusal that is not about the protocol comes from a daemon of this release, which closes: the one-shot stops at it at
# once, though its transport (here a stub that holds the connection) stays open. The sender's window is never waited out.
case_a_hello_refusal_stops_the_oneshot_request_at_once() {
    rm -rf "${HUB:?}"; mkdir -p "$HUB"
    { printf '#!/bin/sh\n'; cat <<'STUB'
IFS= read -r line
printf '{"v":1,"id":1,"kind":"res","op":"hello","payload":{"error":"host-a has said hello as more than one OS account","code":"os_user_conflict"}}\n'
exec sleep 8
STUB
    } > "$HUB/nc"; chmod +x "$HUB/nc"
    local out rc=0 err t0 t1
    t0="$(date +%s)"
    out="$(cd "$WORK" && PATH="$HUB:$PATH" SOT_COMM_TEST_HOST="$HOST_PIN" SOT_SEND_TIMEOUT=10 bash -c '
        . "$1/comm-lib.sh"; ENDPOINT="unix:$2/hub.sock"
        sot_oneshot_request "{\"v\":1,\"id\":1,\"kind\":\"req\",\"op\":\"version.query\",\"payload\":{}}" version.query' _ "$SCRIPTS_DIR" "$WORK" 2>"$WORK/err.txt")" || rc=$?
    t1="$(date +%s)"
    err="$(cat "$WORK/err.txt" 2>/dev/null)"
    [ "$rc" -eq 1 ] || { echo "  rc $rc, want 1 (out: $out err: $err)"; return 1; }
    [ $((t1 - t0)) -le 2 ] || { echo "  took $((t1 - t0)) s, want at most 2"; return 1; }
    [ "$err" = "sot_oneshot_request: hello refused: host-a has said hello as more than one OS account" ] \
        || { echo "  err: $err"; return 1; }
    return 0
}

# An older daemon refuses only the protocol and then answers the request (a second later here, so a client that stops at the
# refusal is caught): the one-shot returns that answer and says nothing.
case_a_protocol_refusal_does_not_decide_the_oneshot_request() {
    rm -rf "${HUB:?}"; mkdir -p "$HUB"
    { printf '#!/bin/sh\n'; cat <<'STUB'
while IFS= read -r line; do
    case "$line" in
        *'"op":"hello"'*) printf '{"v":1,"id":1,"kind":"res","op":"hello","payload":{"error":"protocol mismatch","code":"protocol_mismatch"}}\n' ;;
        *'"op":"version.query"'*) sleep 1; printf '{"v":1,"id":1,"kind":"res","op":"version.query","payload":{"ok":true}}\n'; exit 0 ;;
    esac
done
STUB
    } > "$HUB/nc"; chmod +x "$HUB/nc"
    local out rc=0 err
    out="$(cd "$WORK" && PATH="$HUB:$PATH" SOT_COMM_TEST_HOST="$HOST_PIN" SOT_SEND_TIMEOUT=5 bash -c '
        . "$1/comm-lib.sh"; ENDPOINT="unix:$2/hub.sock"
        sot_oneshot_request "{\"v\":1,\"id\":1,\"kind\":\"req\",\"op\":\"version.query\",\"payload\":{}}" version.query' _ "$SCRIPTS_DIR" "$WORK" 2>"$WORK/err.txt")" || rc=$?
    err="$(cat "$WORK/err.txt" 2>/dev/null)"
    [ "$rc" -eq 0 ] || { echo "  rc $rc, want 0 (out: $out err: $err)"; return 1; }
    printf '%s' "$out" | jq -e '.op == "version.query" and .payload.ok == true' >/dev/null 2>&1 || { echo "  reply: $out"; return 1; }
    [ -z "$err" ] || { echo "  stderr: $err"; return 1; }
    return 0
}

# S4 — a directed wire send with no daemon found is that send's FAILED line.
case_a_wire_send_with_no_daemon_is_failed() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local out rc=0 err
    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-sender.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
        env -u SOT_RELAY_ENDPOINT -u SOT_SOCKET "$BIN/comm-relay.sh" send @t-far "no daemon" 2>"$WORK/err.txt")" || rc=$?
    err="$(cat "$WORK/err.txt" 2>/dev/null)"
    [ "$rc" -eq 1 ] || { echo "  rc $rc, want 1 (out: $out err: $err)"; return 1; }
    contains "$err" "FAILED -> @t-far: no sotd daemon found; " || { echo "  err: $err"; return 1; }
    contains "$err$out" "ERROR:" && { echo "  an ERROR line: $err"; return 1; }
    return 0
}

# T5 on a faked Windows box: `uname` says MINGW, this box's own daemon is a
# fake `sotd.exe` under a fake LOCALAPPDATA, and a fake `powershell.exe` is
# the pipe transport (fakes from test-join-disambiguation.sh's Windows
# discovery case). It answers the connect probe, logs each oneshot's argv and
# the frames on its stdin, and answers `comm.file` with $WINHUB/answer
# ("" = silence). A copy of the scripts WITHOUT this file's endpoint stubs, so
# the real Windows discovery runs.
WINBIN="$WORK/winbin"; WINFAKE="$WORK/winfake"; WINAPP="$WORK/winappdata"; WINHUB="$WORK/winhub"
cp -r "$SCRIPTS_DIR" "$WINBIN"
# Cygwin's /proc as the Windows walk reads it: each script that sources the library
# is one MSYS process under a native parent (Windows pid 1000).
cat >> "$WINBIN/comm-lib.sh" <<WINPROC

# ---- test only: a Cygwin /proc stand-in ----
_SOT_PROC="$WORK/winproc/\$\$"; mkdir -p "\$_SOT_PROC/\$\$"
printf '%s (bash) S 1\n' "\$\$" > "\$_SOT_PROC/\$\$/stat"; printf 'bash\0' > "\$_SOT_PROC/\$\$/cmdline"; echo 1000 > "\$_SOT_PROC/\$\$/winpid"
WINPROC
mkdir -p "$WINFAKE" "$WINAPP/sot/bin" "$WINHUB"
printf '#!/bin/sh\necho "MINGW64_NT-10.0-19045"\n' > "$WINFAKE/uname"
cat > "$WINAPP/sot/bin/sotd.exe" <<'FAKESOTD'
#!/bin/sh
if [ "$1" = session-socket-path ] && [ "$2" = local ]; then printf '%s\n' '\\.\pipe\sot-fakeuser-local'; exit 0; fi
# `ancestors --from`: the one process above the comm script's shell, no agent among them.
if [ "$1" = ancestors ] && [ "$2" = --from ]; then printf '1001\tbash.exe\tbash.exe\n'; exit 0; fi
exit 1
FAKESOTD
{ printf '#!/bin/sh\nd=%s\n' "$WINHUB"; cat <<'FAKEPS'
case " $* " in *" -File "*) ;; *) exit 0 ;; esac
printf '%s\n' "$*" >> "$d/argv.log"
while IFS= read -r line; do
    printf '%s\n' "$line" >> "$d/stdin.log"
    case "$line" in
        *'"op":"hello"'*) ;;
        *'"op":"comm.file"'*)
            [ -s "$d/answer" ] && printf '{"v":1,"id":1,"kind":"res","op":"comm.file","payload":%s}\n' "$(cat "$d/answer")"
            exit 0 ;;
        *) exit 0 ;;
    esac
done
FAKEPS
} > "$WINFAKE/powershell.exe"
# `cmd //c "whoami /user /fo csv /nh"`, the hello's `os_user` on Windows (comm-lib-client.sh `_sot_os_user`).
printf '#!/bin/sh\nprintf '"'"'"fakehost\\\\fakeuser","S-1-5-21-1-2-3-1001"\\r\\n'"'"'\n' > "$WINFAKE/cmd"
chmod +x "$WINFAKE/uname" "$WINFAKE/powershell.exe" "$WINAPP/sot/bin/sotd.exe" "$WINFAKE/cmd"
win_send() {  # ANSWER
    rm -f "${WINHUB:?}"/*.log; printf '%s' "$1" > "$WINHUB/answer"
    SEND_OUT="$(cd "$WORK" && unset OS OSTYPE SOT_SOCKET SOTD_BIN && PATH="$WINFAKE:$PATH" LOCALAPPDATA="$WINAPP" \
        SOT_COMM_SELF_FILE="$WORK/self-sender.txt" SOT_COMM_TEST_HOST="$HOST_PIN" SOT_SEND_TIMEOUT=3 \
        SOT_INBOX_LOCK_WAIT_SECS=1 "$WINBIN/comm-send.sh" @t-peer "/win text on stdin only" 2>"$WORK/err.txt")"
    SEND_RC=$?
    SEND_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
}

case_a_windows_send_is_one_comm_file_over_the_pipe() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local frame
    win_send '{"ok":true}'
    [ "$SEND_RC" -eq 0 ] || { echo "  ok: rc $SEND_RC (out: $SEND_OUT err: $SEND_ERR)"; return 1; }
    contains "$SEND_OUT" "filed -> @t-peer" || { echo "  ok: out: $SEND_OUT"; return 1; }
    [ ! -s "$INBOX/t-peer.jsonl" ] || { echo "  the send appended locally"; return 1; }
    frame="$(grep '"op":"comm.file"' "$WINHUB/stdin.log")"
    [ "$(printf '%s\n' "$frame" | wc -l)" -eq 1 ] || { echo "  comm.file frames: $frame"; return 1; }
    printf '%s' "$frame" | jq -e '(.payload | has("id") | not) and .payload.to == "t-peer" and .payload.text == "/win text on stdin only"' >/dev/null \
        || { echo "  the frame: $frame"; return 1; }
    grep -q 'win text on stdin only' "$WINHUB/argv.log" && { echo "  the text reached argv"; return 1; }

    win_send '{"error":"no live session holds @t-peer","code":"no_live_session"}'
    [ "$SEND_RC" -eq 1 ] || { echo "  refusal: rc $SEND_RC"; return 1; }
    contains "$SEND_ERR" "FAILED -> @t-peer: no live session holds @t-peer" || { echo "  refusal: err: $SEND_ERR"; return 1; }

    win_send ''
    [ "$SEND_RC" -eq 1 ] || { echo "  silence: rc $SEND_RC"; return 1; }
    contains "$SEND_ERR" "FAILED -> @t-peer: the daemon did not answer at pipe:" || { echo "  silence: err: $SEND_ERR"; return 1; }
    [ ! -s "$INBOX/t-peer.jsonl" ] || { echo "  a send appended locally"; return 1; }
    return 0
}

case_a_wire_send_prints_the_hubs_answer() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local answer rc want got
    while IFS='|' read -r answer rc want; do
        write_hub_stub "$answer"; wire_send
        got="$SEND_OUT$SEND_ERR"
        [ "$SEND_RC" -eq "$rc" ] && [ "$got" = "$want" ] \
            || { echo "  [$answer] rc $SEND_RC, got '$got', want $rc '$want'"; return 1; }
        [ ! -e "$HUB/agent-send.log" ] || { echo "  [$answer] fell back to agent.send"; return 1; }
        jq -e '.op == "comm.file" and .payload == {from:"t-sender",to:"t-far",text:"/to the far box",broadcast:false}' \
            "$HUB/comm-file.log" >/dev/null && [ "$(wc -l < "$HUB/comm-file.log")" -eq 1 ] \
            || { echo "  [$answer] frame: $(cat "$HUB/comm-file.log")"; return 1; }
    done <<CASES
{"ok":true}|0|filed -> @t-far
{"error":"not a handle: t-far","code":"bad_handle"}|1|FAILED -> @t-far: not a handle: t-far
{"error":"no live session holds @t-far","code":"no_live_session"}|1|FAILED -> @t-far: no live session holds @t-far
{"error":"the append failed: disk full","code":"file_failed"}|1|FAILED -> @t-far: the append failed: disk full
{"error":"unknown op: comm.file"}|1|FAILED -> @t-far: unknown op: comm.file
|1|FAILED -> @t-far: the daemon did not answer at unix:$WORK/hub.sock
CASES
    # not_here, and only it, falls back to the not-mine leg (deleted in B2).
    write_hub_stub '{"error":"no box knows that handle: t-far","code":"not_here"}'; wire_send
    [ "$SEND_RC" -eq 0 ] && [ "$SEND_OUT" = "filed -> @t-far (by fe@far, relay)" ] \
        || { echo "  not_here: rc $SEND_RC ($SEND_OUT$SEND_ERR)"; return 1; }
    [ "$(wc -l < "$HUB/agent-send.log")" -eq 1 ] || { echo "  not_here: no agent.send fallback"; return 1; }
    # The guard's own route reads the same answer with the same helper, and
    # for it not_here is FAILED: this box's registry named the handle.
    fresh_route
    got="$(route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" unix:/own "" \
        '{"v":1,"id":1,"kind":"res","op":"comm.file","payload":{"error":"no box knows that handle: t-peer","code":"not_here"}}')"; rc=$?
    [ "$rc" -eq 1 ] && [ "$got" = "no box knows that handle: t-peer" ] || { echo "  guard not_here: rc $rc ($got)"; return 1; }
    return 0
}

# The hub may wait its whole lock bound before it files; a read window no
# longer than that would call a filed line FAILED and the sender would resend
# it. A 1s lock wait and a caller's 1s send timeout: the window is still the
# lock wait plus 10s, so an answer at 2s is filed.
case_the_read_window_outlasts_the_hubs_lock_wait() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    write_hub_stub '{"ok":true}' 2
    wire_send SOT_INBOX_LOCK_WAIT_SECS=1 SOT_SEND_TIMEOUT=1
    [ "$SEND_RC" -eq 0 ] && [ "$SEND_OUT" = "filed -> @t-far" ] \
        || { echo "  rc $SEND_RC ($SEND_OUT$SEND_ERR)"; return 1; }
    return 0
}

# T6 — the hub's line (file_frame's shape, repo "daemon") and a local one.
case_a_hub_line_and_a_local_line_read_alike() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local out n
    append_one "$PEER" '{"from":"t-sender","to":"t-peer","repo":"r","msg":"local line","ts":"2026-09-30T00:00:01Z"}' \
        || { echo "  the local append failed"; return 1; }
    printf '%s\n' '{"from":"t-sender","to":"t-peer","repo":"daemon","msg":"hub line","ts":"2026-09-30T00:00:02Z"}' \
        >> "$INBOX/$PEER.jsonl"
    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" "$BIN/comm-poll.sh" 2>&1)"
    n="$(printf '%s\n' "$out" | grep -c -E '^\[2026-09-30T00:00:0[12]Z\] \[t-sender:(r|daemon)\] (local|hub) line$')"
    [ "$n" -eq 2 ] || { echo "  $n of 2 lines rendered alike: $out"; return 1; }
    out="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" "$BIN/comm-poll.sh" 2>&1)"
    contains "$out" "No new messages." || { echo "  the cursor did not pass both: $out"; return 1; }
    return 0
}
