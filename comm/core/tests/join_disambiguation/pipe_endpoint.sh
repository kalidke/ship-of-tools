# test-join-disambiguation.sh part: the LU6e pipe: and simulated-Windows endpoint cases (sourced in order by the entry).

# --- LU6e: the pipe: endpoint (ADR 0042 amendment, decision 5) ----------
# This box has no real Windows/PowerShell to test against, so these cases
# prove the BASH side only — dispatch on the pipe: prefix, the argv shape
# handed to powershell.exe, capture into sot_oneshot_request's own $tmp
# poll loop (unchanged for pipe:), and the clean-failure paths — via a
# STUB powershell.exe on PATH standing in for a live named pipe. What the
# real comm-pipe-request.ps1's own NamedPipeClientStream/JSON-filtering
# logic does is NOT exercised here (no pwsh on this box); see the LU6e
# implementation report for the static review of that file.

case_pipe_endpoint_oneshot_request_matches_reply() {
    local fakebin argvlog frame out
    fakebin="$WORK/fake-powershell-oneshot"
    mkdir -p "$fakebin"
    # Echoes a canned res line for whatever -Op/-PipeName it was invoked
    # with, and logs its own argv so the test can assert the invocation
    # shape sot_oneshot_request's pipe: arm produces.
    argvlog="$WORK/fake-powershell-oneshot-argv.log"
    rm -f "${argvlog:?}"
    cat > "$fakebin/powershell.exe" <<FAKEPS
#!/bin/sh
op=""
pipename=""
while [ \$# -gt 0 ]; do
    case "\$1" in
        -Op) op="\$2"; shift 2 ;;
        -PipeName) pipename="\$2"; shift 2 ;;
        *) shift ;;
    esac
done
printf '%s %s\n' "\$pipename" "\$op" >> "$argvlog"
printf '{"v":1,"id":9,"kind":"res","op":"%s","payload":{"ok":true,"stub":true}}\n' "\$op"
FAKEPS
    chmod +x "$fakebin/powershell.exe"

    frame='{"v":1,"id":2,"kind":"req","op":"workspace.list","payload":{}}'

    # Full \\.\pipe\<name> form (what sot_daemon_endpoint actually prints).
    out="$(PATH="$fakebin:$PATH" SCRIPT_DIR="$SCRIPTS_DIR" \
        ENDPOINT='pipe:\\.\pipe\sot-testuser-local' SOT_SEND_TIMEOUT=5 \
        sot_oneshot_request "$frame" workspace.list)"
    contains "$out" '"op":"workspace.list"' \
        || { echo "  full-path pipe: endpoint didn't return the stub's matching reply: got '$out'"; return 1; }
    contains "$out" '"stub":true' \
        || { echo "  reply missing the stub marker: $out"; return 1; }
    contains "$(cat "$argvlog" 2>/dev/null)" "sot-testuser-local workspace.list" \
        || { echo "  argv didn't carry the normalised bare pipe name + op: $(cat "$argvlog" 2>/dev/null)"; return 1; }

    # Bare pipe:<name> form must normalise identically (a no-op strip).
    rm -f "${argvlog:?}"
    out="$(PATH="$fakebin:$PATH" SCRIPT_DIR="$SCRIPTS_DIR" \
        ENDPOINT='pipe:sot-testuser-local' SOT_SEND_TIMEOUT=5 \
        sot_oneshot_request "$frame" workspace.list)"
    contains "$out" '"op":"workspace.list"' \
        || { echo "  bare pipe: endpoint didn't return the stub's matching reply: got '$out'"; return 1; }
    contains "$(cat "$argvlog" 2>/dev/null)" "sot-testuser-local workspace.list" \
        || { echo "  bare-name argv mismatch: $(cat "$argvlog" 2>/dev/null)"; return 1; }
    return 0
}

case_pipe_endpoint_oneshot_request_fails_cleanly_with_no_powershell() {
    # No powershell.exe anywhere on PATH -- must fail FAST (the check runs
    # before anything is backgrounded) and CLEANLY: empty stdout, nonzero
    # return, never a hang for the full SOT_SEND_TIMEOUT window.
    local emptybin frame out rc
    emptybin="$WORK/no-powershell-bin"
    mkdir -p "$emptybin"
    frame='{"v":1,"id":2,"kind":"req","op":"workspace.list","payload":{}}'
    out="$(PATH="$emptybin:/usr/bin:/bin" SCRIPT_DIR="$SCRIPTS_DIR" \
        ENDPOINT='pipe:sot-testuser-local' SOT_SEND_TIMEOUT=5 \
        sot_oneshot_request "$frame" workspace.list)"
    rc=$?
    [ -z "$out" ] || { echo "  expected no reply with no powershell.exe on PATH, got: $out"; return 1; }
    [ "$rc" -ne 0 ] || { echo "  expected a nonzero return with no powershell.exe on PATH"; return 1; }
    return 0
}

case_pipe_endpoint_oneshot_request_fails_cleanly_with_missing_ps1() {
    # comm-pipe-request.ps1 absent from SCRIPT_DIR (a broken/partial
    # deploy) -- must fail before ever invoking powershell.exe, not with a
    # cryptic failure from inside a backgrounded job.
    local fakebin emptyscriptdir frame out rc err
    fakebin="$WORK/fake-powershell-missing-ps1"
    mkdir -p "$fakebin"
    cat > "$fakebin/powershell.exe" <<'FAKEPS'
#!/bin/sh
echo "should never run" >&2
exit 1
FAKEPS
    chmod +x "$fakebin/powershell.exe"
    emptyscriptdir="$WORK/empty-script-dir"
    mkdir -p "$emptyscriptdir"

    frame='{"v":1,"id":2,"kind":"req","op":"workspace.list","payload":{}}'
    out="$(PATH="$fakebin:$PATH" SCRIPT_DIR="$emptyscriptdir" \
        ENDPOINT='pipe:sot-testuser-local' SOT_SEND_TIMEOUT=5 \
        sot_oneshot_request "$frame" workspace.list 2>"$WORK/missing-ps1.err")"
    rc=$?
    err="$(cat "$WORK/missing-ps1.err" 2>/dev/null || true)"
    [ -z "$out" ] || { echo "  expected no reply with comm-pipe-request.ps1 missing, got: $out"; return 1; }
    [ "$rc" -ne 0 ] || { echo "  expected a nonzero return with comm-pipe-request.ps1 missing"; return 1; }
    contains "$err" "comm-pipe-request.ps1" \
        || { echo "  missing a diagnostic naming comm-pipe-request.ps1: $err"; return 1; }
    return 0
}

case_windows_pipe_discovery_returns_pipe_endpoint_and_skips_pgrep() {
    # sot_daemon_endpoint, on a simulated Windows host (faked via `uname`
    # exactly like the receive-path case above does, since
    # $OS/$OSTYPE are unset here): must ask the local daemon for its pipe
    # FIRST (the same query scripts/sot-local-daemon.ps1 makes), prove it
    # live with a bounded connect probe, and return pipe:<path> -- all
    # before ever reaching for pgrep, which is not on a stock git-bash
    # PATH. A fake sotd.exe answers `session-socket-path local`; a fake
    # powershell.exe simulates a live connect probe (exit 0); a fake pgrep
    # records whether it was ever invoked at all.
    local fakebin pgreplog appdata out
    fakebin="$WORK/win-discovery-bin"
    mkdir -p "$fakebin"
    cat > "$fakebin/uname" <<'FAKEUNAME'
#!/bin/sh
echo "MINGW64_NT-10.0-19045"
FAKEUNAME
    cat > "$fakebin/powershell.exe" <<'FAKEPS3'
#!/bin/sh
exit 0
FAKEPS3
    pgreplog="$WORK/win-discovery-pgrep.log"
    rm -f "${pgreplog:?}"
    cat > "$fakebin/pgrep" <<FAKEPGREP
#!/bin/sh
echo "pgrep called: \$*" >> "$pgreplog"
exit 1
FAKEPGREP
    chmod +x "$fakebin/uname" "$fakebin/powershell.exe" "$fakebin/pgrep"

    appdata="$WORK/win-discovery-localappdata"
    mkdir -p "$appdata/sot/bin"
    cat > "$appdata/sot/bin/sotd.exe" <<'FAKESOTD'
#!/bin/sh
if [ "$1" = "session-socket-path" ] && [ "$2" = "local" ]; then
    printf '%s\n' '\\.\pipe\sot-fakeuser-local'
    exit 0
fi
# C10 named check 1: the same fake must dispatch on argv, not just answer
# every call the same way -- `topology relay-endpoint` on a box with no
# hosts.toml answers this box's own endpoint (the Rust-side no-plan rule,
# isolation-plan.md §3 C10), which on this simulated Windows box is its
# own pipe.
if [ "$1" = "topology" ] && [ "$2" = "relay-endpoint" ]; then
    printf '%s\n' 'pipe:\\.\pipe\sot-fakeuser-local'
    exit 0
fi
exit 1
FAKESOTD
    chmod +x "$appdata/sot/bin/sotd.exe"

    out="$(
        unset OS OSTYPE SOT_SOCKET SOTD_BIN
        PATH="$fakebin:$PATH"
        LOCALAPPDATA="$appdata"
        sot_daemon_endpoint
    )"
    contains "$out" 'pipe:' \
        || { echo "  expected a pipe: endpoint on a simulated Windows host, got: $out"; return 1; }
    contains "$out" 'sot-fakeuser-local' \
        || { echo "  pipe endpoint missing the resolved pipe path: $out"; return 1; }
    [ ! -s "$pgreplog" ] \
        || { echo "  pgrep was invoked during Windows discovery (must never be): $(cat "$pgreplog")"; return 1; }
    return 0
}

case_windows_relay_endpoint_is_never_the_pipe_the_shell_probed() {
    # sot_relay_endpoint on the simulated Windows host: the SHELL no longer
    # decides "prefer this box's own pipe for relay traffic" at all (C10,
    # isolation-plan.md §3) -- it asks `sotd topology relay-endpoint` and
    # returns whatever the BINARY says, through the one gate. On this box
    # (no hosts.toml) the binary's own no-plan rule answers this box's own
    # endpoint, which happens to print as a pipe: value here -- but it is
    # the binary's answer, not the shell reaching for the pipe it already
    # knew about the way the old pipe-first discovery did (2026-09-08:
    # cross-host sends from a Windows session went dark exactly that way).
    local fakebin appdata out
    fakebin="$WORK/win-discovery-bin"
    appdata="$WORK/win-discovery-localappdata"
    [ -x "$fakebin/uname" ] && [ -x "$appdata/sot/bin/sotd.exe" ] \
        || { echo "  depends on case_windows_pipe_discovery_returns_pipe_endpoint_and_skips_pgrep's fakes"; return 1; }
    out="$(
        unset OS OSTYPE SOT_SOCKET SOTD_BIN SOT_RELAY_ENDPOINT
        PATH="$fakebin:$PATH"
        LOCALAPPDATA="$appdata"
        sot_relay_endpoint
    )"
    [ "$out" = 'pipe:\\.\pipe\sot-fakeuser-local' ] \
        || { echo "  expected the binary's own no-plan pipe: answer, got: $out"; return 1; }
    out="$(sot_relay_endpoint "unix:/explicit.sock")"
    [ "$out" = "unix:/explicit.sock" ] \
        || { echo "  an explicit endpoint must win verbatim, got: $out"; return 1; }
    return 0
}

