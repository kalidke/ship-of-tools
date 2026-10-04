# Part of test-hub-files.sh, sourced by it: local append or the wire, lock records, the broadcast flag, identity fixtures, T13.
# One append through the guard with the wire faked. $1 = own mount as
# findmnt prints it ("" = unknown), $2 = the record ("-" = absent), $3 = own
# daemon endpoint, $4 = relay endpoint ("" = none), $5 = the fake daemon's
# answer ("" = silence), $6 = 0 for no flock(1), $7 = the line. Each wire
# attempt is one `<endpoint> <frame>` line in $WORK/wire.log.
OK_ANSWER='{"v":1,"id":1,"kind":"res","op":"comm.file","payload":{"ok":true}}'
DIRECTED='{"from":"t-sender","to":"t-peer","repo":"r","msg":"/slash first","ts":"t"}'
route_append() {
    rm -f "${SOT_COMM_HOME:?}/inbox-lock-manager"
    [ "$2" = - ] || printf '%s\n' "$2" > "$SOT_COMM_HOME/inbox-lock-manager"
    printf '%s\n' "${7:-$DIRECTED}" | FAKE_MNT="$1" OWN="$3" RELAY="$4" ANSWER="$5" FLOCK="${6:-1}" \
        WIRE="$WORK/wire.log" bash -c '
            source "$1/comm-lib.sh"
            if [ "$FLOCK" = 0 ]; then _sot_have_flock() { return 1; }; fi
            sot_daemon_endpoint() { [ -n "$OWN" ] && printf "%s" "$OWN"; }
            sot_relay_endpoint() { [ -n "$RELAY" ] && printf "%s" "$RELAY"; }
            sot_oneshot_request() { printf "%s %s\n" "$ENDPOINT" "$1" >> "$WIRE"; [ -n "$ANSWER" ] && printf "%s" "$ANSWER"; }
            sot_inbox_append t-peer' _ "$BIN"
    local rc=$?
    printf '%s\n' "$RECORD" > "$SOT_COMM_HOME/inbox-lock-manager"
    return "$rc"
}
fresh_route() { rm -f "${WORK:?}/wire.log"; printf '%s\n' '{"msg":"before"}' > "$INBOX/t-peer.jsonl"; }
wire_count() { [ -e "$WORK/wire.log" ] && wc -l < "$WORK/wire.log" || echo 0; }

case_a_shared_nfs4_lock_manager_appends_locally() {
    local out rc
    fresh_route
    out="$(route_append "nfs4 rw,vers=4.2,local_lock=none A:/x" "nfs4 A:/x" unix:/own ssh:hub "$OK_ANSWER")"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    [ "$(wire_count)" -eq 0 ] || { echo "  went to the wire"; return 1; }
    [ "$(wc -l < "$INBOX/t-peer.jsonl")" -eq 2 ] || { echo "  not appended locally"; return 1; }
    return 0
}

# The hub's record is two lines, its lock manager then its writer's machine id;
# a script compares line 1 only.
case_a_two_line_record_whose_line_1_matches_appends_locally() {
    local out rc
    fresh_route
    out="$(route_append "nfs4 rw,vers=4.2,local_lock=none A:/x" "nfs4 A:/x"$'\n'"m-a" unix:/own ssh:hub "$OK_ANSWER")"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    [ "$(wire_count)" -eq 0 ] || { echo "  went to the wire"; return 1; }
    [ "$(wc -l < "$INBOX/t-peer.jsonl")" -eq 2 ] || { echo "  not appended locally"; return 1; }
    return 0
}

# An unknown lock binds the folder to the machine that wrote the record: on
# NFSv3, a record naming this machine's own `none@<machine-id>` appends
# locally under its one kernel lock (another machine's goes to the wire, below).
case_a_v3_record_naming_this_machine_appends_locally() {
    local out rc
    fresh_route
    out="$(route_append "nfs rw,vers=3 A:/x" "none@0123456789abcdef0123456789abcdef"$'\n'"0123456789abcdef0123456789abcdef" \
        unix:/own ssh:hub "$OK_ANSWER")"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    [ "$(wire_count)" -eq 0 ] || { echo "  went to the wire"; return 1; }
    [ "$(wc -l < "$INBOX/t-peer.jsonl")" -eq 2 ] || { echo "  not appended locally"; return 1; }
    return 0
}

# Every case that cannot prove one lock manager goes to the wire.
case_anything_unproven_goes_to_the_wire() {
    local mnt rec flock out rc
    while IFS='|' read -r mnt rec flock; do
        fresh_route
        out="$(route_append "$mnt" "$rec" unix:/own ssh:hub "$OK_ANSWER" "$flock")"; rc=$?
        [ "$rc" -eq 0 ] || { echo "  [$mnt|$rec|$flock] rc $rc ($out)"; return 1; }
        [ "$(wire_count)" -eq 1 ] || { echo "  [$mnt|$rec|$flock] $(wire_count) wire frames, want 1"; return 1; }
        jq -e '.op == "comm.file"' <<<"$(cut -d' ' -f2- "$WORK/wire.log")" >/dev/null \
            || { echo "  [$mnt|$rec|$flock] not comm.file"; return 1; }
        [ "$(cat "$INBOX/t-peer.jsonl")" = '{"msg":"before"}' ] || { echo "  [$mnt|$rec|$flock] appended locally"; return 1; }
    done <<'CASES'
nfs rw,vers=3 A:/x|nfs4 A:/x|1
nfs rw,vers=3 A:/x|none@fedcba9876543210fedcba9876543210|1
nfs rw,vers=3 A:/x|none|1
|nfs4 A:/x|1
nfs4 rw,vers=4.2,local_lock=none B:/x|nfs4 A:/x|1
nfs4 rw,vers=4.2,local_lock=none hub.example:/home|local 0123456789abcdef0123456789abcdef|1
nfs4 rw,vers=4.2,local_lock=none A:/x|-|1
nfs4 rw,vers=4.2,local_lock=none A:/x|none|1
fuse.sshfs rw u@far.example:/x|none|1
nfs4 rw,vers=4.2,local_lock=none A:/x|nfs4 A:/x|0
nfs4 rw,vers=4.2,local_lock=flock A:/x|nfs4 A:/x|1
CASES
    return 0
}

# perl makes the append, so a box without it cannot append locally: with
# flock(1), a matching record and a PATH holding every tool but perl, the
# send is one comm.file frame and the inbox is unchanged.
case_no_perl_goes_to_the_wire() {
    local d out rc
    mkdir -p "$WORK/noperl"
    for d in ${PATH//:/ }; do ln -s "$d"/* "$WORK/noperl/" 2>/dev/null; done
    rm -f "${WORK:?}/noperl"/perl*
    ! PATH="$WORK/noperl" command -v perl >/dev/null 2>&1 || { echo "  perl is still on the PATH"; return 1; }
    PATH="$WORK/noperl" command -v flock >/dev/null 2>&1 || { echo "  flock left the PATH"; return 1; }
    fresh_route
    out="$(PATH="$WORK/noperl" route_append "nfs4 rw,vers=4.2,local_lock=none A:/x" "nfs4 A:/x" unix:/own ssh:hub "$OK_ANSWER")"; rc=$?
    [ "$rc" -eq 0 ] || { echo "  rc $rc ($out)"; return 1; }
    [ "$(wire_count)" -eq 1 ] || { echo "  $(wire_count) wire frames, want 1"; return 1; }
    [ "$(cat "$INBOX/t-peer.jsonl")" = '{"msg":"before"}' ] || { echo "  appended locally"; return 1; }
    return 0
}

# The wire is this box's own daemon, else the relay; one route, chosen once.
case_the_wire_is_the_own_daemon_else_the_relay_and_only_one() {
    local out rc
    fresh_route; route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" unix:/own ssh:hub "$OK_ANSWER" >/dev/null
    [ "$(cut -d' ' -f1 "$WORK/wire.log")" = unix:/own ] || { echo "  own daemon not chosen: $(cat "$WORK/wire.log")"; return 1; }
    fresh_route; route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" "" ssh:hub "$OK_ANSWER" >/dev/null
    [ "$(cut -d' ' -f1 "$WORK/wire.log")" = ssh:hub ] || { echo "  relay not chosen: $(cat "$WORK/wire.log")"; return 1; }
    fresh_route; out="$(route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" unix:/own ssh:hub "")"; rc=$?
    [ "$rc" -eq 1 ] && [ "$out" = "the daemon did not answer at unix:/own" ] || { echo "  silence: rc $rc ($out)"; return 1; }
    [ "$(wire_count)" -eq 1 ] || { echo "  a second route was tried: $(cat "$WORK/wire.log")"; return 1; }
    fresh_route; out="$(route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" unix:/own ssh:hub \
        '{"v":1,"id":1,"kind":"res","op":"comm.file","payload":{"error":"no live session holds @t-peer","code":"no_live_session"}}')"; rc=$?
    [ "$rc" -eq 1 ] && [ "$out" = "no live session holds @t-peer" ] || { echo "  refusal: rc $rc ($out)"; return 1; }
    fresh_route; out="$(route_append "nfs rw,vers=3 A:/x" "nfs4 A:/x" "" "" "$OK_ANSWER")"; rc=$?
    [ "$rc" -eq 1 ] && contains "$out" "no daemon is reachable" || { echo "  no endpoint: rc $rc ($out)"; return 1; }
    [ "$(cat "$INBOX/t-peer.jsonl")" = '{"msg":"before"}' ] || { echo "  appended locally"; return 1; }
    return 0
}

# The frame: from/to/text and whether the line was a broadcast copy (to:"").
case_the_wire_frame_carries_the_broadcast_flag() {
    fresh_route; route_append "" - unix:/own "" "$OK_ANSWER" >/dev/null
    jq -e '.payload == {from:"t-sender",to:"t-peer",text:"/slash first",broadcast:false}' \
        <<<"$(cut -d' ' -f2- "$WORK/wire.log")" >/dev/null || { echo "  directed: $(cat "$WORK/wire.log")"; return 1; }
    fresh_route; route_append "" - unix:/own "" "$OK_ANSWER" 1 \
        '{"from":"t-sender","to":"","repo":"r","msg":"all","ts":"t"}' >/dev/null
    jq -e '.payload == {from:"t-sender",to:"t-peer",text:"all",broadcast:true}' \
        <<<"$(cut -d' ' -f2- "$WORK/wire.log")" >/dev/null || { echo "  broadcast: $(cat "$WORK/wire.log")"; return 1; }
    return 0
}

# Identity parity: the fixture set comm/mail/inbox_tests.rs's unit test reads, through
# the REAL identity function with findmnt pointed at each fixture.
case_the_lock_identity_matches_the_shared_fixtures() {
    local fx="$SCRIPT_DIR/fixtures/inbox-lock-identity" file path want got n=0
    command -v findmnt >/dev/null 2>&1 || { echo "  no findmnt on this box"; return 1; }
    while IFS=$'\t' read -r file path want; do
        case "$file" in ''|'#'*) continue ;; esac
        got="$(FX="$fx" MI="$fx/$file" bash -c '
            source "$1"
            _sot_findmnt() { command findmnt -F "$MI" "$@"; }
            _sot_machine_id() { local m; read -r m < "$FX/machine-id"; printf "%s" "$m"; }
            sot_inbox_lock_identity "$2"' _ "$SCRIPTS_DIR/comm-lib.sh" "$path")"
        [ "$got" = "$want" ] || { echo "  $file: got [$got], want [$want]"; return 1; }
        n=$((n + 1))
    done < "$fx/cases.tsv"
    [ "$n" -eq 7 ] || { echo "  $n fixtures, want 7"; return 1; }
    # With no machine id an unknown lock is bare `none`, which never matches.
    got="$(MI="$fx/nfs3-home.mountinfo" bash -c '
        source "$1"
        _sot_findmnt() { command findmnt -F "$MI" "$@"; }
        _sot_machine_id() { :; }
        sot_inbox_lock_identity /fixture-home' _ "$SCRIPTS_DIR/comm-lib.sh")"
    [ "$got" = none ] || { echo "  v3 with no machine id: got [$got], want [none]"; return 1; }
    return 0
}

# T13 — the wait is ONE number in both languages; no lease constant survives.
case_the_wait_is_one_number_and_no_lease_survives() {
    # The library is the loader and its seven parts: every comm-lib*.sh, read as one text.
    local libs=("$SCRIPTS_DIR"/comm-lib*.sh) names
    [ "${#libs[@]}" -ge 8 ] && [ -f "${libs[0]}" ] || { echo "  fewer than eight comm-lib*.sh in $SCRIPTS_DIR"; return 1; }
    [ "$(cat "${libs[@]}" | grep -c 'SOT_INBOX_LOCK_WAIT_SECS="${SOT_INBOX_LOCK_WAIT_SECS:-10}"')" -eq 1 ] \
        || { echo "  comm-lib*.sh does not default the wait to 10 exactly once"; return 1; }
    names="$(cat "${libs[@]}" | grep -ohE 'SOT_INBOX_LOCK_[A-Z_]+' | sort -u)"
    [ "$names" = "SOT_INBOX_LOCK_WAIT_SECS" ] || { echo "  lock knobs: $names"; return 1; }
    cat "${libs[@]}" | grep -niE 'inbox.{0,40}(stale|patience|reclaim|lease)|(stale|patience|reclaim|lease).{0,40}inbox' \
        | grep -v '^[0-9]*: *#' && { echo "  a lease constant is spelled in comm-lib*.sh"; return 1; }
    cat "${libs[@]}" | grep -n 'flock -w' | grep -qv 'SOT_INBOX_LOCK_WAIT_SECS' && { echo "  a flock wait not read from the one knob"; return 1; }
    # The Rust filer: the same knob, the same 10, no other wait outside its
    # tests, and no lease word outside a comment.
    local rs="$SCRIPT_DIR/../../rust/backend/src/comm/mail/inbox.rs"
    local rs_tests="$SCRIPT_DIR/../../rust/backend/src/comm/mail/inbox_tests.rs"
    [ -f "$rs" ] && [ -f "$rs_tests" ] || { echo "  no $rs or $rs_tests"; return 1; }
    [ "$(grep -c 'pub const INBOX_LOCK_WAIT_DEFAULT_SECS: u64 = 10;' "$rs")" -eq 1 ] \
        || { echo "  inbox.rs does not default the wait to 10 exactly once"; return 1; }
    grep -q 'pub const INBOX_LOCK_WAIT_ENV: &str = "SOT_INBOX_LOCK_WAIT_SECS";' "$rs" \
        || { echo "  inbox.rs does not read the one knob"; return 1; }
    sed '/^#\[cfg(test)\]/,$d' "$rs" | grep -nE 'from_secs\([0-9]' && { echo "  a wait literal in the filer"; return 1; }
    cat "$rs" "$rs_tests" | grep -vE '^[[:space:]]*//' | grep -niE 'stale|patience|reclaim|lease' && { echo "  a lease constant is spelled in inbox.rs"; return 1; }
    return 0
}
