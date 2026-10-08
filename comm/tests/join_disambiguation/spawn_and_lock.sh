# test-join-disambiguation.sh part: stranding warning, spawn refusals, lock gap, rollback, trap restore, hash, host alias, pinned name (sourced in order by the entry).

case_join_warns_on_stranding_escalation_when_the_bare_handle_is_live() {
    # Coordinator ruling item 2: comm-join.sh must warn LOUDLY — never
    # silently strand — when a derived join is about to escalate AWAY from
    # the bare tier-1 handle AND that bare handle still has a fresh
    # heartbeat (sot_handle_live): the near-certain signature of this
    # session's OWN evicted identity (case_legacy_unknown_root_row above is
    # the exact registry shape that forces this escalation), not a real
    # collision with an unrelated project. A stale or absent last_seen must
    # not fire it.
    local root base parent h1 h2 legacy_obj fresh stale
    mkdir -p "$WORK/strandtest/grpZ/proj7"
    root="$(realpath "$WORK/strandtest/grpZ/proj7")"
    base="proj7"; parent="grpZ"
    h1="${base}-${HOST}"
    h2="${base}-${parent}-${HOST}"
    fresh="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    stale="2000-01-01T00:00:00Z"

    # $1 = last_seen to stamp on the bare handle's row ("" = no such field)
    _strand_run() {
        legacy_obj="$(jq -n --arg repo "$base" --arg ls "$1" \
            '{host:"other",tmux:"",pane_id:"",repo:$repo,expertise:[],status:"idle",joined:"t"} + (if $ls == "" then {} else {last_seen:$ls} end)')"
        jq --arg n "$h1" --argjson o "$legacy_obj" '.agents[$n] = $o' "$REGISTRY" > "$REGISTRY.tmp" \
            && mv "$REGISTRY.tmp" "$REGISTRY"
        join_in "$root"
        [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
        contains "$JOIN_OUT" "Joined sot-comm as @$h2" \
            || { echo "  stdout: $JOIN_OUT (want escalation to @$h2)"; return 1; }
    }

    _strand_run "$fresh" || return 1
    contains "$JOIN_ERR" "WARNING" || { echo "  a fresh heartbeat under @$h1 fired no stranding warning: $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "$h1" || { echo "  warning doesn't name the bare handle @$h1: $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "comm-leave.sh --name $h2" \
        || { echo "  warning missing the exact reclaim recipe (comm-leave.sh --name $h2): $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "comm-join.sh --name $h1" \
        || { echo "  warning missing the exact reclaim recipe (comm-join.sh --name $h1): $JOIN_ERR"; return 1; }

    # A fresh join needs a clean slate: drop the escalated row.
    with_lock registry_del "$h2"
    _strand_run "$stale" || return 1
    contains "$JOIN_ERR" "WARNING" \
        && { echo "  a STALE heartbeat under @$h1 fired the stranding warning: $JOIN_ERR"; return 1; }

    with_lock registry_del "$h2"
    _strand_run "" || return 1
    contains "$JOIN_ERR" "WARNING" \
        && { echo "  an ABSENT last_seen under @$h1 fired the stranding warning: $JOIN_ERR"; return 1; }
    return 0
}

case_spawn_fresh_only_refusal() {
    # Codex review F3: comm-spawn.sh must NEVER reclaim an existing row —
    # even one sharing its own project root — the way comm-join.sh does.
    # Set up a LIVE-looking row via an ordinary join (status "idle", as a
    # real join sets), then spawn against the same root with no --name:
    # the live row must survive untouched, and the new agent must land on
    # a DIFFERENT (escalated) handle instead of clobbering it.
    local root base parent h1 h2
    mkdir -p "$WORK/spawntest/grpS/proj"
    root="$(realpath "$WORK/spawntest/grpS/proj")"
    base="proj"; parent="grpS"
    h1="${base}-${HOST}"
    h2="${base}-${parent}-${HOST}"

    join_in "$root"
    [ "$JOIN_RC" -eq 0 ] || { echo "  setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$h1" || { echo "  setup join stdout: $JOIN_OUT"; return 1; }

    # The stub daemon answers the create and reports the row ready; a real
    # sotd is never reached (the suite-wide endpoint is dead).
    PRE_CREATE_LIST='[]'
    start_stub_daemon "ws-fresh-1" "$base" "$root"
    SOT_SPAWN_ENDPOINT="unix:$STUB_SOCK" spawn_in "$root"
    local created; created="$(grep -c '"op":"workspace.create"' "$STUB_REQLOG" 2>/dev/null || echo 0)"
    stop_stub_daemon
    unset PRE_CREATE_LIST
    [ "$SPAWN_RC" -eq 0 ] || { echo "  comm-spawn.sh exited $SPAWN_RC: $SPAWN_ERR"; return 1; }
    contains "$SPAWN_OUT" "@$h2" \
        || { echo "  spawn stdout: $SPAWN_OUT (want escalation to @$h2, not a reclaim of @$h1)"; return 1; }
    [ "$created" -eq 1 ] || { echo "  the stub daemon saw $created workspace.create requests (want 1)"; return 1; }

    [ "$(registry_field "$h1" status)" = "idle" ] \
        || { echo "  @$h1 (the live row) was overwritten by spawn: status=$(registry_field "$h1" status)"; return 1; }
    [ "$(registry_root "$h1")" = "$root" ] \
        || { echo "  @$h1 root changed by spawn: $(registry_root "$h1")"; return 1; }

    [ "$(registry_root "$h2")" = "$root" ] || { echo "  @$h2 root=$(registry_root "$h2"), want $root"; return 1; }
    [ "$(registry_field "$h2" status)" = "spawning" ] \
        || { echo "  @$h2 status=$(registry_field "$h2" status), want spawning"; return 1; }
    return 0
}

case_spawn_refuses_task_when_spawner_has_no_identity() {
    # Codex review round-2 SHOULD-FIX 3/G: comm-spawn.sh used to fall back
    # to an unroutable "spawner-$HOST" placeholder sender when the
    # spawning session itself wasn't joined. --task promises a reply route
    # back to @SPAWNER; with no resolved identity there is nothing to
    # route to, so it must refuse rather than hand out a placeholder.
    local root self errfile out rc err
    mkdir -p "$WORK/spawn-no-identity/proj17"
    root="$(realpath "$WORK/spawn-no-identity/proj17")"
    next_self_file; self="$NEXT_SELF_FILE"   # never created -> no identity

    errfile="$WORK/spawn-no-identity-task.err"
    out="$(SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        "$SPAWN" "$root" --task "do the thing" 2>"$errfile")"
    rc=$?
    err="$(cat "$errfile" 2>/dev/null || true)"
    [ "$rc" -ne 0 ] || { echo "  comm-spawn.sh --task succeeded with no spawner identity: $out"; return 1; }
    contains "$err" "identity did not resolve" || { echo "  missing the identity-refusal message: $err"; return 1; }

    # Sanity: the SAME unjoined spawner, with NO --task, must still be able
    # to spawn a fire-and-forget agent — a task-less spawn makes no reply
    # promise, so it needs no identity.
    next_self_file; self="$NEXT_SELF_FILE"
    errfile="$WORK/spawn-no-identity-notask.err"
    out="$(SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        "$SPAWN" "$root" 2>"$errfile")"
    rc=$?
    err="$(cat "$errfile" 2>/dev/null || true)"
    # No daemon in this suite (dead SOT_SPAWN_ENDPOINT): a no-task spawn passes
    # the identity gate and fails only later, at the daemon step.
    contains "$err" "identity did not resolve" && { echo "  no-task spawn was refused on identity: $err"; return 1; }
    [ "$rc" -ne 0 ] || { echo "  no-task spawn succeeded with no daemon: $out"; return 1; }
    return 0
}

case_spawn_task_refuses_when_spawner_has_no_registry_row() {
    # Codex review round-3 finding 4: a VALID self-file (NAME resolves
    # locally) but a DELETED registry row used to pass the old
    # nonempty-only SPAWNER check — spawn "succeeded" (rc=0) while the
    # task silently never reached the child's inbox. The routability
    # check (same as comm-send/relay/bootstrap) now runs before any
    # socket/spawn work when --task is given.
    local root childroot h self errfile out rc err
    mkdir -p "$WORK/spawn-task-no-row/proj21" "$WORK/spawn-task-no-row/child22"
    root="$(realpath "$WORK/spawn-task-no-row/proj21")"
    childroot="$(realpath "$WORK/spawn-task-no-row/child22")"
    join_in "$root"
    [ "$JOIN_RC" -eq 0 ] || { echo "  setup join exited $JOIN_RC: $JOIN_ERR"; return 1; }
    h="proj21-${HOST}"; self="$NEXT_SELF_FILE"
    contains "$JOIN_OUT" "Joined sot-comm as @$h" || { echo "  setup join stdout: $JOIN_OUT"; return 1; }

    with_lock registry_del "$h"

    errfile="$WORK/spawn-task-no-row.err"
    out="$(cd "$root" && SOT_COMM_SELF_FILE="$self" SOT_COMM_TEST_HOST="$HOST" \
        "$SPAWN" "$childroot" --task "do the thing" 2>"$errfile")"
    rc=$?
    err="$(cat "$errfile" 2>/dev/null || true)"
    [ "$rc" -ne 0 ] || { echo "  comm-spawn.sh --task succeeded despite a deleted spawner registry row: $out"; return 1; }
    contains "$err" "no registry row" || { echo "  missing the spawner-routability refusal: $err"; return 1; }
    return 0
}

case_lock_closes_derive_write_gap() {
    # Deterministic simulation of the race claim_derived_handle exists to
    # close: two derived joins for DIFFERENT roots that would both decide on
    # the bare tier-1 handle if "derive" and "registry_put" were not one
    # locked step. True concurrency isn't reproducible deterministically in
    # bash, so this uses the registry lock itself as the synchronization
    # point instead of timing:
    #   1. We seize $LOCKDIR ourselves (standing in for "another process
    #      already holds the claim critical section").
    #   2. We start a second derived join (root B) in the BACKGROUND, with
    #      $SOT_COMM_TEST_LOCK_BARRIER pointed at a file with_lock touches
    #      right before its first mkdir attempt (Codex review F10 — this
    #      is the REAL handshake; a sleep, no matter how generous, could
    #      only ever make the failure mode LESS likely to reproduce, never
    #      prove the fix).
    #   3. We wait for that barrier file — bounded, so a child that never
    #      reaches its lock attempt fails the test instead of hanging it —
    #      then mutate the registry as if a THIRD, already-locked claim
    #      (root A) just landed, and release the lock.
    #   4. The backgrounded join can only ever observe the registry AFTER
    #      that mutation once it finally acquires the lock. If derive+put
    #      were not atomic (the pre-fix shape: decide the name, unlocked,
    #      then lock only to write), it would have "decided" tier 1 before
    #      ever touching the lock and clobbered root A's row regardless of
    #      what happened while it waited. With the fix, it must re-derive
    #      under the lock and see the collision.
    local rootA rootB rbase="racer" rh1 rh2 mutate_obj self out errfile pid rc barrier
    mkdir -p "$WORK/lockrace-a/grp/racer"
    mkdir -p "$WORK/lockrace-b/grp/racer"
    rootA="$(realpath "$WORK/lockrace-a/grp/racer")"
    rootB="$(realpath "$WORK/lockrace-b/grp/racer")"
    rh1="${rbase}-${HOST}"
    rh2="${rbase}-grp-${HOST}"

    mkdir "$LOCKDIR" || { echo "  could not seize the test lock (already held?)"; return 1; }

    next_self_file; self="$NEXT_SELF_FILE"
    out="$WORK/lockrace.out"; errfile="$WORK/lockrace.err"
    barrier="$WORK/lockrace.barrier"
    rm -f "${barrier:?}"
    ( cd "$rootB" && SOT_COMM_SELF_FILE="$self" SOT_COMM_NAME="" \
        SOT_COMM_TEST_LOCK_BARRIER="$barrier" "$JOIN" >"$out" 2>"$errfile" ) &
    pid=$!

    await test -e "$barrier" || {
        echo "  timed out waiting for the backgrounded join to reach its lock attempt"
        kill -9 "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
        rmdir "$LOCKDIR" 2>/dev/null || true
        return 1
    }

    mutate_obj="$(jq -n --arg root "$rootA" \
        '{host:"other",tmux:"",pane_id:"",repo:"racer",root:$root,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    jq --arg n "$rh1" --argjson o "$mutate_obj" '.agents[$n] = $o' "$REGISTRY" > "$REGISTRY.tmp" \
        && mv "$REGISTRY.tmp" "$REGISTRY"

    rmdir "$LOCKDIR"

    # Bounded wait on the child too (Codex review F10): a live-stuck child
    # must fail the test, not hang it forever.
    await not_running "$pid" || {
        echo "  backgrounded join did not finish after the lock was released"
        kill -9 "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
        return 1
    }
    wait "$pid"; rc=$?

    local bg_out bg_err
    bg_out="$(cat "$out" 2>/dev/null || true)"
    bg_err="$(cat "$errfile" 2>/dev/null || true)"

    [ "$rc" -eq 0 ] || { echo "  backgrounded comm-join.sh exited $rc: $bg_err"; return 1; }
    contains "$bg_out" "Joined sot-comm as @$rh2" \
        || { echo "  stdout: $bg_out (want @$rh2 — a race window let it clobber @$rh1 instead)"; return 1; }
    [ "$(registry_root "$rh1")" = "$rootA" ] \
        || { echo "  @$rh1 (the interleaved claim) was clobbered: root=$(registry_root "$rh1")"; return 1; }
    [ "$(registry_root "$rh2")" = "$rootB" ] \
        || { echo "  @$rh2 root=$(registry_root "$rh2"), want $rootB"; return 1; }
    return 0
}

case_rollback_survives_replacement_row() {
    # Codex review PR #148 round 2, finding 1: registry_del_if_provisional
    # must NOT delete a row that has since been replaced — reproduced by
    # the round-2 reviewer as an unconditional `registry_del "$NAME"`
    # deleting a genuinely live row a child had already written. Drives
    # the SAME function comm-spawn.sh's rollback trap calls (comm-lib.sh),
    # not a reimplementation, by sourcing comm-lib.sh directly (see the
    # top of this file).
    local name="rollback-test-agent" root="/fake/root/for/rollback-test"
    local nonce="nonce-abc-123" prov replacement

    prov="$(jq -n --arg root "$root" --arg nonce "$nonce" \
        '{host:"h",tmux:"",pane_id:"",repo:"r",root:$root,expertise:[],status:"spawning",joined:"t0",last_seen:"t0",nonce:$nonce}')"
    with_lock registry_put "$name" "$prov"

    # Simulate "the child joined for real" (or an explicit claimant took
    # over) BEFORE the spawner's rollback runs: a live row, no nonce.
    replacement="$(jq -n --arg root "$root" \
        '{host:"h",workspace_id:"ws-repl-1",repo:"r",root:$root,expertise:[],status:"idle",joined:"t1",last_seen:"t1"}')"
    with_lock registry_put "$name" "$replacement"

    with_lock registry_del_if_provisional "$name" "$root" "$nonce"
    local rc=$?
    [ "$rc" -eq 2 ] || { echo "  registry_del_if_provisional returned $rc, want 2 (not-ours-anymore)"; return 1; }

    [ "$(registry_field "$name" status)" = "idle" ] \
        || { echo "  the replacement row was deleted/altered: status=$(registry_field "$name" status)"; return 1; }
    [ "$(registry_field "$name" workspace_id)" = "ws-repl-1" ] \
        || { echo "  the replacement row's workspace_id is gone: $(registry_field "$name" workspace_id)"; return 1; }

    # Sanity check the OTHER branch too: an UNREPLACED provisional row
    # (matching root+nonce+status) must still be deletable.
    local name2="rollback-test-agent-2"
    with_lock registry_put "$name2" "$prov"
    with_lock registry_del_if_provisional "$name2" "$root" "$nonce"
    rc=$?
    [ "$rc" -eq 0 ] || { echo "  an untouched provisional row was NOT deleted: rc=$rc"; return 1; }
    [ "$(registry_field "$name2" status)" = "MISSING" ] \
        || { echo "  provisional row for @$name2 still present after a claimed rollback"; return 1; }

    with_lock registry_del "$name" >/dev/null 2>&1 || true
    return 0
}

case_with_lock_restores_prior_trap_on_failure() {
    # Codex review PR #148 round 2, finding 4: a callee that fails
    # DIRECTLY (not via `|| true`) under a caller's `set -e` must still
    # leave the caller's own prior EXIT trap intact — it used to be lost
    # (only with_lock's own lock-release trap fired) because `"$@"` ran as
    # a bare statement that aborted the whole subprocess right there,
    # skipping the restore lines below it. Needs a REAL subprocess with
    # its own `set -e` and its own prior trap — this test script itself
    # doesn't run under `set -e`, so the bug can't reproduce inline.
    local marker="$WORK/trap-marker.txt" script="$WORK/trap-restore-check.sh"
    rm -f "${marker:?}"
    cat > "$script" <<SCRIPT
#!/usr/bin/env bash
set -euo pipefail
source "$SCRIPTS_DIR/comm-lib.sh"
ensure_home
trap 'echo prior-trap-fired > "$marker"' EXIT
fail_cmd() { return 7; }
with_lock fail_cmd
echo UNREACHABLE >&2
SCRIPT
    bash "$script" >/dev/null 2>"$WORK/trap-restore.err"
    local rc=$?
    [ "$rc" -ne 0 ] || { echo "  subprocess did not fail as expected (rc=0): $(cat "$WORK/trap-restore.err")"; return 1; }
    [ -f "$marker" ] || { echo "  prior EXIT trap did not fire (no marker file); stderr: $(cat "$WORK/trap-restore.err")"; return 1; }
    contains "$(cat "$marker")" "prior-trap-fired" || { echo "  marker content wrong: $(cat "$marker")"; return 1; }
    [ ! -e "$LOCKDIR" ] || { echo "  lock dir leaked after the failure"; return 1; }
    return 0
}

case_hash_command_failure_fails_loudly() {
    # Codex review PR #148 round 2, finding 5: an installed-but-FAILING
    # sha256sum must not be silently accepted as success with an empty
    # hash. Both sha256sum AND shasum are faked to exit nonzero (sot_hash6
    # tries shasum as a fallback when sha256sum fails — faking only
    # sha256sum would just exercise that fallback path onto the REAL
    # shasum and succeed, not test the failure path at all) and put ahead
    # of the real ones on PATH for one comm-join.sh call, forced to reach
    # tier 3 (tiers 1-2 already taken, by ROOT1/ROOT2 from earlier cases)
    # where a hash is actually needed.
    local fakebin="$WORK/fakebin"
    mkdir -p "$fakebin"
    cat > "$fakebin/sha256sum" <<'FAKESHA'
#!/bin/sh
exit 23
FAKESHA
    cp "$fakebin/sha256sum" "$fakebin/shasum"
    chmod +x "$fakebin/sha256sum" "$fakebin/shasum"

    mkdir -p "$WORK/site4/groupX/instructor-materials"
    local root; root="$(realpath "$WORK/site4/groupX/instructor-materials")"

    JOIN_PATH_PREFIX="$fakebin"
    join_in "$root"
    JOIN_PATH_PREFIX=""

    [ "$JOIN_RC" -ne 0 ] || { echo "  expected a nonzero exit with a broken sha256sum, got 0: $JOIN_OUT"; return 1; }
    contains "$JOIN_ERR" "sot_hash6" || { echo "  missing sot_hash6 failure reason: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @" \
        && { echo "  claimed a handle despite the broken hash tool: $JOIN_OUT"; return 1; }
    return 0
}

case_host_alias_guard_triggers_on_long_host() {
    # CI incident follow-up (round 3): every OTHER case pins HOST short and
    # clean specifically so the F7 host-alias guard never fires — which
    # means the guard itself would otherwise have ZERO positive coverage in
    # this suite. Deliberately route a long host through the SAME
    # $SOT_COMM_TEST_HOST seam for just this one call, and confirm the
    # digest-suffix transformation actually happens.
    #
    # The expected value here necessarily mirrors sot_sanitize_component's
    # clamp + sot_hash6's algorithm — that's not "a parallel implementation
    # that can drift" in the sense the fix direction warned against (that
    # warning was about NOT computing per-host host-3 for the OTHER,
    # host-agnostic cases — the fix there is pinning the input, not
    # replicating the transform). Here the transform IS the thing under
    # test, so asserting its exact output requires computing what it
    # should produce — using the SAME sha256sum tool, not a hand-rolled
    # hash.
    local root long_host sanitized_prefix hash6 expected_handle saved_host
    mkdir -p "$WORK/hosttest/proj-host"
    root="$(realpath "$WORK/hosttest/proj-host")"
    long_host="ci-runner-with-a-long-dirty-hostname-example"

    sanitized_prefix="${long_host:0:12}"
    hash6="$(printf '%s' "$long_host" | sha256sum | cut -c1-6)"
    expected_handle="proj-host-${sanitized_prefix}-${hash6}"

    saved_host="$SOT_COMM_TEST_HOST"
    export SOT_COMM_TEST_HOST="$long_host"
    join_in "$root"
    export SOT_COMM_TEST_HOST="$saved_host"

    [ "$JOIN_RC" -eq 0 ] || { echo "  exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$expected_handle" \
        || { echo "  stdout: $JOIN_OUT (want @$expected_handle — sanitized-prefix + '-' + digest-of-raw-host)"; return 1; }
    [ "$(registry_root "$expected_handle")" = "$root" ] \
        || { echo "  root=$(registry_root "$expected_handle"), want $root"; return 1; }
    return 0
}

# --- capsule-comm-identity fix (Windows capsule spawn + comm scripts) ----

case_pinned_comm_name_never_adopts_selffile_identity() {
    # Coordinator hardening (capsule-comm-identity fix, item A): "a pinned
    # SOT_COMM_NAME never adopts a name from any self-file." The daemon
    # now stamps SOT_COMM_SELF_FILE on a capsule producer's own dedicated
    # slot (comm-lib.sh's EXISTING pin-the-self-file seam — no new slot-
    # naming scheme, and already honoured unchanged by both comm-context.sh
    # and comm-join.sh), so this is the second, belt-and-braces line of
    # defense: even when comm-context.sh resolves a VALID (root-matching)
    # self-file identity, a pinned SOT_COMM_NAME must still win. Before
    # this fix, comm-join.sh only consulted $SOT_COMM_NAME when NAME was
    # EMPTY — a resolved-but-wrong self-file identity silently outranked
    # the pin (the exact field bug: a capsule adopted another session's
    # handle from a shared self-file slot).
    local root self crafted
    mkdir -p "$WORK/pinned-never-adopts/proj22"
    root="$(realpath "$WORK/pinned-never-adopts/proj22")"
    crafted="$WORK/pinned-never-adopts-self.txt"
    printf 'other-existing-handle\nrepo=%s\nroot=%s\n' "$(basename "$root")" "$root" > "$crafted"

    JOIN_SELF_FILE_OVERRIDE="$crafted"
    JOIN_ENV_NAME="pinned-capsule-handle"
    join_in "$root"
    JOIN_SELF_FILE_OVERRIDE=""
    JOIN_ENV_NAME=""

    [ "$JOIN_RC" -eq 0 ] || { echo "  exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @pinned-capsule-handle" \
        || { echo "  stdout: $JOIN_OUT (want the PINNED SOT_COMM_NAME, not the self-file's 'other-existing-handle')"; return 1; }
    [ "$(registry_root "pinned-capsule-handle")" = "$root" ] \
        || { echo "  pinned-capsule-handle root=$(registry_root "pinned-capsule-handle"), want $root"; return 1; }
    return 0
}

