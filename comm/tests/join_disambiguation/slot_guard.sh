# test-join-disambiguation.sh part: the identity-slot agreement guard and comm-self-audit (sourced in order by the entry).

# ---- the identity-slot agreement guard (comm-lib.sh
# sot_self_file_project_conflict). A slot is keyed by the workspace row in the
# ENVIRONMENT while the identity written into it comes from the shell's CWD,
# and the two used to be written without ever being compared: a shell in one
# row with its cwd in another row's repo handed that row's session a handle
# that was not its own, and directed mail was then filed for the wrong reader.
# The read-side matrix above catches it only AFTER that misdelivery. ----

# _plant_slot FILE HANDLE REPO ROOT — an incumbent identity in a slot.
_plant_slot() { printf '%s\nrepo=%s\nroot=%s\n' "$2" "$3" "$4" > "$1"; }

case_slot_guard_refuses_another_projects_slot() {
    local self="$WORK/slot-guard-foreign.txt"
    _plant_slot "$self" "other-repo-$HOST" "other-repo" "$ROOT4"
    JOIN_SELF_FILE_OVERRIDE="$self"
    join_in "$ROOT1"
    JOIN_SELF_FILE_OVERRIDE=""
    [ "$JOIN_RC" -eq 3 ] || { echo "  exited $JOIN_RC, want 3 (out: $JOIN_OUT err: $JOIN_ERR)"; return 1; }
    contains "$JOIN_ERR" "REFUSING" || { echo "  stderr was: $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "--repin" || { echo "  the refusal must name the way out: $JOIN_ERR"; return 1; }
    # The incumbent is untouched: the whole point is that its own session
    # keeps reading its own handle.
    [ "$(sed -n '1p' "$self")" = "other-repo-$HOST" ] \
        || { echo "  the slot was overwritten anyway: $(cat "$self")"; return 1; }
    return 0
}

case_slot_guard_repin_writes_anyway() {
    local self="$WORK/slot-guard-repin.txt"
    _plant_slot "$self" "other-repo-$HOST" "other-repo" "$ROOT4"
    JOIN_SELF_FILE_OVERRIDE="$self"
    join_in "$ROOT1" --repin
    JOIN_SELF_FILE_OVERRIDE=""
    [ "$JOIN_RC" -eq 0 ] || { echo "  exited $JOIN_RC, want 0 (err: $JOIN_ERR)"; return 1; }
    [ "$(sed -n '3p' "$self")" = "root=$ROOT1" ] \
        || { echo "  --repin did not re-pin the slot: $(cat "$self")"; return 1; }
    return 0
}

case_slot_guard_allows_a_same_project_rewrite() {
    local self="$WORK/slot-guard-same.txt"
    _plant_slot "$self" "stale-name-$HOST" "instructor-materials" "$ROOT1"
    JOIN_SELF_FILE_OVERRIDE="$self"
    join_in "$ROOT1"
    JOIN_SELF_FILE_OVERRIDE=""
    [ "$JOIN_RC" -eq 0 ] || { echo "  a same-project rewrite must not be refused: $JOIN_RC / $JOIN_ERR"; return 1; }
    return 0
}

case_slot_guard_exempts_the_shared_nopane_slot() {
    # The one slot where two projects legitimately alternate: every no-pane
    # shell on a host shares it and it is last-writer-wins BY DESIGN
    # (comm-lib.sh). A guard there would refuse ordinary use.
    local self="$WORK/slot-guard-nopane/${HOST}__nopane.txt"
    mkdir -p "$WORK/slot-guard-nopane"
    _plant_slot "$self" "other-repo-$HOST" "other-repo" "$ROOT4"
    JOIN_SELF_FILE_OVERRIDE="$self"
    join_in "$ROOT1"
    JOIN_SELF_FILE_OVERRIDE=""
    [ "$JOIN_RC" -eq 0 ] || { echo "  the shared nopane slot must stay writable: $JOIN_RC / $JOIN_ERR"; return 1; }
    [ "$(sed -n '3p' "$self")" = "root=$ROOT1" ] \
        || { echo "  the nopane slot was not rewritten: $(cat "$self")"; return 1; }
    return 0
}

case_slot_guard_refusal_in_the_write_gap_exits_three() {
    # The pre-check in comm-join.sh and the writer's own guard are ONE rule at
    # two moments, and only the writer can see a slot claimed DURING the join:
    # by a concurrent join, as here, and on a box whose deployed comm-lib.sh
    # is newer than its comm-join.sh (no pre-check at all) by every conflict
    # there is. A refusal there used to exit 1 through the writer's FATAL
    # branch, sending the operator after a directory permission that is fine
    # and never naming --repin — so the two moments must agree on exit 3.
    #
    # Same handshake as case_lock_closes_derive_write_gap: seize the registry
    # lock, start a derived join in the BACKGROUND, wait for the barrier
    # with_lock touches before its first mkdir attempt (so the join is
    # provably past its pre-check and not yet at its write), plant a foreign
    # identity in the slot, release. No sleep is load-bearing.
    local self out errfile pid rc barrier deadline err
    next_self_file; self="$NEXT_SELF_FILE"
    out="$WORK/writegap.out"; errfile="$WORK/writegap.err"
    barrier="$WORK/writegap.barrier"; rm -f "${barrier:?}"

    mkdir "$LOCKDIR" || { echo "  could not seize the test lock (already held?)"; return 1; }
    ( cd "$ROOT1" && SOT_COMM_SELF_FILE="$self" SOT_COMM_NAME="" \
        SOT_COMM_TEST_LOCK_BARRIER="$barrier" "$JOIN" >"$out" 2>"$errfile" ) &
    pid=$!

    deadline=$(( $(date +%s) + 10 ))
    until [ -e "$barrier" ]; do
        if [ "$(date +%s)" -ge "$deadline" ]; then
            echo "  timed out waiting for the backgrounded join to reach its lock attempt"
            kill -9 "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
            rmdir "$LOCKDIR" 2>/dev/null || true
            return 1
        fi
        sleep 0.02
    done

    _plant_slot "$self" "other-repo-$HOST" "other-repo" "$ROOT4"
    rmdir "$LOCKDIR"

    deadline=$(( $(date +%s) + 10 ))
    while kill -0 "$pid" 2>/dev/null; do
        if [ "$(date +%s)" -ge "$deadline" ]; then
            echo "  backgrounded join did not finish within 10s after the lock was released"
            kill -9 "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
            return 1
        fi
        sleep 0.02
    done
    wait "$pid"; rc=$?
    err="$(cat "$errfile" 2>/dev/null || true)"

    [ "$rc" -eq 3 ] || { echo "  exited $rc, want 3 — a refusal is not a write failure (err: $err)"; return 1; }
    contains "$err" "REFUSING" || { echo "  stderr was: $err"; return 1; }
    contains "$err" "--repin" || { echo "  the refusal must name the way out: $err"; return 1; }
    if contains "$err" "permissions"; then
        echo "  a refusal must not send the operator after a permissions problem: $err"; return 1
    fi
    [ "$(sed -n '1p' "$self")" = "other-repo-$HOST" ] \
        || { echo "  the incumbent was overwritten anyway: $(cat "$self")"; return 1; }
    return 0
}

case_self_audit_uses_the_daemons_own_slug_rule() {
    # One slug rule, and it is the daemon's (comm-lib.sh's sot_slug, a mirror
    # of Rust slug()). This script carried a second, sed-based copy that
    # agreed with it over every real repo name on one box but not in general:
    # a literal repeated dash survives the daemon's keep-branch, so
    # `alpha- beta` keys a row `alpha-beta` while the copy scored
    # `alpha--beta` — neither equal nor suffixed at a boundary, so a HEALTHY
    # slot printed DIFFERS and the run exited 1. Cry-wolf is the failure this
    # audit can least afford; the case fails if that copy ever returns.
    local home="$WORK/audit-slug-rule" out rc
    rm -rf "${home:?}"; mkdir -p "$home/self"
    _plant_slot "$home/self/h__ws-alpha-beta-7b1.txt" "alpha-beta-h" "alpha- beta" "/p/alpha- beta"
    out="$(SOT_COMM_HOME="$home" bash "$SCRIPTS_DIR/comm-self-audit.sh" -v 2>&1)"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  exited $rc, want 0 (out: $out)"; return 1; }
    contains "$out" "agree     ws-alpha-beta-7b1" || { echo "  want it agreeing: $out"; return 1; }
    return 0
}

case_self_audit_does_not_excuse_a_repo_suffixing_the_label() {
    # The benign rule holds in ONE direction: a row's label may continue its
    # repo's slug (`beta-cx` for repo `beta`, covered below). The reverse — a
    # row keyed `ws-alpha-…` carrying `repo=alpha-tools` — was excused too,
    # with no instance in 161 real slots, no example in the rule's own comment
    # and no test. `alpha` and `alpha-tools` are two checkouts, which is
    # precisely what this audit exists to report.
    local home="$WORK/audit-reverse-suffix" out rc
    rm -rf "${home:?}"; mkdir -p "$home/self"
    _plant_slot "$home/self/h__ws-alpha-7b2.txt" "alpha-tools-h" "alpha-tools" "/p/alpha-tools"
    out="$(SOT_COMM_HOME="$home" bash "$SCRIPTS_DIR/comm-self-audit.sh" 2>&1)"; rc=$?
    [ "$rc" -eq 1 ] || { echo "  exited $rc, want 1 (out: $out)"; return 1; }
    contains "$out" "DIFFERS   ws-alpha-7b2" || { echo "  want it reported: $out"; return 1; }
    contains "$out" "0 benign" || { echo "  it must not be counted benign: $out"; return 1; }
    return 0
}

case_self_audit_flags_only_a_slot_naming_another_project() {
    # comm-self-audit.sh over a planted slot directory: the shapes it must
    # NOT flag are as load-bearing as the one it must — a suffixed row and a
    # path-disambiguated name are deliberate, and a run that cries wolf over
    # them is a run nobody reads.
    local home="$WORK/audit-home" out rc
    rm -rf "${home:?}"; mkdir -p "$home/self"
    _plant_slot "$home/self/h__ws-alpha-6a1.txt"  "alpha-h"  "alpha"        "/p/alpha"
    _plant_slot "$home/self/h__ws-alpha_jl-6a2.txt" "Alpha-h" "Alpha.jl"    "/p/Alpha.jl"
    _plant_slot "$home/self/h__ws-beta-cx-6a3.txt" "beta-cx-h" "beta"       "/p/beta"
    _plant_slot "$home/self/h__nopane.txt"        "gamma-h"  "gamma"        "/p/gamma"
    _plant_slot "$home/self/h__17.txt"            "delta-h"  "delta"        "/p/delta"
    _plant_slot "$home/self/h__ws-epsilon-6a4.txt" "alpha-h" "alpha"        "/p/alpha"
    out="$(SOT_COMM_HOME="$home" bash "$SCRIPTS_DIR/comm-self-audit.sh" 2>&1)"; rc=$?
    [ "$rc" -eq 1 ] || { echo "  exited $rc, want 1 (out: $out)"; return 1; }
    contains "$out" "DIFFERS   ws-epsilon-6a4  names @alpha-h with repo=alpha" \
        || { echo "  the foreign slot was not named: $out"; return 1; }
    contains "$out" "2 agree, 1 benign, 1 differ, 0 without a repo= line, 2 not workspace-keyed" \
        || { echo "  wrong tally: $out"; return 1; }
    rm -f "${home:?}/self/h__ws-epsilon-6a4.txt"
    out="$(SOT_COMM_HOME="$home" bash "$SCRIPTS_DIR/comm-self-audit.sh" 2>&1)"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  a clean directory must exit 0, got $rc (out: $out)"; return 1; }
    return 0
}

