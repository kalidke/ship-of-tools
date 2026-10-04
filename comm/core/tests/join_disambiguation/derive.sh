# test-join-disambiguation.sh part: derived handles and the locked claim (sourced in order by the entry).

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

case_claim_derived_handle_tier1_three_field_parse() {
    # Codex review round-1 finding 1: sot_derive_handle's tier-1 line used
    # to be tab-delimited with an EMPTY qualifier field in the middle
    # ("$tier1\t\t$tier1"), and `IFS=$'\t' read -r a b c` collapses
    # adjacent tabs exactly like default whitespace splitting does — tab
    # is an IFS-WHITESPACE character, not a plain delimiter. The empty
    # middle field vanished instead of reading back as "", shifting
    # tier1's value into CLAIMED_QUALIFIER and leaving CLAIMED_TIER1
    # empty. Every ordinary, uncontested tier-1 spawn then misread
    # CLAIMED_QUALIFIER as non-empty and comm-spawn.sh synthesized a wrong
    # "qualified" display label (comm-spawn.sh:229-230). Assert all THREE
    # globals directly, at the comm-lib.sh level, for a plain tier-1
    # claim — this is the unit the field bug lived in, not just its
    # downstream effect.
    local root base obj expect_h1
    mkdir -p "$WORK/tier1parse/proj9"
    root="$(realpath "$WORK/tier1parse/proj9")"
    base="proj9"
    expect_h1="${base}-${HOST}"

    obj="$(jq -n --arg repo "$base" --arg root "$root" \
        '{host:"h",tmux:"",pane_id:"",repo:$repo,root:$root,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"

    CLAIMED_NAME=""; CLAIMED_QUALIFIER=""; CLAIMED_TIER1=""
    # claim_derived_handle already wraps its own with_lock — call it
    # directly, exactly as comm-join.sh does, never nested inside another
    # with_lock (which would deadlock against its own lock directory).
    claim_derived_handle reclaim "$root" "$HOST" "$obj"
    local rc=$?
    [ "$rc" -eq 0 ] || { echo "  claim_derived_handle failed: rc=$rc"; return 1; }
    [ "$CLAIMED_NAME" = "$expect_h1" ] || { echo "  CLAIMED_NAME=$CLAIMED_NAME, want $expect_h1"; return 1; }
    [ -z "$CLAIMED_QUALIFIER" ] \
        || { echo "  CLAIMED_QUALIFIER='$CLAIMED_QUALIFIER', want empty at tier 1 — non-empty is exactly the tab-collapse bug (comm-spawn.sh would synthesize a bogus qualified display label)"; return 1; }
    [ "$CLAIMED_TIER1" = "$expect_h1" ] \
        || { echo "  CLAIMED_TIER1='$CLAIMED_TIER1', want $expect_h1 — empty means the tab-collapse bug ate this field"; return 1; }

    with_lock registry_del "$expect_h1" >/dev/null 2>&1 || true
    return 0
}

