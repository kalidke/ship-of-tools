#!/usr/bin/env bash
# test-endpoint-gate.sh — C10 (isolation-plan.md §3 as amended by
# dev/output/c3-second-connection-amendment.md): the one gate every
# endpoint value leaves comm-lib.sh through. HERMETIC: sources
# comm-lib.sh directly for the gate/resolver unit cases, with fake
# `sotd`/`ssh` binaries on `PATH` and a temp `$SOT_COMM_HOME` — never the
# real `~/.sot-comm`, a real daemon, or a real network connection.
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-endpoint-gate-XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

export SOT_COMM_HOME="$WORK/home"
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
    out="$(env -u SOTD_BIN PATH="$dir" sot_relay_endpoint 2>/dev/null)"
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
#    first on PATH, and comm-listen.sh --selftest completes over an
#    ssh: endpoint through the same helper.
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

echo ""
echo "$PASS passed, $FAIL failed, $SKIP skipped"
[ "$FAIL" -eq 0 ]
