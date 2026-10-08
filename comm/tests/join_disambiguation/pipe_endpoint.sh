# test-join-disambiguation.sh part: the LU6e pipe: and simulated-Windows endpoint cases (sourced in order by the entry).

# --- LU6e: the pipe: endpoint (ADR 0042 amendment, decision 5) ----------
# This box has no real Windows pipe, so these cases prove the BASH side only:
# dispatch on the pipe: prefix through sot_dial, the endpoint handed to
# `sotd stdio-bridge --endpoint` (the full and the bare name both become
# pipe:\\.\pipe\<name>), capture into sot_oneshot_request's own $tmp poll
# loop, and the clean-failure paths, via a STUB sotd standing in for the
# bridge. What the real bridge does with a pipe (connect_own, the owner
# check) is rust/backend/tests/shell_dial.rs's, not this file's.

case_pipe_endpoint_oneshot_request_matches_reply() {
    local stub argvlog frame out
    stub="$WORK/fake-bridge-oneshot/sotd"
    mkdir -p "${stub%/*}"
    # A stand-in for `sotd stdio-bridge --endpoint <pipe>`: logs the endpoint it
    # was handed, and answers each request line but the hello with a canned res
    # for its op, until its input ends, as the real bridge closes at input end.
    argvlog="$WORK/fake-bridge-oneshot-argv.log"
    rm -f "${argvlog:?}"
    cat > "$stub" <<'FAKEBRIDGE'
#!/bin/sh
[ "$1" = stdio-bridge ] && [ "$2" = --endpoint ] || exit 97
printf '%s\n' "$3" >> "$FAKE_BRIDGE_ARGV"
while IFS= read -r line; do
    case "$line" in
        *'"op":"hello"'*) ;;
        *'"op":"'*)
            op="${line#*\"op\":\"}"; op="${op%%\"*}"
            printf '{"v":1,"id":9,"kind":"res","op":"%s","payload":{"ok":true,"stub":true}}\n' "$op" ;;
    esac
done
FAKEBRIDGE
    chmod +x "$stub"

    frame='{"v":1,"id":2,"kind":"req","op":"workspace.list","payload":{}}'

    # Full \\.\pipe\<name> form (what sot_daemon_endpoint actually prints).
    out="$(FAKE_BRIDGE_ARGV="$argvlog" SOTD_BIN="$stub" SCRIPT_DIR="$SCRIPTS_DIR" \
        ENDPOINT='pipe:\\.\pipe\sot-testuser-local' SOT_SEND_TIMEOUT=5 \
        sot_oneshot_request "$frame" workspace.list)"
    contains "$out" '"op":"workspace.list"' \
        || { echo "  full-path pipe: endpoint didn't return the stub's matching reply: got '$out'"; return 1; }
    contains "$out" '"stub":true' \
        || { echo "  reply missing the stub marker: $out"; return 1; }
    contains "$(cat "$argvlog" 2>/dev/null)" 'pipe:\\.\pipe\sot-testuser-local' \
        || { echo "  the bridge was not handed the full pipe path: $(cat "$argvlog" 2>/dev/null)"; return 1; }

    # Bare pipe:<name> form must normalise identically.
    rm -f "${argvlog:?}"
    out="$(FAKE_BRIDGE_ARGV="$argvlog" SOTD_BIN="$stub" SCRIPT_DIR="$SCRIPTS_DIR" \
        ENDPOINT='pipe:sot-testuser-local' SOT_SEND_TIMEOUT=5 \
        sot_oneshot_request "$frame" workspace.list)"
    contains "$out" '"op":"workspace.list"' \
        || { echo "  bare pipe: endpoint didn't return the stub's matching reply: got '$out'"; return 1; }
    contains "$(cat "$argvlog" 2>/dev/null)" 'pipe:\\.\pipe\sot-testuser-local' \
        || { echo "  the bare name was not normalised for the bridge: $(cat "$argvlog" 2>/dev/null)"; return 1; }
    return 0
}

case_pipe_endpoint_oneshot_request_fails_cleanly_with_no_sotd() {
    # No sotd anywhere it is looked for: no SOTD_BIN, none on PATH or under
    # HOME, and a pgrep that finds no live one. sot_dial says so, and the
    # request fails CLEANLY: empty stdout, nonzero return, that line on stderr.
    local emptybin frame out rc err
    emptybin="$WORK/no-sotd-bin"
    mkdir -p "$emptybin" "$WORK/no-sotd-home"
    printf '#!/bin/sh\nexit 1\n' > "$emptybin/pgrep"
    chmod +x "$emptybin/pgrep"
    frame='{"v":1,"id":2,"kind":"req","op":"workspace.list","payload":{}}'
    out="$(unset SOTD_BIN; PATH="$emptybin:/usr/bin:/bin" HOME="$WORK/no-sotd-home" SCRIPT_DIR="$SCRIPTS_DIR" \
        ENDPOINT='pipe:sot-testuser-local' SOT_SEND_TIMEOUT=5 \
        sot_oneshot_request "$frame" workspace.list 2>"$WORK/no-sotd.err")"
    rc=$?
    err="$(cat "$WORK/no-sotd.err" 2>/dev/null || true)"
    [ -z "$out" ] || { echo "  expected no reply with no sotd, got: $out"; return 1; }
    [ "$rc" -ne 0 ] || { echo "  expected a nonzero return with no sotd"; return 1; }
    contains "$err" "sot_dial: no sotd to open pipe:" \
        || { echo "  missing sot_dial's diagnostic: $err"; return 1; }
    return 0
}

case_pipe_endpoint_oneshot_request_names_the_bridges_refusal() {
    # The bridge refuses the pipe (as `connect_own` refuses one another
    # account serves): the request fails with no reply, and its stderr
    # carries the bridge's own line rather than a bare timeout.
    local stub frame out rc err
    stub="$WORK/fake-bridge-refusing/sotd"
    mkdir -p "${stub%/*}"
    cat > "$stub" <<'FAKEBRIDGE'
#!/bin/sh
[ "$1" = stdio-bridge ] && [ "$2" = --endpoint ] || exit 97
printf 'sotd stdio-bridge: %s: not connecting: another OS account serves this pipe\n' "$3" >&2
exit 1
FAKEBRIDGE
    chmod +x "$stub"
    frame='{"v":1,"id":2,"kind":"req","op":"workspace.list","payload":{}}'
    out="$(SOTD_BIN="$stub" SCRIPT_DIR="$SCRIPTS_DIR" \
        ENDPOINT='pipe:sot-testuser-local' SOT_SEND_TIMEOUT=5 \
        sot_oneshot_request "$frame" workspace.list 2>"$WORK/bridge-refusal.err")"
    rc=$?
    err="$(cat "$WORK/bridge-refusal.err" 2>/dev/null || true)"
    [ -z "$out" ] || { echo "  expected no reply from a refusing bridge, got: $out"; return 1; }
    [ "$rc" -ne 0 ] || { echo "  expected a nonzero return from a refusing bridge"; return 1; }
    contains "$err" "not connecting: another OS account serves this pipe" \
        || { echo "  the bridge's refusal did not reach the caller: $err"; return 1; }
    return 0
}

case_windows_pipe_discovery_returns_pipe_endpoint_and_skips_pgrep() {
    # sot_daemon_endpoint, on a simulated Windows host (faked via `uname`
    # exactly like the receive-path case above does, since
    # $OS/$OSTYPE are unset here): must ask the local daemon for its pipe
    # FIRST (the same query scripts/sot-local-daemon.ps1 makes), prove it
    # live with a bounded connect probe, and return pipe:<path> -- all
    # before ever reaching for pgrep, which is not on a stock git-bash
    # PATH. A fake sotd.exe answers `session-socket-path local`; the
    # fake sotd.exe's `stdio-bridge` arm answers the connect probe (exit 0); a fake pgrep
    # records whether it was ever invoked at all.
    local fakebin pgreplog appdata out
    fakebin="$WORK/win-discovery-bin"
    mkdir -p "$fakebin"
    cat > "$fakebin/uname" <<'FAKEUNAME'
#!/bin/sh
echo "MINGW64_NT-10.0-19045"
FAKEUNAME
    pgreplog="$WORK/win-discovery-pgrep.log"
    rm -f "${pgreplog:?}"
    cat > "$fakebin/pgrep" <<FAKEPGREP
#!/bin/sh
echo "pgrep called: \$*" >> "$pgreplog"
exit 1
FAKEPGREP
    chmod +x "$fakebin/uname" "$fakebin/pgrep"

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
# The connect probe the library now makes through the bridge, with empty input:
# a live pipe that this account serves.
if [ "$1" = "stdio-bridge" ] && [ "$2" = "--endpoint" ]; then
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
        unset OS OSTYPE SOT_SOCKET SOTD_BIN
        PATH="$fakebin:$PATH"
        LOCALAPPDATA="$appdata"
        sot_relay_endpoint
    )"
    [ "$out" = 'pipe:\\.\pipe\sot-fakeuser-local' ] \
        || { echo "  expected the binary's own no-plan pipe: answer, got: $out"; return 1; }
    return 0
}


# ADR 0049, User isolation: the executable comes from the caller or install, never another process.
case_windows_sotd_exe_is_never_a_listed_process() {
    local dir app other out
    dir="$WORK/win-exe-bin"; app="$WORK/win-exe-app"; other="$WORK/other-sotd.exe"
    mkdir -p "$dir" "$app/sot/bin"
    printf '#!/bin/sh\nexit 0\n' > "$other"
    printf '#!/bin/sh\nexit 0\n' > "$app/sot/bin/sotd.exe"
    printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$other" > "$dir/powershell.exe"
    chmod +x "$other" "$app/sot/bin/sotd.exe" "$dir/powershell.exe"
    out="$(unset SOTD_BIN; OS=Windows_NT PATH="$dir:$PATH" LOCALAPPDATA="$app" _sot_windows_sotd_exe)"
    [ "$out" = "$app/sot/bin/sotd.exe" ] || { echo "  the executable came from a listed process: $out"; return 1; }
    out="$(SOTD_BIN="$other" LOCALAPPDATA="$app" _sot_windows_sotd_exe)"
    [ "$out" = "$other" ] || { echo "  SOTD_BIN was not used: $out"; return 1; }
}
