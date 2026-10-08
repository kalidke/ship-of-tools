# test-join-disambiguation.sh part: the declared host (sot_host) names the registry row and the unpinned self slot,
# the raw host only a derived handle and a validated legacy slot (sourced after self_file.sh by the entry).

DH_N=0
DH_HOME=""
DH_RAW="rawbox"
DH_DECL="Declared-Box"

# dh_home — a fresh whole comm home in DH_HOME.
dh_home() {
    DH_N=$((DH_N + 1)); DH_HOME="$WORK/dh-$DH_N"
    SOT_COMM_HOME="$DH_HOME" bash -c 'source "$1"; ensure_home' _ "$SCRIPTS_DIR/comm-lib.sh" >/dev/null 2>&1 || return 1
}

# dh_row HANDLE WSID ROOT — a registry row in DH_HOME.
dh_row() {
    local obj
    obj="$(jq -nc --arg ws "$2" --arg root "$3" --arg repo "$(basename "$3")" \
        '{host:"rawbox",workspace_id:$ws,repo:$repo,root:$root,expertise:[],status:"idle",joined:"t",last_seen:"t"}')"
    SOT_COMM_HOME="$DH_HOME" bash -c 'source "$1"; with_lock registry_put "$2" "$3"' _ "$SCRIPTS_DIR/comm-lib.sh" "$1" "$obj"
}

# dh_context WSID ROOT [ENV...] — comm-context.sh beneath a stand-in capsule of row WSID, with no pinned self file,
# the raw host rawbox and the declared host Declared-Box. Sets DH_OUT, DH_ERR, DH_RC.
DH_OUT=""; DH_ERR=""; DH_RC=0
dh_context() {
    local ws="$1" root="$2"; shift 2
    DH_OUT="$(cd "$root" && in_row "$ws" env -u SOT_COMM_SELF_FILE SOT_COMM_HOME="$DH_HOME" SOT_WORKSPACE_ID="$ws" \
        SOT_COMM_TEST_HOST="$DH_RAW" SOT_SELF_HOST="$DH_DECL" "$@" "$CONTEXT" 2>"$WORK/dh-err.tmp")"
    DH_RC=$?
    DH_ERR="$(cat "$WORK/dh-err.tmp" 2>/dev/null || true)"
}

case_join_uses_the_declared_host_and_the_raw_handle_component() {
    local root="$WORK/dhproj" out err rc
    mkdir -p "$root"; root="$(realpath "$root")"
    out="$(cd "$root" && env -u SOT_COMM_SELF_FILE SOT_COMM_NAME= SOT_COMM_TEST_HOST="$DH_RAW" SOT_SELF_HOST="$DH_DECL" "$JOIN" 2>"$WORK/dh-err.tmp")"; rc=$?
    err="$(cat "$WORK/dh-err.tmp")"
    [ "$rc" -eq 0 ] || { echo "  join exited $rc: $err"; return 1; }
    contains "$out" "Joined sot-comm as @dhproj-$DH_RAW" || { echo "  stdout: $out (want the derived handle to keep the raw host)"; return 1; }
    [ "$(registry_field "dhproj-$DH_RAW" host)" = "$DH_DECL" ] \
        || { echo "  registry host=$(registry_field "dhproj-$DH_RAW" host), want the declared $DH_DECL"; return 1; }
    [ -f "$SELF_DIR/${DH_DECL}__nopane.txt" ] || { echo "  no declared-host slot: $(ls "$SELF_DIR" | tr '\n' ' ')"; return 1; }
    [ ! -e "$SELF_DIR/${DH_RAW}__nopane.txt" ] || { echo "  a raw-host slot was written"; return 1; }
}

case_unrepresentable_declared_host_is_refused_before_any_write() {
    local root="$WORK/dhbad" fresh="$WORK/dh-fresh" host out rc
    mkdir -p "$root"; root="$(realpath "$root")"
    for host in 'bad/host' 'bad\host' "$(printf 'a%.0s' $(seq 1 300))"; do
        out="$(cd "$root" && env -u SOT_COMM_SELF_FILE SOT_COMM_HOME="$fresh" SOT_COMM_NAME= SOT_COMM_TEST_HOST="$DH_RAW" SOT_SELF_HOST="$host" "$CONTEXT" 2>&1)"; rc=$?
        [ "$rc" -ne 0 ] || { echo "  comm-context.sh accepted '${host:0:20}'"; return 1; }
        out="$(cd "$root" && env -u SOT_COMM_SELF_FILE SOT_COMM_HOME="$fresh" SOT_COMM_NAME= SOT_COMM_TEST_HOST="$DH_RAW" SOT_SELF_HOST="$host" "$JOIN" 2>&1)"; rc=$?
        [ "$rc" -ne 0 ] || { echo "  comm-join.sh accepted '${host:0:20}'"; return 1; }
        [ ! -e "$fresh" ] || { echo "  '${host:0:20}' created the comm home before it was refused: $(ls -A "$fresh" | tr '\n' ' ')"; return 1; }
    done
}

case_self_slot_formatter_validates_the_whole_leaf() {
    local long240 long245 out
    long240="$(printf 'a%.0s' $(seq 1 240))"; long245="$(printf 'a%.0s' $(seq 1 245))"
    out="$(source "$SCRIPTS_DIR/comm-lib.sh"; _sot_self_slot "Declared-Box" "ws 1/x" 2>/dev/null)"
    [ "$out" = "Declared-Box__ws_1_x.txt" ] || { echo "  slot '$out'"; return 1; }
    out="$(source "$SCRIPTS_DIR/comm-lib.sh"; _sot_self_slot "h" "" 2>/dev/null)"
    [ "$out" = "h__nopane.txt" ] || { echo "  empty workspace: '$out'"; return 1; }
    out="$(source "$SCRIPTS_DIR/comm-lib.sh"; _sot_self_slot "$long240" "" 2>/dev/null)"
    [ "${#out}" -eq 252 ] || { echo "  a leaf of 252 bytes was refused: '${out:0:20}'"; return 1; }
    local bad
    for bad in "" "a/b" 'a\b' $'a\nb' $'a\rb' $'a\tb' "$long245"; do
        ( source "$SCRIPTS_DIR/comm-lib.sh"; _sot_self_slot "$bad" "" >/dev/null 2>&1 ) && { echo "  accepted host '${bad:0:12}' (${#bad} bytes)"; return 1; }
    done
    # The Windows rules, with the platform test forced: a character Windows cannot name, but a device-like host
    # whose whole leaf is representable stays accepted.
    for bad in "a:b" 'a?b' 'a*b' 'a"b' 'a<b' 'a>b' 'a|b'; do
        ( source "$SCRIPTS_DIR/comm-lib.sh"; _sot_is_windows() { return 0; }; _sot_self_slot "$bad" "" >/dev/null 2>&1 ) \
            && { echo "  Windows accepted host '$bad'"; return 1; }
        ( source "$SCRIPTS_DIR/comm-lib.sh"; _sot_is_windows() { return 1; }; _sot_self_slot "$bad" "" >/dev/null 2>&1 ) \
            || { echo "  a non-Windows leaf with '$bad' was refused"; return 1; }
    done
    out="$( source "$SCRIPTS_DIR/comm-lib.sh"; _sot_is_windows() { return 0; }; _sot_self_slot "con" "" 2>/dev/null)"
    [ "$out" = "con__nopane.txt" ] || { echo "  Windows refused a representable device-like host: '$out'"; return 1; }
}

# dh_legacy NAME WSID ROOT — a v2 raw-host legacy slot of row WSID in DH_HOME.
dh_legacy() {
    printf '%s\nrepo=%s\nroot=%s\n' "$1" "$(basename "$3")" "$3" > "$DH_HOME/self/${DH_RAW}__$2.txt"
}

case_legacy_raw_host_slot_migrates_only_on_registry_proof() {
    # A stand-in capsule (in_row) is an exec -a of a Unix process; a Windows walk names the executable instead.
    ! _sot_is_windows || return 2
    local root="$WORK/dhmig" canon legacy
    mkdir -p "$root"; root="$(realpath "$root")"
    canon() { printf '%s/self/%s__%s.txt' "$DH_HOME" "$DH_DECL" "$1"; }
    legacy() { printf '%s/self/%s__%s.txt' "$DH_HOME" "$DH_RAW" "$1"; }
    local name

    # Proof: a registry row of this handle naming this workspace and this root.
    dh_home && dh_row mig-handle wsM1 "$root" && dh_legacy mig-handle wsM1 "$root" || return 1
    dh_context wsM1 "$root"
    name="$(printf '%s\n' "$DH_OUT" | sed -n 's/^NAME=//p')"
    [ "$DH_RC" -eq 0 ] && [ "$name" = "mig-handle" ] || { echo "  proved legacy slot: rc $DH_RC NAME='$name': $DH_ERR"; return 1; }
    [ "$(sed -n 1p "$(canon wsM1)" 2>/dev/null)" = "mig-handle" ] || { echo "  no canonical slot after the migration"; return 1; }
    [ ! -e "$(legacy wsM1)" ] || { echo "  the validated legacy slot was not retired"; return 1; }
    dh_context wsM1 "$root"
    [ ! -e "$(legacy wsM1)" ] || { echo "  the legacy slot came back"; return 1; }

    # No registry row: the slot is only a leftover and names no identity.
    dh_home && dh_legacy ghost wsM2 "$root" || return 1
    dh_context wsM2 "$root"
    name="$(printf '%s\n' "$DH_OUT" | sed -n 's/^NAME=//p')"
    [ -z "$name" ] && [ ! -e "$(canon wsM2)" ] && [ -f "$(legacy wsM2)" ] \
        || { echo "  an unproved legacy slot moved or named '$name'"; return 1; }

    # A row of the same handle for another workspace, or another root, proves nothing.
    dh_home && dh_row other-ws wsOTHER "$root" && dh_legacy other-ws wsM3 "$root" || return 1
    dh_context wsM3 "$root"
    [ ! -e "$(canon wsM3)" ] && [ -f "$(legacy wsM3)" ] || { echo "  a slot whose row names another workspace moved"; return 1; }
    dh_home && dh_row other-root wsM4 "$WORK" && dh_legacy other-root wsM4 "$root" || return 1
    dh_context wsM4 "$root"
    [ ! -e "$(canon wsM4)" ] && [ -f "$(legacy wsM4)" ] || { echo "  a slot whose row names another root moved"; return 1; }

    # An unreadable registry authorizes nothing.
    dh_home && dh_row mig-handle wsM5 "$root" && dh_legacy mig-handle wsM5 "$root" || return 1
    printf 'not json' > "$DH_HOME/registry.json"
    dh_context wsM5 "$root"
    [ ! -e "$(canon wsM5)" ] && [ -f "$(legacy wsM5)" ] || { echo "  an unreadable registry authorized a migration"; return 1; }
}

# dh_background_context WSID ROOT BARRIER — dh_context in the background, with the registry lock seized first so
# the migration waits for it; returns once the child has reached its lock attempt. Sets DH_PID.
DH_PID=""
dh_background_context() {
    local ws="$1" root="$2" barrier="$3"
    rm -f "${barrier:?}"
    mkdir "$DH_HOME/.registry.lock" || return 1
    ( cd "$root" && in_row "$ws" env -u SOT_COMM_SELF_FILE SOT_COMM_HOME="$DH_HOME" SOT_WORKSPACE_ID="$ws" \
        SOT_COMM_TEST_HOST="$DH_RAW" SOT_SELF_HOST="$DH_DECL" SOT_COMM_TEST_LOCK_BARRIER="$barrier" "$CONTEXT" >/dev/null 2>&1 ) &
    DH_PID=$!
    await test -e "$barrier" || { rmdir "$DH_HOME/.registry.lock"; kill -9 "$DH_PID" 2>/dev/null; wait "$DH_PID" 2>/dev/null; return 1; }
}

case_legacy_migration_conflict_and_failure_keep_the_old_slot() {
    # A stand-in capsule (in_row) is an exec -a of a Unix process; a Windows walk names the executable instead.
    ! _sot_is_windows || return 2
    local root="$WORK/dhconf" fakebin="$WORK/dh-mktemp-bin" real_mktemp barrier="$WORK/dh-barrier"
    mkdir -p "$root" "$fakebin"; root="$(realpath "$root")"
    real_mktemp="$(command -v mktemp)"

    # A canonical slot populated while the migration waits for the lock is never replaced.
    dh_home && dh_row mig-handle wsC1 "$root" && dh_legacy mig-handle wsC1 "$root" || return 1
    dh_background_context wsC1 "$root" "$barrier" || { echo "  the migration never reached its lock attempt"; return 1; }
    printf 'newer-handle\nrepo=%s\nroot=%s\n' "$(basename "$root")" "$root" > "$DH_HOME/self/${DH_DECL}__wsC1.txt"
    rmdir "$DH_HOME/.registry.lock"
    await not_running "$DH_PID" || { echo "  the migration did not finish after the lock was released"; kill -9 "$DH_PID" 2>/dev/null; return 1; }
    wait "$DH_PID" 2>/dev/null
    [ "$(sed -n 1p "$DH_HOME/self/${DH_DECL}__wsC1.txt")" = "newer-handle" ] || { echo "  a newly populated canonical slot was overwritten"; return 1; }
    [ -f "$DH_HOME/self/${DH_RAW}__wsC1.txt" ] || { echo "  the legacy slot was removed beside a populated canonical one"; return 1; }

    # A legacy slot changed to another project's identity while the migration waited is revalidated, not moved.
    dh_home && dh_row mig-handle wsC2 "$root" && dh_row impostor wsC2OTHER "$WORK" && dh_legacy mig-handle wsC2 "$root" || return 1
    dh_background_context wsC2 "$root" "$barrier" || { echo "  the migration never reached its lock attempt"; return 1; }
    printf 'impostor\nrepo=%s\nroot=%s\n' "$(basename "$WORK")" "$root" > "$DH_HOME/self/${DH_RAW}__wsC2.txt"
    rmdir "$DH_HOME/.registry.lock"
    await not_running "$DH_PID" || { echo "  the migration did not finish after the lock was released"; kill -9 "$DH_PID" 2>/dev/null; return 1; }
    wait "$DH_PID" 2>/dev/null
    [ ! -e "$DH_HOME/self/${DH_DECL}__wsC2.txt" ] && [ "$(sed -n 1p "$DH_HOME/self/${DH_RAW}__wsC2.txt")" = "impostor" ] \
        || { echo "  a legacy slot changed during the wait was moved or removed"; return 1; }

    # A publication failure is loud and keeps the legacy slot.
    printf '#!/bin/sh\ncase "$*" in *%s__*) exit 1 ;; esac\nexec %s "$@"\n' "$DH_DECL" "$real_mktemp" > "$fakebin/mktemp"
    chmod +x "$fakebin/mktemp"
    dh_home && dh_row mig-handle wsC3 "$root" && dh_legacy mig-handle wsC3 "$root" || return 1
    dh_context wsC3 "$root" PATH="$fakebin:$PATH"
    contains "$DH_ERR" "could not publish the migrated self slot" || { echo "  no loud failure: $DH_ERR"; return 1; }
    [ -f "$DH_HOME/self/${DH_RAW}__wsC3.txt" ] && [ ! -e "$DH_HOME/self/${DH_DECL}__wsC3.txt" ] \
        || { echo "  a failed publication lost the legacy slot or left a canonical one"; return 1; }
}

case_explicit_pin_ignores_the_declared_host() {
    local root="$WORK/dhpin" pin="$WORK/dh-pin-self.txt" out
    mkdir -p "$root"; root="$(realpath "$root")"
    printf 'pinned-handle\nrepo=dhpin\nroot=%s\n' "$root" > "$pin"
    out="$(cd "$root" && SOT_COMM_SELF_FILE="$pin" SOT_COMM_TEST_HOST="$DH_RAW" SOT_SELF_HOST="a/b" "$CONTEXT" 2>/dev/null)"
    [ "$(printf '%s\n' "$out" | sed -n 's/^NAME=//p')" = "pinned-handle" ] || { echo "  the pin was not kept verbatim: $out"; return 1; }
}
