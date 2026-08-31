#!/usr/bin/env bash
# test-join-disambiguation.sh — self-contained test for the derived-handle
# disambiguation feature (ADR 0028 addendum: "derived vs explicit"). No bats
# dependency. HERMETIC: runs against a temp $SOT_COMM_HOME and a temp
# self-file per simulated session (via $SOT_COMM_SELF_FILE); never touches
# the real ~/.sot-comm.
#
# The last case, case_lock_closes_derive_write_gap, also covers
# claim_derived_handle's atomicity (derive + registry_put as one locked
# step): it deterministically interleaves a registry mutation into a
# backgrounded derived join's wait-for-lock window by controlling the
# registry lock directly, rather than racing on timing (see its comment).
#
# Usage: comm/core/tests/test-join-disambiguation.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
JOIN="$SCRIPTS_DIR/comm-join.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-test-XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

export SOT_COMM_HOME="$WORK/home"
mkdir -p "$SOT_COMM_HOME"
REGISTRY="$SOT_COMM_HOME/registry.json"
# Mirrors comm-lib.sh's LOCKDIR="$COMM_HOME/.registry.lock" exactly — used by
# case_lock_closes_derive_write_gap below to simulate a concurrent claim
# landing WHILE a derived join is blocked waiting for the lock.
LOCKDIR="$SOT_COMM_HOME/.registry.lock"

HOST="$(hostname -s 2>/dev/null || hostname)"

PASS=0
FAIL=0

# check DESC FN — run FN (which prints diagnostics and returns 1 on
# failure), then print the required PASS/FAIL line.
check() {
    local desc="$1" fn="$2"
    if "$fn"; then
        echo "PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "FAIL: $desc"
        FAIL=$((FAIL + 1))
    fi
}

# --- fake project roots -------------------------------------------------
# Three roots share the leaf basename "instructor-materials". The first two
# ALSO share the parent basename "groupX" — set up on purpose so the third
# forces the three-way (hash-tier) collision: tier1 (bare) and tier2
# (parentdir-qualified) are both already taken by the time it joins.
mkdir -p "$WORK/site1/groupX/instructor-materials"
mkdir -p "$WORK/site2/groupX/instructor-materials"
mkdir -p "$WORK/site3/groupX/instructor-materials"
mkdir -p "$WORK/other-repo"
mkdir -p "$WORK/other-repo2"

ROOT1="$(realpath "$WORK/site1/groupX/instructor-materials")"
ROOT2="$(realpath "$WORK/site2/groupX/instructor-materials")"
ROOT3="$(realpath "$WORK/site3/groupX/instructor-materials")"
ROOT4="$(realpath "$WORK/other-repo")"
ROOT5="$(realpath "$WORK/other-repo2")"

BASE="instructor-materials"
H1="${BASE}-${HOST}"                  # tier 1: bare
H2="${BASE}-groupX-${HOST}"           # tier 2: parentdir-qualified

# --- helpers -------------------------------------------------------------

# next_self_file — sets $NEXT_SELF_FILE to a fresh, never-before-used path.
# NOT a `$(...)` command-substitution helper on purpose: that would fork a
# subshell, and the SELFN increment would be lost on return (every call
# would hand back "self-1.txt") — the exact bug this comment now guards
# against, caught by this test failing against itself during development.
SELFN=0
NEXT_SELF_FILE=""
next_self_file() {
    SELFN=$((SELFN + 1))
    NEXT_SELF_FILE="$WORK/self-$SELFN.txt"
}

# join_in ROOT [ARGS...] — run comm-join.sh with cwd=ROOT and a FRESH,
# never-before-seen self-file, so every call simulates a brand-new session
# (no inherited identity) unless ARGS/env explicitly supply one. Sets
# JOIN_OUT / JOIN_ERR / JOIN_RC. JOIN_ENV_NAME, when non-empty, is exported
# as $SOT_COMM_NAME for that one call (used by the env-verbatim case).
JOIN_OUT=""; JOIN_ERR=""; JOIN_RC=0
JOIN_ENV_NAME=""
join_in() {
    local root="$1"; shift
    local self errfile
    next_self_file
    self="$NEXT_SELF_FILE"
    errfile="$WORK/stderr.tmp"
    JOIN_OUT="$(cd "$root" && SOT_COMM_SELF_FILE="$self" SOT_COMM_NAME="$JOIN_ENV_NAME" \
        "$JOIN" "$@" 2>"$errfile")"
    JOIN_RC=$?
    JOIN_ERR="$(cat "$errfile" 2>/dev/null || true)"
}

registry_root() {  # NAME -> prints its `root`, or MISSING if unset/absent
    jq -r --arg n "$1" '.agents[$n].root // "MISSING"' "$REGISTRY" 2>/dev/null
}

contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# --- cases -----------------------------------------------------------

case_fresh_claim() {
    join_in "$ROOT1"
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$H1" || { echo "  stdout: $JOIN_OUT"; return 1; }
    [ "$(registry_root "$H1")" = "$ROOT1" ] || { echo "  root=$(registry_root "$H1"), want $ROOT1"; return 1; }
    return 0
}

case_same_root_rejoin() {
    # A second, independent "session" (fresh self-file — simulates a new
    # pane, not a literal rejoin) joining from the SAME root: today's
    # reclaim behavior must be unchanged — same bare handle, no escalation.
    join_in "$ROOT1"
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$H1" || { echo "  stdout: $JOIN_OUT"; return 1; }
    [ "$(registry_root "$H1")" = "$ROOT1" ] || { echo "  root changed: $(registry_root "$H1")"; return 1; }
    contains "$JOIN_ERR" "already held" && { echo "  unexpected qualification notice: $JOIN_ERR"; return 1; }
    return 0
}

case_diff_root_collision() {
    join_in "$ROOT2"
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$H2" || { echo "  stdout: $JOIN_OUT (want @$H2)"; return 1; }
    [ "$(registry_root "$H2")" = "$ROOT2" ] || { echo "  H2 root=$(registry_root "$H2"), want $ROOT2"; return 1; }
    # the first entry must be untouched by the second session's escalation
    [ "$(registry_root "$H1")" = "$ROOT1" ] || { echo "  H1 (first entry) was mutated: $(registry_root "$H1")"; return 1; }
    contains "$JOIN_ERR" "$H1" || { echo "  stderr missing bare-handle notice: $JOIN_ERR"; return 1; }
    contains "$JOIN_ERR" "$H2" || { echo "  stderr missing qualified-handle notice: $JOIN_ERR"; return 1; }
    return 0
}

case_three_way_collision() {
    local hash6 h3
    hash6="$(printf '%s' "$ROOT3" | sha256sum | cut -c1-6)"
    h3="${BASE}-${hash6}-${HOST}"
    join_in "$ROOT3"
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$h3" || { echo "  stdout: $JOIN_OUT (want @$h3)"; return 1; }
    [ "$(registry_root "$h3")" = "$ROOT3" ] || { echo "  h3 root=$(registry_root "$h3"), want $ROOT3"; return 1; }
    [ "$(registry_root "$H1")" = "$ROOT1" ] || { echo "  H1 was mutated: $(registry_root "$H1")"; return 1; }
    [ "$(registry_root "$H2")" = "$ROOT2" ] || { echo "  H2 was mutated: $(registry_root "$H2")"; return 1; }
    return 0
}

case_explicit_name_verbatim() {
    # Explicitly claiming the ALREADY-HELD bare handle from a totally
    # different (4th) root must be used verbatim — no auto-disambiguation,
    # today's overwrite behavior, no qualification notice.
    join_in "$ROOT4" --name "$H1"
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$H1" || { echo "  stdout: $JOIN_OUT"; return 1; }
    [ "$(registry_root "$H1")" = "$ROOT4" ] || { echo "  expected overwrite to $ROOT4, got $(registry_root "$H1")"; return 1; }
    contains "$JOIN_ERR" "already held" && { echo "  unexpected disambiguation on explicit --name: $JOIN_ERR"; return 1; }
    return 0
}

case_env_name_verbatim() {
    # Same as above but via $SOT_COMM_NAME instead of --name, colliding with
    # the tier-2 handle claimed earlier.
    JOIN_ENV_NAME="$H2"
    join_in "$ROOT5"
    JOIN_ENV_NAME=""
    [ "$JOIN_RC" -eq 0 ] || { echo "  comm-join.sh exited $JOIN_RC: $JOIN_ERR"; return 1; }
    contains "$JOIN_OUT" "Joined sot-comm as @$H2" || { echo "  stdout: $JOIN_OUT"; return 1; }
    [ "$(registry_root "$H2")" = "$ROOT5" ] || { echo "  expected overwrite to $ROOT5, got $(registry_root "$H2")"; return 1; }
    contains "$JOIN_ERR" "already held" && { echo "  unexpected disambiguation via \$SOT_COMM_NAME: $JOIN_ERR"; return 1; }
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
    #   2. We start a second derived join (root B) in the BACKGROUND. It
    #      cannot proceed past its own `with_lock` until we release —
    #      guaranteed by mkdir semantics, not by scheduling luck.
    #   3. While it is blocked, we mutate the registry as if a THIRD,
    #      already-locked claim (root A) just landed and release the lock.
    #   4. The backgrounded join can only ever observe the registry AFTER
    #      that mutation once it finally acquires the lock. If derive+put
    #      were not atomic (the pre-fix shape: decide the name, unlocked,
    #      then lock only to write), it would have "decided" tier 1 before
    #      ever touching the lock and clobbered root A's row regardless of
    #      what happened while it waited. With the fix, it must re-derive
    #      under the lock and see the collision.
    # No sleep is load-bearing for correctness here — only for giving the
    # background job a moment to reach the spin loop before we act; if it
    # hasn't yet, our mkdir/mutate/rmdir still happen before it can ever
    # acquire the lock, so the assertion holds either way.
    local rootA rootB rbase="racer" rh1 rh2 mutate_obj self out errfile pid rc
    mkdir -p "$WORK/lockrace-a/grp/racer"
    mkdir -p "$WORK/lockrace-b/grp/racer"
    rootA="$(realpath "$WORK/lockrace-a/grp/racer")"
    rootB="$(realpath "$WORK/lockrace-b/grp/racer")"
    rh1="${rbase}-${HOST}"
    rh2="${rbase}-grp-${HOST}"

    mkdir "$LOCKDIR" || { echo "  could not seize the test lock (already held?)"; return 1; }

    next_self_file; self="$NEXT_SELF_FILE"
    out="$WORK/lockrace.out"; errfile="$WORK/lockrace.err"
    ( cd "$rootB" && SOT_COMM_SELF_FILE="$self" SOT_COMM_NAME="" "$JOIN" >"$out" 2>"$errfile" ) &
    pid=$!

    sleep 0.2   # let it reach the spin loop; not required for correctness (see above)

    mutate_obj="$(jq -n --arg root "$rootA" \
        '{host:"other",tmux:"",pane_id:"",repo:"racer",root:$root,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    jq --arg n "$rh1" --argjson o "$mutate_obj" '.agents[$n] = $o' "$REGISTRY" > "$REGISTRY.tmp" \
        && mv "$REGISTRY.tmp" "$REGISTRY"

    rmdir "$LOCKDIR"
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

# --- run, in order (later cases depend on earlier ones' registry state) --

check "fresh claim records root"                            case_fresh_claim
check "same-root rejoin keeps bare handle"                  case_same_root_rejoin
check "different-root collision -> parentdir-qualified, first entry intact" case_diff_root_collision
check "three-way collision -> hash-qualified handle"         case_three_way_collision
check "explicit --name is verbatim even when it collides"    case_explicit_name_verbatim
check "SOT_COMM_NAME env is verbatim even when it collides"  case_env_name_verbatim
check "concurrent claim landing mid-wait is not clobbered (lock closes the derive/write gap)" case_lock_closes_derive_write_gap

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
