#!/usr/bin/env bash
# test-endpoint-gate.sh — C10 (isolation-plan.md §3 as amended by
# dev/output/c3-second-connection-amendment.md): the one gate every
# endpoint value leaves comm-lib.sh through. HERMETIC: sources
# comm-lib.sh directly for the gate/resolver unit cases, with fake
# `sotd`/`ssh` binaries on `PATH` and a temp `$SOT_COMM_HOME` — never the
# real `~/.sot-comm`, a real daemon, or a real network connection.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-endpoint-gate-XXXXXX")"
trap 'rm -rf "${WORK:?}"' EXIT

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
# shellcheck source=../scripts/comm-lib.sh
source "$SCRIPTS_DIR/comm-lib.sh"
ensure_home

PASS=0; FAIL=0; SKIP=0
check() {
    local desc="$1" fn="$2"; local rc
    "$fn"
    rc=$?
    case "$rc" in
        0) echo "PASS: $desc"; PASS=$((PASS + 1)) ;;
        2) echo "SKIP: $desc"; SKIP=$((SKIP + 1)) ;;
        *) echo "FAIL: $desc"; FAIL=$((FAIL + 1)) ;;
    esac
}
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# fake_bin_dir — a fresh empty dir this run's caller can drop a stub
# `sotd`/`ssh` into and prepend to $PATH; every case gets its own so one
# case's stub is never visible to another's PATH lookup.
fake_bin_dir() {
    local d
    d="$(mktemp -d "$WORK/bin-XXXXXX")"
    printf '%s\n' "$d"
}

# =========================================================================
# 1. The gate itself (_sot_emit_endpoint) — isolation-plan.md §6's own
#    first C10 row.
# =========================================================================

case_gate_passes_the_dialable_schemes() {
    local out
    out="$(_sot_emit_endpoint "unix:/tmp/x.sock")" || { echo "  unix: refused"; return 1; }
    [ "$out" = "unix:/tmp/x.sock" ] || { echo "  unix: mangled: $out"; return 1; }
    out="$(_sot_emit_endpoint 'pipe:\\.\pipe\sot-local')" || { echo "  pipe: refused"; return 1; }
    [ "$out" = 'pipe:\\.\pipe\sot-local' ] || { echo "  pipe: mangled: $out"; return 1; }
    out="$(_sot_emit_endpoint "ssh:hub")" || { echo "  ssh: (no host) refused"; return 1; }
    [ "$out" = "ssh:hub" ] || { echo "  ssh: (no host) mangled: $out"; return 1; }
    out="$(_sot_emit_endpoint "ssh:hub/gamma")" || { echo "  ssh: (with host) refused"; return 1; }
    [ "$out" = "ssh:hub/gamma" ] || { echo "  ssh: (with host) mangled: $out"; return 1; }
    return 0
}

case_gate_discards_tcp_and_unknown_schemes_with_one_stderr_line() {
    local out rc
    out="$(_sot_emit_endpoint "tcp:127.0.0.1:18743" 2>"$WORK/err1")"
    rc=$?
    { [ -z "$out" ] && [ "$rc" -ne 0 ]; } || { echo "  tcp: not discarded: out=$out rc=$rc"; return 1; }
    contains "$(cat "$WORK/err1")" "tcp:127.0.0.1:18743" \
        || { echo "  no stderr line naming the discarded tcp: value: $(cat "$WORK/err1")"; return 1; }
    [ "$(wc -l < "$WORK/err1")" -eq 1 ] || { echo "  expected exactly one stderr line, got: $(cat "$WORK/err1")"; return 1; }
    out="$(_sot_emit_endpoint "junk:foo" 2>/dev/null)"
    rc=$?
    { [ -z "$out" ] && [ "$rc" -ne 0 ]; } || { echo "  unknown scheme not discarded"; return 1; }
    return 0
}

case_gate_empty_value_is_a_silent_nonzero() {
    local out rc
    out="$(_sot_emit_endpoint "" 2>"$WORK/err2")"
    rc=$?
    { [ -z "$out" ] && [ "$rc" -ne 0 ]; } || { echo "  empty value did not refuse"; return 1; }
    [ ! -s "$WORK/err2" ] || { echo "  empty value must be silent, got: $(cat "$WORK/err2")"; return 1; }
    return 0
}

case_gate_rejects_malformed_ssh_halves() {
    local bad
    for bad in "ssh:-oProxyCommand=x" "ssh:hub/-oProxyCommand=x" "ssh:Hub" "ssh:hub name" "ssh:hub/host;rm -rf"; do
        if _sot_emit_endpoint "$bad" >/dev/null 2>/dev/null; then
            echo "  accepted a bad ssh: value: $bad"; return 1
        fi
    done
    return 0
}

# =========================================================================
# 2. _sot_planned_relay_endpoint / _sot_sotd_bin, and the "scripts newer
#    than the binary" + "pre-upgrade session" cases.
# =========================================================================

case_planned_relay_endpoint_uses_sotd_bin_with_no_live_socket_needed() {
    # _sot_sotd_bin's ladder must not require a live socket (named check
    # 5): a bare executable file is enough.
    local dir
    dir="$(fake_bin_dir)"
    cat > "$dir/sotd" <<'EOF'
#!/bin/sh
[ "$1" = "topology" ] && [ "$2" = "relay-endpoint" ] && { echo "ssh:hub"; exit 0; }
exit 1
EOF
    chmod +x "$dir/sotd"
    local out
    out="$(SOTD_BIN="$dir/sotd" sot_relay_endpoint)"
    [ "$out" = "ssh:hub" ] || { echo "  expected ssh:hub via SOTD_BIN ladder, got: $out"; return 1; }
    return 0
}

case_relay_endpoint_scripts_newer_than_binary_discards_stale_tcp_answer() {
    # An old sotd still answering tcp: (pre-C2) yields no endpoint at all
    # -- discarded by the gate, no other source to fall back to.
    local dir
    dir="$(fake_bin_dir)"
    cat > "$dir/sotd" <<'EOF'
#!/bin/sh
[ "$1" = "topology" ] && [ "$2" = "relay-endpoint" ] && { echo "tcp:127.0.0.1:18743"; exit 0; }
exit 1
EOF
    chmod +x "$dir/sotd"
    local out rc
    out="$(SOTD_BIN="$dir/sotd" sot_relay_endpoint 2>/dev/null)"
    rc=$?
    { [ -z "$out" ] && [ "$rc" -ne 0 ]; } || { echo "  expected no endpoint from a stale tcp: answer, got: $out"; return 1; }
    return 0
}

case_relay_endpoint_pre_upgrade_stale_env_value_is_discarded_not_used() {
    # A session holding a stale SOT_RELAY_ENDPOINT=tcp:... from before the
    # upgrade: the explicit arm refuses it (a miss, not a death, for THIS
    # resolver) and the planned endpoint is used instead.
    local dir
    dir="$(fake_bin_dir)"
    cat > "$dir/sotd" <<'EOF'
#!/bin/sh
[ "$1" = "topology" ] && [ "$2" = "relay-endpoint" ] && { echo "ssh:hub"; exit 0; }
exit 1
EOF
    chmod +x "$dir/sotd"
    local out
    out="$(SOTD_BIN="$dir/sotd" sot_relay_endpoint "tcp:127.0.0.1:18743" 2>/dev/null)"
    [ "$out" = "ssh:hub" ] || { echo "  expected the stale value discarded and the planned answer used, got: $out"; return 1; }
    return 0
}

# =========================================================================
# 3. The daemonless server box — all three answers the binary can give.
# =========================================================================

case_daemonless_box_no_sotd_on_path_is_no_endpoint() {
    local dir out rc
    dir="$(fake_bin_dir)"
    out="$(env -u SOTD_BIN PATH="$_GUARD_STUBS:$dir" sot_relay_endpoint 2>/dev/null)"
    rc=$?
    { [ -z "$out" ] && [ "$rc" -ne 0 ]; } || { echo "  expected no endpoint with no sotd at all, got: $out"; return 1; }
    return 0
}

case_daemonless_box_no_file_answers_its_own_socket() {
    local dir
    dir="$(fake_bin_dir)"
    cat > "$dir/sotd" <<'EOF'
#!/bin/sh
[ "$1" = "topology" ] && [ "$2" = "relay-endpoint" ] && { echo "unix:/run/user/1000/sot/sessions/sot.sock"; exit 0; }
exit 1
EOF
    chmod +x "$dir/sotd"
    local out
    out="$(SOTD_BIN="$dir/sotd" sot_relay_endpoint)"
    [ "$out" = "unix:/run/user/1000/sot/sessions/sot.sock" ] \
        || { echo "  expected the no-file own-socket answer returned verbatim, got: $out"; return 1; }
    return 0
}

case_daemonless_box_not_a_listed_host_is_no_endpoint_with_the_binarys_own_line() {
    local dir out rc
    dir="$(fake_bin_dir)"
    cat > "$dir/sotd" <<'EOF'
#!/bin/sh
if [ "$1" = "topology" ] && [ "$2" = "relay-endpoint" ]; then
    echo "sotd: 'this-box' is not a listed host" >&2
    exit 1
fi
exit 1
EOF
    chmod +x "$dir/sotd"
    out="$(SOTD_BIN="$dir/sotd" sot_relay_endpoint 2>"$WORK/err3")"
    rc=$?
    { [ -z "$out" ] && [ "$rc" -ne 0 ]; } || { echo "  expected no endpoint for an unlisted host, got: $out"; return 1; }
    contains "$(cat "$WORK/err3")" "not a listed host" \
        || { echo "  the binary's own 'not a listed host' line must pass through, got: $(cat "$WORK/err3")"; return 1; }
    return 0
}

# =========================================================================
# 4. sot_daemon_endpoint: a refused EXPLICIT endpoint is fatal, never a
#    substituted local daemon.
# =========================================================================

case_daemon_endpoint_explicit_refused_is_fatal_and_names_the_variable() {
    local out rc
    out="$(sot_daemon_endpoint "tcp:127.0.0.1:18743" 2>"$WORK/err4")"
    rc=$?
    { [ -z "$out" ] && [ "$rc" -ne 0 ]; } || { echo "  a refused explicit endpoint must be fatal, got: $out"; return 1; }
    contains "$(cat "$WORK/err4")" "refusing to substitute a local daemon" \
        || { echo "  missing the 'refusing to substitute' line: $(cat "$WORK/err4")"; return 1; }
    return 0
}

case_daemon_endpoint_explicit_good_value_passes_through() {
    local out
    out="$(sot_daemon_endpoint "unix:/tmp/explicit.sock")"
    [ "$out" = "unix:/tmp/explicit.sock" ] || { echo "  expected verbatim pass-through, got: $out"; return 1; }
    return 0
}

case_relay_endpoint_explicit_refused_is_a_miss_not_a_death() {
    # The mirror of the fatal case above: sot_relay_endpoint's explicit
    # arm continues to its next source on a refusal (main's ruling).
    local dir
    dir="$(fake_bin_dir)"
    cat > "$dir/sotd" <<'EOF'
#!/bin/sh
[ "$1" = "topology" ] && [ "$2" = "relay-endpoint" ] && { echo "ssh:hub"; exit 0; }
exit 1
EOF
    chmod +x "$dir/sotd"
    local out
    out="$(SOTD_BIN="$dir/sotd" sot_relay_endpoint "tcp:stale:1" 2>/dev/null)"
    [ "$out" = "ssh:hub" ] || { echo "  expected the refusal to fall through to the planned endpoint, got: $out"; return 1; }
    return 0
}

# =========================================================================
# 5. The wire: sot_ssh_bridge reaches a stub daemon through a stub ssh
#    first on PATH.
# =========================================================================

case_ssh_bridge_carries_the_frame_to_a_stub_daemon_and_back() {
    local dir
    dir="$(fake_bin_dir)"
    # The stub `ssh` ignores every option/target argv (ssh_bridge.rs's own
    # unit tests already pin the argv shape) and instead runs a tiny
    # in-process "daemon": echo the hello it's handed back with ok:true,
    # then echo one canned agent.send ack.
    cat > "$dir/ssh" <<'EOF'
#!/bin/sh
while IFS= read -r line; do
    case "$line" in
        *'"op":"hello"'*) printf '{"v":1,"id":1,"kind":"res","op":"hello","payload":{"ok":true}}\n' ;;
        *'"op":"agent.send"'*) printf '{"v":1,"id":1,"kind":"res","op":"agent.send","payload":{"ok":true,"receivers":[]}}\n' ;;
    esac
done
EOF
    chmod +x "$dir/ssh"
    local out
    out="$(
        unset XDG_RUNTIME_DIR
        PATH="$dir:$PATH"
        sot_ssh_bridge hub <<'FRAME'
{"v":1,"id":1,"kind":"req","op":"hello","payload":{}}
{"v":1,"id":2,"kind":"req","op":"agent.send","payload":{}}
FRAME
    )"
    contains "$out" '"op":"hello"' || { echo "  missing hello reply: $out"; return 1; }
    contains "$out" '"op":"agent.send"' || { echo "  missing agent.send reply: $out"; return 1; }
    return 0
}

case_ssh_bridge_dying_child_yields_no_reply() {
    local dir
    dir="$(fake_bin_dir)"
    cat > "$dir/ssh" <<'EOF'
#!/bin/sh
echo "Permission denied (publickey)." >&2
exit 255
EOF
    chmod +x "$dir/ssh"
    local out rc
    out="$(
        unset XDG_RUNTIME_DIR
        PATH="$dir:$PATH"
        sot_ssh_bridge hub </dev/null 2>/dev/null
    )"
    rc=$?
    { [ -z "$out" ] && [ "$rc" -ne 0 ]; } || { echo "  expected empty output and a nonzero exit from a dying child, got out=$out rc=$rc"; return 1; }
    return 0
}

# =========================================================================
# 6. BLOCKER 1 (S1 fix round): the bound moved INTO sot_ssh_bridge as a
#    third positional parameter -- a caller passes it there now, never
#    wraps the call in `timeout` (which never saw a shell function, even
#    exported: `timeout 1 f` on an exported function is 127, no output).
#    comm-lib.sh's sot_oneshot_request (call site 1) is sourced right
#    here, so it is driven directly rather than through a subprocess.
# =========================================================================

case_ssh_bridge_third_arg_bounds_it_and_an_empty_one_stays_unbounded() {
    local dir
    dir="$(fake_bin_dir)"
    cat > "$dir/ssh" <<'EOF'
#!/bin/sh
sleep 5
EOF
    chmod +x "$dir/ssh"
    local out rc start end
    start="$(date +%s)"
    out="$(
        unset XDG_RUNTIME_DIR
        PATH="$dir:$PATH"
        sot_ssh_bridge hub "" 1 </dev/null 2>/dev/null
    )"
    rc=$?
    end="$(date +%s)"
    { [ -z "$out" ] && [ "$rc" -eq 124 ]; } || { echo "  a 1s bound on a 5s-sleeping child gave out='$out' rc=$rc, want empty/124"; return 1; }
    [ "$((end - start))" -le 3 ] || { echo "  took $((end - start))s to time out at 1s -- the bound did not apply"; return 1; }
    return 0
}

case_oneshot_request_ssh_arm_reaches_a_stub_daemon_and_back() {
    local dir
    dir="$(fake_bin_dir)"
    cat > "$dir/ssh" <<'EOF'
#!/bin/sh
while IFS= read -r line; do
    case "$line" in
        *'"op":"hello"'*) printf '{"v":1,"id":1,"kind":"res","op":"hello","payload":{"ok":true}}\n' ;;
        *'"op":"workspace.list"'*) printf '{"v":1,"id":1,"kind":"res","op":"workspace.list","payload":{"rows":[]}}\n' ;;
    esac
done
EOF
    chmod +x "$dir/ssh"
    local out
    out="$(
        unset XDG_RUNTIME_DIR
        PATH="$dir:$PATH"
        ENDPOINT="ssh:hub" sot_oneshot_request \
            '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list
    )"
    contains "$out" '"op":"workspace.list"' \
        || { echo "  expected the stub daemon's reply, got: '$out' (the bridge never reached it)"; return 1; }
    return 0
}

case_oneshot_request_ssh_arm_dying_child_is_silent_but_diagnosed() {
    local dir
    dir="$(fake_bin_dir)"
    cat > "$dir/ssh" <<'EOF'
#!/bin/sh
echo "ssh: connect to host hub port 22: Connection refused" >&2
exit 255
EOF
    chmod +x "$dir/ssh"
    local out err rc
    out="$(
        unset XDG_RUNTIME_DIR
        PATH="$dir:$PATH"
        ENDPOINT="ssh:hub" SOT_SEND_TIMEOUT=3 sot_oneshot_request \
            '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list 2>"$WORK/oneshot.err"
    )"
    rc=$?
    err="$(cat "$WORK/oneshot.err" 2>/dev/null)"
    # A status-only assertion here would also have passed pre-fix (that
    # code died at 127 before ever touching the stub, also nonzero, also
    # empty stdout) -- the diagnostic naming the real reason is what a
    # caller now sees on comm-lib.sh's own stderr instead of /dev/null.
    { [ -z "$out" ] && [ "$rc" -ne 0 ]; } || { echo "  a dying bridge gave out='$out' rc=$rc, want empty/nonzero"; return 1; }
    contains "$err" "Connection refused" \
        || { echo "  the child's own stderr never reached ours: '$err'"; return 1; }
    return 0
}

check "the gate passes unix:/pipe:/ssh: verbatim" case_gate_passes_the_dialable_schemes
check "the gate discards tcp: and an unknown scheme, with exactly one stderr line" case_gate_discards_tcp_and_unknown_schemes_with_one_stderr_line
check "the gate treats an empty value as a silent nonzero" case_gate_empty_value_is_a_silent_nonzero
check "the gate rejects an ssh: value whose target or host is not a plain host name" case_gate_rejects_malformed_ssh_halves
check "_sot_planned_relay_endpoint reaches sotd via SOTD_BIN with no live socket required" case_planned_relay_endpoint_uses_sotd_bin_with_no_live_socket_needed
check "sot_relay_endpoint discards a scripts-newer-than-binary stale tcp: answer" case_relay_endpoint_scripts_newer_than_binary_discards_stale_tcp_answer
check "sot_relay_endpoint discards a pre-upgrade stale env value and uses the planned answer" case_relay_endpoint_pre_upgrade_stale_env_value_is_discarded_not_used
check "a daemonless box with no sotd on PATH resolves no endpoint" case_daemonless_box_no_sotd_on_path_is_no_endpoint
check "a daemonless box with no hosts.toml file gets its own socket back verbatim" case_daemonless_box_no_file_answers_its_own_socket
check "a box not listed in an existing hosts.toml resolves no endpoint, with the binary's own line" case_daemonless_box_not_a_listed_host_is_no_endpoint_with_the_binarys_own_line
check "sot_daemon_endpoint's explicit arm is fatal on a refused value and names the variable" case_daemon_endpoint_explicit_refused_is_fatal_and_names_the_variable
check "sot_daemon_endpoint's explicit arm passes a good value through verbatim" case_daemon_endpoint_explicit_good_value_passes_through
check "sot_relay_endpoint's explicit arm treats a refusal as a miss, not a death" case_relay_endpoint_explicit_refused_is_a_miss_not_a_death
check "sot_ssh_bridge carries a frame to a stub daemon over a stub ssh and back" case_ssh_bridge_carries_the_frame_to_a_stub_daemon_and_back
check "a dying stub ssh yields no reply, never a hang" case_ssh_bridge_dying_child_yields_no_reply
check "sot_ssh_bridge's third arg bounds the call, and an empty one stays unbounded" case_ssh_bridge_third_arg_bounds_it_and_an_empty_one_stays_unbounded
check "sot_oneshot_request's ssh: arm (call site 1) reaches a stub daemon and back" case_oneshot_request_ssh_arm_reaches_a_stub_daemon_and_back
check "sot_oneshot_request's ssh: arm folds a dying child's stderr into its own diagnostic" case_oneshot_request_ssh_arm_dying_child_is_silent_but_diagnosed

# =========================================================================
# 7. BLOCKER 3: a source-built box (no release install, SOTD_BIN unset,
#    neither ~/.local path present) resolves its relay endpoint through
#    the LIVE process, not just a PATH lookup -- `_sot_sotd_bin`'s last
#    candidate reads /proc/<pid>/exe. A shell script's own /proc/pid/exe
#    resolves to its INTERPRETER, not to the script (the kernel loads the
#    shebang target), so a real ELF is compiled here -- the smallest thing
#    that runs directly, answers `topology relay-endpoint` on its own
#    argv, and otherwise just sleeps so `pgrep` finds it alive.
# =========================================================================

case_source_built_box_resolves_relay_endpoint_via_proc_exe() {
    command -v cc >/dev/null 2>&1 || { echo "  no cc on PATH; cannot build the live-process stub"; return 2; }
    local dir src
    dir="$(mktemp -d "$WORK/proc-stub-XXXXXX")"
    src="$dir/stub.c"
    cat > "$src" <<'EOF'
#include <string.h>
#include <stdio.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc >= 3 && strcmp(argv[1], "topology") == 0 && strcmp(argv[2], "relay-endpoint") == 0) {
        printf("ssh:hub-from-proc\n");
        return 0;
    }
    sleep(300);
    return 0;
}
EOF
    cc -O0 -o "$dir/sotd" "$src" 2>"$WORK/cc.err" || { echo "  cc failed: $(cat "$WORK/cc.err")"; return 2; }

    # A live "daemon": no args, so it sleeps -- exactly like a real sotd
    # sitting on its socket. Named check: NEVER on this test's own PATH
    # and NEVER at either ~/.local fallback -- only pgrep/proc can find it.
    "$dir/sotd" &
    local pid=$!
    trap 'kill '"$pid"' 2>/dev/null; wait '"$pid"' 2>/dev/null' RETURN
    local tries=0
    while [ ! -r "/proc/$pid/exe" ] && [ "$tries" -lt 50 ]; do sleep 0.05; tries=$((tries + 1)); done
    [ -r "/proc/$pid/exe" ] || { echo "  stub never came up (no /proc/$pid/exe)"; return 1; }

    # A DECOY first, then the real stub: round-2 item 2's own regression
    # guard. `_sot_sotd_bin`'s /proc loop used to take the FIRST match
    # `pgrep -af 'sotd'` turned up and return it merely because
    # `/proc/<pid>/exe` was readable and executable -- true of ANY live
    # process, never proof it IS the daemon. A `sleep` sitting in the
    # background matches that pgrep pattern just as well as a real
    # `journalctl -fu sotd` or `tail -f .../sotd.log` would (both have
    # "sotd" somewhere on their command line), and its own /proc/pid/exe
    # resolves to the real `sleep` binary -- executable, and WRONG. The
    # fixed loop must skip it by basename and keep going.
    "sleep" 300 &
    local decoy_pid=$!
    local decoy_tries=0
    while [ ! -r "/proc/$decoy_pid/exe" ] && [ "$decoy_tries" -lt 50 ]; do sleep 0.05; decoy_tries=$((decoy_tries + 1)); done
    [ -r "/proc/$decoy_pid/exe" ] || { echo "  decoy never came up (no /proc/$decoy_pid/exe)"; kill "$decoy_pid" 2>/dev/null; wait "$decoy_pid" 2>/dev/null; return 2; }

    # A fake `pgrep` naming the decoy FIRST, then this stub's own pid --
    # this box (the one actually running this test) is not hermetic
    # against a REAL `sotd` elsewhere in its own process table, and the
    # real one's lower pid would sort first and win the loop before ever
    # reaching ours. Real `pgrep` is still what production runs; this is
    # the same seam test-join-disambiguation.sh already fakes `pgrep`
    # through.
    local fakebin
    fakebin="$(mktemp -d "$WORK/proc-stub-fakebin-XXXXXX")"
    printf '#!/bin/sh\necho "%s sleep 300"\necho "%s %s"\n' "$decoy_pid" "$pid" "$dir/sotd" > "$fakebin/pgrep"
    chmod +x "$fakebin/pgrep"
    # The guard's other refusing stubs stay on this rebuilt PATH, all but sotd:
    # the case's premise is that `command -v sotd` finds nothing.
    local gs; for gs in sotd.exe powershell.exe pwsh pwsh.exe; do ln -s "$_GUARD_STUBS/$gs" "$fakebin/$gs"; done
    trap 'kill '"$decoy_pid"' 2>/dev/null; wait '"$decoy_pid"' 2>/dev/null; kill '"$pid"' 2>/dev/null; wait '"$pid"' 2>/dev/null' RETURN

    local fakehome out
    fakehome="$(mktemp -d "$WORK/proc-stub-home-XXXXXX")"
    # A plain assignment prefix, not `env` (an external command that
    # `execve`s its argument and, same class as BLOCKER 1, would never see
    # a shell function): sets PATH/HOME for this one sourced-function call
    # only.
    out="$(
        unset SOTD_BIN
        PATH="$fakebin:/usr/bin:/bin" HOME="$fakehome" sot_relay_endpoint 2>/dev/null
    )"
    [ "$out" = "ssh:hub-from-proc" ] \
        || { echo "  expected the stub's own answer 'ssh:hub-from-proc', got: '$out'"; return 1; }
    return 0
}
check "a source-built box (no release install) resolves its relay endpoint via a live process's /proc/pid/exe" case_source_built_box_resolves_relay_endpoint_via_proc_exe

# =========================================================================
# 8. E1: a `sotd --socket` in another process's argv is never an endpoint. The
#    last-resort scrape of a development daemon's argv could hand a line to a
#    stranger's or a test daemon, which would then answer for this comm folder.
# =========================================================================
case_an_argv_socket_of_another_process_is_never_an_endpoint() {
    local fakebin fakehome out rc=0
    fakebin="$(mktemp -d "$WORK/scrape-fakebin-XXXXXX")"
    fakehome="$(mktemp -d "$WORK/scrape-home-XXXXXX")"
    printf '#!/bin/sh\necho "4242 sotd --socket /tmp/m4-scrape.sock"\n' > "$fakebin/pgrep"
    chmod +x "$fakebin/pgrep"
    out="$(
        unset SOTD_BIN SOT_SOCKET
        PATH="$fakebin:$PATH" HOME="$fakehome" sot_daemon_endpoint 2>/dev/null
    )" || rc=$?
    [ -z "$out" ] && [ "$rc" -ne 0 ] || { echo "  rc $rc, endpoint '$out': a scraped argv socket was used"; return 1; }
    return 0
}
check "no sotd --socket in another process's argv is ever an endpoint" case_an_argv_socket_of_another_process_is_never_an_endpoint

# E2: pgrep matches any command line that mentions sotd. A process is asked for
# a daemon socket (run, as its own binary) only when that binary is named sotd.
case_a_process_is_asked_for_a_socket_only_when_its_binary_is_named_sotd() {
    [ "$(uname -s)" = Linux ] && [ -r /proc/self/exe ] || { echo "  needs /proc"; return 2; }
    local spy="$WORK/spy" fakebin fakehome pida pidb out rc=0 ran
    mkdir -p "$spy"
    cp "$(command -v bash)" "$spy/spybash"; cp "$(command -v bash)" "$spy/sotd"
    printf '%s\n' 'printf "%s\n" "$BASH" >> "$(dirname "$0")/ran"' > "$spy/session-socket-path"
    "$spy/spybash" -c 'sleep 30; :' & pida=$!
    "$spy/sotd" -c 'sleep 30; :' & pidb=$!
    local tries=0
    while { [ ! -r "/proc/$pida/exe" ] || [ ! -r "/proc/$pidb/exe" ]; } && [ "$tries" -lt 50 ]; do sleep 0.05; tries=$((tries + 1)); done
    fakebin="$(mktemp -d "$WORK/spy-fakebin-XXXXXX")"
    fakehome="$(mktemp -d "$WORK/spy-home-XXXXXX")"
    printf '#!/bin/sh\necho "%s spybash -c sleep 30 sotd"\necho "%s sotd -c sleep 30"\n' "$pida" "$pidb" > "$fakebin/pgrep"
    chmod +x "$fakebin/pgrep"
    out="$(
        cd "$spy" && unset SOT_SOCKET SOTD_BIN
        PATH="$fakebin:$PATH" HOME="$fakehome" sot_daemon_endpoint 2>/dev/null
    )" || rc=$?
    kill "$pida" "$pidb" 2>/dev/null; wait "$pida" "$pidb" 2>/dev/null
    ran="$(cat "$spy/ran" 2>/dev/null)"
    [ "$rc" -ne 0 ] && [ -z "$out" ] || { echo "  rc $rc, endpoint '$out'"; return 1; }
    contains "$ran" "$spy/sotd" || { echo "  the sotd-named binary was never asked: '$ran'"; return 1; }
    contains "$ran" "$spy/spybash" && { echo "  a process not named sotd was run: '$ran'"; return 1; }
    return 0
}
check "a process is asked for a socket only when its binary is named sotd" case_a_process_is_asked_for_a_socket_only_when_its_binary_is_named_sotd

echo ""
echo "$PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
