# test-join-disambiguation.sh part: send, relay and bootstrap refusals; the legacy root row (sourced in order by the entry).

case_comm_relay_send_refuses_with_no_identity() {
    # Caller-audit follow-up (ruling 5): comm-send.sh's identity refusal is
    # covered above, but comm-relay.sh's OWN refusal (send_frame — a
    # SEPARATE code path) was never
    # exercised. comm-relay.sh checks the identity before it resolves any
    # endpoint, so no daemon and no endpoint are needed to prove this refusal.
    local self scratch out err rc errfile
    next_self_file; self="$NEXT_SELF_FILE"   # never created -> no identity
    scratch="$(realpath "$WORK")"
    errfile="$WORK/relay-refusal.err"
    out="$(cd "$scratch" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        "$SCRIPTS_DIR/comm-relay.sh" send @somebody "hello" 2>"$errfile")"
    rc=$?
    err="$(cat "$errfile" 2>/dev/null || true)"
    [ "$rc" -ne 0 ] || { echo "  comm-relay.sh send succeeded with no identity: $out"; return 1; }
    contains "$err" "identity did not resolve" || { echo "  missing identity-refusal message: $err"; return 1; }
    return 0
}

case_comm_bootstrap_refuses_with_no_identity() {
    # Same caller-audit gap for comm-bootstrap.sh: its NAME check runs
    # before any daemon or workspace lookup, so a bogus target is enough to
    # exercise the refusal without a daemon or a peer session.
    local self scratch out err rc errfile
    next_self_file; self="$NEXT_SELF_FILE"
    scratch="$(realpath "$WORK")"
    errfile="$WORK/bootstrap-refusal.err"
    out="$(cd "$scratch" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        "$SCRIPTS_DIR/comm-bootstrap.sh" "nonexistent-row" 2>"$errfile")"
    rc=$?
    err="$(cat "$errfile" 2>/dev/null || true)"
    [ "$rc" -ne 0 ] || { echo "  comm-bootstrap.sh succeeded with no identity: $out"; return 1; }
    contains "$err" "identity did not resolve" || { echo "  missing identity-refusal message: $err"; return 1; }
    return 0
}

case_comm_bootstrap_refuses_a_bash_row() {
    # A row whose agent is none runs a shell: nothing there can join, and the
    # nudge would be typed to the shell as a command line. Refused after the
    # row is found, and nothing is typed.
    local root self errfile out rc err req
    mkdir -p "$WORK/bootstrap-bash/sender41"
    root="$(realpath "$WORK/bootstrap-bash/sender41")"
    join_in "$root"
    [ "$JOIN_RC" -eq 0 ] || { echo "  setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    self="$NEXT_SELF_FILE"
    PRE_CREATE_LIST="$(jq -nc --arg root "$WORK/bootstrap-bash" \
        '[{workspace_id:"ws-bash-41",slug:"bash41",label:"bash41",project_root:$root,is_default:false,agent:"none",agent_name:"",agent_handle:"",runtime:"capsule",phase:"ready"}]')"
    start_stub_daemon "ws-bash-41" "bash41" "$WORK/bootstrap-bash"
    errfile="$WORK/bootstrap-bash.err"
    out="$(cd "$root" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        SOT_SPAWN_ENDPOINT="unix:$STUB_SOCK" "$SCRIPTS_DIR/comm-bootstrap.sh" bash41 2>"$errfile")"
    rc=$?
    err="$(cat "$errfile" 2>/dev/null || true)"
    req="$(grep -m1 '"op":"pty\.' "$STUB_REQLOG" 2>/dev/null || true)"
    stop_stub_daemon
    PRE_CREATE_LIST=""
    [ "$rc" -eq 1 ] || { echo "  exited $rc (want 1): $out $err"; return 1; }
    contains "$err" "is a bash row (agent none)" || { echo "  stderr: $err"; return 1; }
    [ -z "$req" ] || { echo "  comm-bootstrap.sh typed into a bash row: $req"; return 1; }
    return 0
}

case_comm_bootstrap_types_into_an_agent_row() {
    # The other side of the bash-row refusal: a claude row, a codex row and a
    # row whose list entry has no agent field are each typed into, once.
    local root self errfile out rc err kind n
    mkdir -p "$WORK/bootstrap-agent/sender42"
    root="$(realpath "$WORK/bootstrap-agent/sender42")"
    join_in "$root"
    [ "$JOIN_RC" -eq 0 ] || { echo "  setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    self="$NEXT_SELF_FILE"
    for kind in claude codex absent; do
        PRE_CREATE_LIST="$(jq -nc --arg root "$WORK/bootstrap-agent" --arg k "$kind" \
            '[{workspace_id:"ws-agent-42",slug:"agent42",label:"agent42",project_root:$root,is_default:false,agent:$k,agent_name:"",agent_handle:"",runtime:"capsule",phase:"ready"} | if $k == "absent" then del(.agent) else . end]')"
        start_stub_daemon "ws-agent-42" "agent42" "$WORK/bootstrap-agent"
        errfile="$WORK/bootstrap-agent.err"
        out="$(cd "$root" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
            SOT_SPAWN_ENDPOINT="unix:$STUB_SOCK" "$SCRIPTS_DIR/comm-bootstrap.sh" agent42 2>"$errfile")"
        rc=$?
        err="$(cat "$errfile" 2>/dev/null || true)"
        n="$(grep -c '"op":"pty\.input"' "$STUB_REQLOG" 2>/dev/null || true)"
        stop_stub_daemon
        PRE_CREATE_LIST=""
        [ "$rc" -eq 0 ] || { echo "  $kind: exited $rc (want 0): $out $err"; return 1; }
        [ "$n" = 1 ] || { echo "  $kind: $n pty.input requests (want 1)"; return 1; }
    done
    return 0
}

case_send_files_and_types_nothing() {
    # A send is its inbox append and nothing else: the daemon wakes the row,
    # so comm-send.sh sends no pty.input and its line carries no wake verdict,
    # with a daemon reachable or not.
    local root_sender root_recipient h_sender h_recipient self_sender errfile out rc err
    mkdir -p "$WORK/send-live/sender31" "$WORK/send-live/recipient31"
    root_sender="$(realpath "$WORK/send-live/sender31")"
    root_recipient="$(realpath "$WORK/send-live/recipient31")"

    SOT_WORKSPACE_ID="ws-live-31" join_in "$root_recipient"
    [ "$JOIN_RC" -eq 0 ] || { echo "  recipient setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    h_recipient="recipient31-${HOST}"
    [ "$(registry_field "$h_recipient" workspace_id)" = "ws-live-31" ] \
        || { echo "  join did not record the workspace id: $(registry_field "$h_recipient" workspace_id)"; return 1; }

    join_in "$root_sender"
    [ "$JOIN_RC" -eq 0 ] || { echo "  sender setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    h_sender="sender31-${HOST}"; self_sender="$NEXT_SELF_FILE"

    start_stub_daemon "ws-live-31" "recipient31" "$root_recipient" "$h_recipient"
    errfile="$WORK/send-live.err"
    out="$(cd "$root_sender" && SOT_COMM_SELF_FILE="$self_sender" SOT_COMM_TEST_HOST="$HOST" \
        SOT_SOCKET="$STUB_SOCK" "$SEND" "@$h_recipient" "hello live" 2>"$errfile")"
    rc=$?
    err="$(cat "$errfile" 2>/dev/null || true)"
    local req; req="$(grep -m1 '"op":"pty\.' "$STUB_REQLOG" 2>/dev/null || true)"
    stop_stub_daemon
    [ "$rc" -eq 0 ] || { echo "  comm-send.sh failed: rc=$rc, stderr: $err"; return 1; }
    contains "$out" "filed -> @$h_recipient" || { echo "  stdout: $out (want 'filed -> @$h_recipient')"; return 1; }
    contains "$out" "woken" && { echo "  stdout: $out (a send carries no wake verdict)"; return 1; }
    [ -z "$req" ] || { echo "  comm-send.sh typed into a row: $req"; return 1; }

    # No daemon: filed all the same, never an error.
    out="$(cd "$root_sender" && SOT_COMM_SELF_FILE="$self_sender" SOT_COMM_TEST_HOST="$HOST" \
        SOT_SOCKET="$WORK/no-daemon.sock" "$SEND" "@$h_recipient" "hello queued" 2>"$errfile")"
    rc=$?
    [ "$rc" -eq 0 ] || { echo "  comm-send.sh failed with no daemon: rc=$rc, stderr: $(cat "$errfile")"; return 1; }
    # The ack is the FILE: filed with a daemon or without one.
    contains "$out" "filed -> @$h_recipient" || { echo "  stdout: $out (want 'filed -> @$h_recipient' with no daemon)"; return 1; }
    [ "$(jq -r 'select(.msg == "hello queued") | .from' "$INBOX_DIR/$h_recipient.jsonl")" = "$h_sender" ] \
        || { echo "  recipient inbox missing the queued message"; return 1; }
    return 0
}

case_relay_send_fails_loudly_with_no_reachable_daemon() {
    # Codex review round-3 finding 3: an EMPTY response from nc_send used
    # to pass `jq -e` — zero JSON inputs means jq never sees a falsy last
    # value to react to, so the check silently succeeded. A missing/
    # unreachable Unix socket used to print "relayed" and exit 0.
    local root h self errfile out rc err
    mkdir -p "$WORK/relay-noack/proj20"
    root="$(realpath "$WORK/relay-noack/proj20")"
    join_in "$root"
    [ "$JOIN_RC" -eq 0 ] || { echo "  setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    h="proj20-${HOST}"; self="$NEXT_SELF_FILE"
    contains "$JOIN_OUT" "Joined sot-comm as @$h" || { echo "  setup join stdout: $JOIN_OUT"; return 1; }

    errfile="$WORK/relay-noack.err"
    # This box's relay endpoint, a stub `sotd`'s answer to `topology relay-endpoint`, is a socket nothing listens on.
    printf '#!/bin/sh\n[ "$1 $2" = "topology relay-endpoint" ] && { echo "unix:%s/no-such-daemon-anywhere.sock"; exit 0; }\nexit 97\n' \
        "$WORK" > "$WORK/relay-noack/sotd"; chmod +x "$WORK/relay-noack/sotd"
    out="$(cd "$root" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" SOTD_BIN="$WORK/relay-noack/sotd" \
        "$SCRIPTS_DIR/comm-relay.sh" send @somebody "hello" 2>"$errfile")"
    rc=$?
    err="$(cat "$errfile" 2>/dev/null || true)"
    [ "$rc" -ne 0 ] || { echo "  comm-relay.sh send succeeded with no reachable daemon: $out"; return 1; }
    contains "$out" "relayed ->" && { echo "  claimed 'relayed' despite no reachable daemon: $out"; return 1; }
    contains "$err" "FAILED -> @somebody: the daemon did not answer at unix:$WORK/no-such-daemon-anywhere.sock" \
        || { echo "  missing the no-answer FAILED line: $err"; return 1; }
    return 0
}

case_send_succeeds_with_rooted_registry_row() {
    # Codex review round-3 finding 8: the suite proved only REFUSALS for
    # comm-send.sh's identity gate; this proves the actual happy path — a
    # real join followed by an ordinary send to another real, registered
    # recipient must succeed end-to-end (delivered, and landed in the
    # recipient's own inbox with the right from-field).
    local root_sender root_recipient h_sender h_recipient self_sender errfile out rc err
    mkdir -p "$WORK/send-happy-path/sender23" "$WORK/send-happy-path/recipient23"
    root_sender="$(realpath "$WORK/send-happy-path/sender23")"
    root_recipient="$(realpath "$WORK/send-happy-path/recipient23")"

    join_in "$root_recipient"
    [ "$JOIN_RC" -eq 0 ] || { echo "  recipient setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    h_recipient="recipient23-${HOST}"
    contains "$JOIN_OUT" "Joined sot-comm as @$h_recipient" || { echo "  recipient setup join stdout: $JOIN_OUT"; return 1; }

    join_in "$root_sender"
    [ "$JOIN_RC" -eq 0 ] || { echo "  sender setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    h_sender="sender23-${HOST}"; self_sender="$NEXT_SELF_FILE"
    contains "$JOIN_OUT" "Joined sot-comm as @$h_sender" || { echo "  sender setup join stdout: $JOIN_OUT"; return 1; }

    errfile="$WORK/send-happy-path.err"
    out="$(cd "$root_sender" && SOT_COMM_SELF_FILE="$self_sender" SOT_COMM_TEST_HOST="$HOST" \
        "$SEND" "@$h_recipient" "hello there" 2>"$errfile")"
    rc=$?
    err="$(cat "$errfile" 2>/dev/null || true)"
    [ "$rc" -eq 0 ] || { echo "  comm-send.sh failed with two genuinely rooted, registered identities: rc=$rc, stderr: $err"; return 1; }
    contains "$err" "identity did not resolve" && { echo "  refused despite a valid rooted registry row: $err"; return 1; }
    contains "$out" "filed -> @$h_recipient" \
        || { echo "  stdout doesn't confirm delivery: $out"; return 1; }
    jq -e --arg h "$h_sender" 'select(.from == $h)' "$INBOX_DIR/$h_recipient.jsonl" >/dev/null 2>&1 \
        || { echo "  recipient inbox missing a message from @$h_sender: $(cat "$INBOX_DIR/$h_recipient.jsonl" 2>/dev/null)"; return 1; }
    return 0
}

case_send_refuses_when_registry_row_missing_despite_resolved_name() {
    # Codex review round-2 finding 4/C: a self-file resolving NAME locally
    # (comm-context.sh validated its root=) is NOT sufficient — the
    # registry must ALSO have a row for it, or sending stamps a from-handle
    # nothing can route a reply to. Simulates an evicted/never-persisted
    # row: join normally (valid v2 self-file + a real registry row), then
    # delete JUST the registry row, leaving the self-file believing it's
    # still joined.
    local root h self
    mkdir -p "$WORK/routable-missing-row/proj15"
    root="$(realpath "$WORK/routable-missing-row/proj15")"
    join_in "$root"
    [ "$JOIN_RC" -eq 0 ] || { echo "  setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    h="proj15-${HOST}"; self="$NEXT_SELF_FILE"
    contains "$JOIN_OUT" "Joined sot-comm as @$h" || { echo "  setup join stdout: $JOIN_OUT"; return 1; }

    with_lock registry_del "$h"

    local send_out send_err send_rc errfile
    errfile="$WORK/send-missing-row.err"
    send_out="$(cd "$root" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        "$SEND" @somebody "hello" 2>"$errfile")"
    send_rc=$?
    send_err="$(cat "$errfile" 2>/dev/null || true)"
    [ "$send_rc" -ne 0 ] || { echo "  comm-send.sh succeeded despite a missing registry row: $send_out"; return 1; }
    contains "$send_err" "no registry row" || { echo "  missing the 'no registry row' refusal: $send_err"; return 1; }
    return 0
}

case_send_refuses_when_registry_root_mismatches_current_project() {
    # Sibling of the above: the self-file resolves NAME locally (its own
    # root= matches THIS project), but the registry row for that handle has
    # since been reassigned to a DIFFERENT project's root entirely (e.g. an
    # explicit --name overwrite from elsewhere). Sending under it would
    # misroute a reply to whoever now actually holds that project — refuse.
    local root h self
    mkdir -p "$WORK/routable-wrong-row/proj16"
    root="$(realpath "$WORK/routable-wrong-row/proj16")"
    join_in "$root"
    [ "$JOIN_RC" -eq 0 ] || { echo "  setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    h="proj16-${HOST}"; self="$NEXT_SELF_FILE"
    contains "$JOIN_OUT" "Joined sot-comm as @$h" || { echo "  setup join stdout: $JOIN_OUT"; return 1; }

    local other_obj
    other_obj="$(jq -n --arg root "/somewhere/else/entirely" \
        '{host:"h",tmux:"",pane_id:"",repo:"other",root:$root,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    with_lock registry_put "$h" "$other_obj"

    local send_out send_err send_rc errfile
    errfile="$WORK/send-wrong-row.err"
    send_out="$(cd "$root" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        "$SEND" @somebody "hello" 2>"$errfile")"
    send_rc=$?
    send_err="$(cat "$errfile" 2>/dev/null || true)"
    [ "$send_rc" -ne 0 ] || { echo "  comm-send.sh succeeded despite a registry row pointing at a DIFFERENT project's root: $send_out"; return 1; }
    contains "$send_err" "DIFFERENT project" || { echo "  missing the root-mismatch refusal: $send_err"; return 1; }
    return 0
}

case_legacy_unknown_root_row() {
    # Codex review F1 / simplicity audit: a registry row that predates this
    # feature (no `root` key at all) must count as a COLLISION for the
    # derivation algorithm, not a free pass — same fail-safe stance as the
    # self-file case above, at the registry layer instead.
    local root base parent h1 h2 legacy_obj
    mkdir -p "$WORK/legacytest/grpL/proj3"
    root="$(realpath "$WORK/legacytest/grpL/proj3")"
    base="proj3"; parent="grpL"
    h1="${base}-${HOST}"
    h2="${base}-${parent}-${HOST}"

    legacy_obj="$(jq -n --arg repo "$base" \
        '{host:"other",tmux:"",pane_id:"",repo:$repo,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    jq --arg n "$h1" --argjson o "$legacy_obj" '.agents[$n] = $o' "$REGISTRY" > "$REGISTRY.tmp" \
        && mv "$REGISTRY.tmp" "$REGISTRY"

    join_in "$root"
    [ "$JOIN_RC" -eq 0 ] || { echo "  exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$h2" \
        || { echo "  stdout: $JOIN_OUT (want escalation to @$h2 — an unknown root must not be a free pass)"; return 1; }
    [ "$(registry_root "$h2")" = "$root" ] || { echo "  h2 root=$(registry_root "$h2"), want $root"; return 1; }
    [ "$(registry_has_root_key "$h1")" = "no" ] \
        || { echo "  legacy row for @$h1 was mutated (now has a root key): $(registry_root "$h1")"; return 1; }
    return 0
}

