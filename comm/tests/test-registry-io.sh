#!/usr/bin/env bash
# test-registry-io.sh — the registry's one writer and one reader (B1b).
#
#   1. sot_registry_read answers 0 present, 1 absent, 2 unreadable, for a row
#      and for the whole file. A missing file, 0 bytes, truncated JSON,
#      `{"agents":5}`, `null` and two documents are all 2, with nothing on
#      stdout. (jq 1.6's -e exits 0 on an empty file, and judges two
#      documents by the last, so no read may rest on that exit code.)
#   2. A lasting empty registry (0 bytes, then truncated JSON): comm-send.sh
#      exits 1 with the unreadable reason, never "no registry row"; no inbox
#      gets a line and the relay is never run.
#   3. A registry that parses but has no row for me still refuses with
#      "no registry row".
#   4. On an empty or two-document registry every writer — registry_touch, registry_put,
#      registry_del, comm-join.sh, comm-status.sh working and stop — exits
#      nonzero with FAILED, and the bytes, the inode and the absence of a
#      registry.json.tmp all hold.
#   5. registry_replace refuses a result that is not one registry: nothing,
#      a non-registry, or two registries.
#   6. The fsync gates the rename: a perl that fails leaves the file as it was
#      and says "could not be flushed"; an mv that fails says "could not be
#      renamed into place".
#   7. ensure_home never truncates (an existing registry, an existing 0-byte
#      one) and creates the skeleton only when there is no file. A failing
#      fsync publishes nothing, a directory at the path gets nothing linked
#      into it, and no tmp is left behind.
#   8. The heartbeat hook on an empty or two-document registry exits 0 and
#      writes nothing; on a valid one with a stale stamp it does write.
#   9. The post-clear and post-compact hooks on an empty, two-document or
#      missing registry still remind; on a registry without my row they do not.
#  10. The Stop hook with unread mail: on an empty or two-document registry it
#      goes on to the mail gate and blocks; on a registry without my row it
#      ends the turn with no block.
#  11. A registry that reads as 0 bytes for ~50 ms and is then whole (a
#      scratch file renamed over it by a helper the case reaps) is re-read:
#      sot_registry_read answers my row and registry_put lands. One that stays
#      empty for 1 s is unreadable: the read answers 2 with nothing on stdout,
#      and the put FAILs and leaves the bytes and the inode as they were.
#  12. With SOT_COMM_TEST_RETRY_LOG counting retries: a missing registry is
#      absent (sot_registry_bytes answers 1, nothing on stdout) and takes no
#      retry, nor does a put on it; non-empty bytes that do not parse take no
#      retry, the read answers 2 and the put FAILs.
#  13. A registry whose read FAILS (mode 000 here; ESTALE across boxes) for
#      ~50 ms is retried: the reader answers my row, the log shows the retry
#      resolved, and a put lands. One that fails for 1 s takes 3 retries and
#      is unreadable (2, nothing on stdout), and the put FAILs leaving the
#      bytes and the inode as they were. One that vanishes while it is retried
#      is unreadable (sot_registry_bytes answers 2), never absent. Skipped,
#      with the reason printed, where mode 000 does not stop the open (root).
#  14. The same for a registry whose open succeeds and whose read fails (a
#      directory at its path here, the shape ESTALE takes across boxes): for
#      ~50 ms the retry resolves; for 1 s the read is 2 after 3 retries and the
#      put FAILs, leaving the registry that comes back as it was.
#
# No bats dependency. HERMETIC: a temp $SOT_COMM_HOME, a v2 self file, a
# pinned $SOT_COMM_TEST_HOST, and a COPY of the scripts dir with the hooks
# beside them (the deployed layout) whose comm-relay.sh, comm-listen.sh and
# comm-session-start.sh are recording stubs, so no relay, bridge or watcher is
# ever started. Never the real ~/.sot-comm.
#
# Usage: comm/core/tests/test-registry-io.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_HOOKS="$(cd "$SCRIPT_DIR/../work_state/hooks" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-registry-io-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SRC_SCRIPTS="$(guard_stage_bin "$WORK")" || exit 2
trap 'rm -rf "${WORK:?}"' EXIT

BIN="$WORK/bin"
mkdir -p "$BIN" "$SOT_COMM_HOME/inbox" "$WORK/proj"
cp "$SRC_SCRIPTS"/*.sh "$SRC_HOOKS"/*.sh "$BIN/"
for stub in comm-relay.sh comm-listen.sh comm-session-start.sh; do
    printf '#!/usr/bin/env bash\necho "%s $*" >> "%s/stub-ran"\necho REMINDER-FROM-STUB\n' "$stub" "$WORK" > "$BIN/$stub"
    chmod +x "$BIN/$stub"
done

ROOT="$(realpath "$WORK/proj")"
export SOT_COMM_TEST_HOST="testhost"
export SOT_COMM_SELF_FILE="$WORK/self"
export SOT_SPAWN_ENDPOINT="unix:$WORK/no-daemon.sock" SOT_RELAY_ENDPOINT="unix:$WORK/no-daemon.sock"
printf 'me\nrepo=proj\nroot=%s\n' "$ROOT" > "$SOT_COMM_SELF_FILE"

# shellcheck source=../scripts/comm-lib.sh
. "$BIN/comm-lib.sh"
REG="$REGISTRY"
[ "$REG" = "$SOT_COMM_HOME/registry.json" ] || { echo "FATAL: the lib's registry is $REG" >&2; exit 1; }

VALID="$(jq -n --arg r "$ROOT" '{protocol_version: 1, agents: {
    me: {host: "testhost", repo: "proj", root: $r, floor: "user", status_at: "2020-01-01T00:00:00Z"},
    peer: {host: "testhost", repo: "p2", root: "/elsewhere"}}}')"
NOT_ME="$(jq -n '{protocol_version: 1, agents: {peer: {host: "testhost", repo: "p2", root: "/elsewhere"}}}')"
# Two documents: my row only in the first (the last says "no row"), and only in the last.
TWO_ME_FIRST="$VALID"$'\n'"$NOT_ME"
TWO_ME_LAST="$NOT_ME"$'\n'"$VALID"

put_reg() {  # BODY | MISSING — the registry's whole content, or no file
    rm -f "${REG:?}" "${REG:?}.tmp"
    [ "$1" = MISSING ] || printf '%s' "$1" > "$REG"
}
snap() { cp "$REG" "$WORK/snap"; INODE="$(stat -c %i "$REG")"; }
same() {  # LABEL — bytes and inode unchanged, and no tmp left behind
    cmp -s "$REG" "$WORK/snap" || { echo "  $1: the registry's bytes changed"; return 1; }
    [ "$(stat -c %i "$REG")" = "$INODE" ] || { echo "  $1: the registry's inode changed"; return 1; }
    [ ! -e "$REG.tmp" ] || { echo "  $1: registry.json.tmp was left behind"; return 1; }
}
in_root() { (cd "$ROOT" && "$@"); }

case_reader_table() {
    local body arg out rc
    for body in MISSING "" '{"agents": {"me": {"host"' '{"agents":5}' 'null' "$TWO_ME_FIRST" "$TWO_ME_LAST"; do
        put_reg "$body"
        for arg in me WHOLE; do
            rc=0
            if [ "$arg" = WHOLE ]; then out="$(sot_registry_read)" || rc=$?
            else out="$(sot_registry_read "$arg")" || rc=$?; fi
            [ "$rc" -eq 2 ] || { echo "  [$body] $arg: rc $rc, want 2 (unreadable)"; return 1; }
            [ -z "$out" ] || { echo "  [$body] $arg: printed '$out' on unreadable"; return 1; }
        done
    done
    put_reg "$VALID"
    rc=0; out="$(sot_registry_read nobody)" || rc=$?
    [ "$rc" -eq 1 ] && [ -z "$out" ] || { echo "  no such row: rc $rc out '$out', want 1 and nothing"; return 1; }
    rc=0; out="$(sot_registry_read me)" || rc=$?
    [ "$rc" -eq 0 ] || { echo "  my row: rc $rc, want 0"; return 1; }
    [ "$(printf '%s' "$out" | jq -r .root)" = "$ROOT" ] || { echo "  my row printed '$out'"; return 1; }
    rc=0; out="$(sot_registry_read)" || rc=$?
    [ "$rc" -eq 0 ] || { echo "  whole file: rc $rc, want 0"; return 1; }
    printf '%s' "$out" | jq -e '.agents.peer.repo == "p2"' >/dev/null || { echo "  whole file printed '$out'"; return 1; }
    [ "$(sot_registry_entry_status nobody)" = "$(printf 'absent\t')" ] || { echo "  entry_status of no row is not absent"; return 1; }
    put_reg ""
    [ "$(sot_registry_entry_status me)" = "$(printf 'error\t')" ] || { echo "  entry_status of an empty file is not error"; return 1; }
    put_reg "$TWO_ME_FIRST"
    [ "$(sot_registry_entry_status me)" = "$(printf 'error\t')" ] || { echo "  entry_status of two documents is not error"; return 1; }
}

case_send_on_a_lasting_empty_registry_is_unreadable() {
    local body err rc
    for body in "" '{"protocol_version": 1, "agents": {"me": {'; do
        put_reg "$body"; snap; rm -f "${WORK:?}/stub-ran"
        rc=0; in_root "$BIN/comm-send.sh" @peer "hello" >/dev/null 2>"$WORK/err" || rc=$?
        err="$(cat "$WORK/err")"
        [ "$rc" -eq 1 ] || { echo "  [$body] comm-send.sh exited $rc, want 1: $err"; return 1; }
        case "$err" in *"FAILED -> @peer: the registry could not be read, so identity @me is unverified; nothing was sent"*) ;;
            *) echo "  [$body] missing the unreadable reason: $err"; return 1 ;; esac
        case "$err" in *"no registry row"*) echo "  [$body] an unreadable registry read as no row: $err"; return 1 ;; esac
        [ -z "$(find "$SOT_COMM_HOME/inbox" -type f -size +0 2>/dev/null)" ] || { echo "  [$body] an inbox got a line"; return 1; }
        [ ! -e "$WORK/stub-ran" ] || { echo "  [$body] the relay ran: $(cat "$WORK/stub-ran")"; return 1; }
        same "[$body] comm-send.sh" || return 1
    done
}

case_a_parsed_registry_without_my_row_is_no_row() {
    local err rc
    put_reg "$NOT_ME"
    rc=0; in_root "$BIN/comm-send.sh" @peer "hello" >/dev/null 2>"$WORK/err" || rc=$?
    err="$(cat "$WORK/err")"
    [ "$rc" -eq 1 ] || { echo "  comm-send.sh exited $rc, want 1: $err"; return 1; }
    case "$err" in *"FAILED -> @peer: your identity @me has no registry row; reclaim it with "*"comm-join.sh --name me"*) ;;
        *) echo "  missing reason A: $err"; return 1 ;; esac
}

case_every_writer_on_an_empty_registry_writes_nothing() {
    local label rc err body
    for body in "" "$TWO_ME_LAST"; do
    for label in touch put del join working stop; do
        put_reg "$body"; snap
        rc=0
        case "$label" in
            touch)   with_lock registry_touch me 2>"$WORK/err" || rc=$? ;;
            put)     with_lock registry_put x '{"host":"testhost"}' 2>"$WORK/err" || rc=$? ;;
            del)     with_lock registry_del me 2>"$WORK/err" || rc=$? ;;
            join)    in_root "$BIN/comm-join.sh" --name x >/dev/null 2>"$WORK/err" || rc=$? ;;
            working) in_root "$BIN/comm-status.sh" working "x" >/dev/null 2>"$WORK/err" || rc=$? ;;
            stop)    in_root "$BIN/comm-status.sh" stop >/dev/null 2>"$WORK/err" || rc=$? ;;
        esac
        err="$(cat "$WORK/err")"
        [ "$rc" -ne 0 ] || { echo "  $label [${body:0:20}]: exited 0 on an unreadable registry"; return 1; }
        case "$err" in *FAILED*) ;; *) echo "  $label [${body:0:20}]: no FAILED line: $err"; return 1 ;; esac
        same "$label [${body:0:20}]" || return 1
    done
    done
}

case_registry_replace_refuses_a_non_registry() {
    local filter rc
    for filter in 'empty' '{"a":1}' '., .'; do
        put_reg "$VALID"; snap
        rc=0; registry_replace "$filter" 2>/dev/null || rc=$?
        [ "$rc" -ne 0 ] || { echo "  registry_replace '$filter' returned 0"; return 1; }
        same "registry_replace '$filter'" || return 1
    done
}

case_the_fsync_gates_the_rename() {
    local rc err
    mkdir -p "$WORK/noperl"
    printf '#!/bin/sh\nexit 1\n' > "$WORK/noperl/perl"; chmod +x "$WORK/noperl/perl"
    put_reg "$VALID"; snap
    rc=0; PATH="$WORK/noperl:$PATH" registry_put x '{"host":"testhost"}' 2>"$WORK/err" || rc=$?
    err="$(cat "$WORK/err")"
    [ "$rc" -ne 0 ] || { echo "  registry_put returned 0 with a failing fsync"; return 1; }
    case "$err" in *"could not be flushed"*) ;; *) echo "  missing 'could not be flushed': $err"; return 1 ;; esac
    same "a failing fsync" || return 1
    mkdir -p "$WORK/nomv"
    printf '#!/bin/sh\nexit 1\n' > "$WORK/nomv/mv"; chmod +x "$WORK/nomv/mv"
    rc=0; PATH="$WORK/nomv:$PATH" registry_put x '{"host":"testhost"}' 2>"$WORK/err" || rc=$?
    err="$(cat "$WORK/err")"
    [ "$rc" -ne 0 ] || { echo "  registry_put returned 0 with a failing mv"; return 1; }
    case "$err" in *"could not be renamed into place"*) ;; *) echo "  missing 'could not be renamed into place': $err"; return 1 ;; esac
    same "a failing mv" || return 1
    # And with the real perl the same put lands.
    registry_put x '{"host":"testhost"}' || { echo "  registry_put failed with a working perl"; return 1; }
    jq -e '.agents.x.host == "testhost"' "$REG" >/dev/null || { echo "  the put did not land"; return 1; }
}

case_ensure_home_never_truncates() {
    local body
    for body in "$VALID" ""; do
        put_reg "$body"; snap
        ensure_home
        same "ensure_home on [${body:0:20}]" || return 1
    done
    put_reg MISSING
    ensure_home
    jq -e --argjson v "$PROTOCOL_VERSION" '.protocol_version == $v and .agents == {}' "$REG" >/dev/null \
        || { echo "  ensure_home on no file wrote '$(cat "$REG" 2>/dev/null)'"; return 1; }
    set -- "$REG".new.*; [ ! -e "$1" ] || { echo "  ensure_home left its tmp: $1"; return 1; }
    # A perl whose fsync (the -MIO::Handle call) fails and whose every other
    # call, the link included, is the real perl's: only the fsync gates it.
    mkdir -p "$WORK/nosync"
    printf '#!/bin/sh\ncase "$1" in -MIO::Handle) exit 1 ;; esac\nexec %s "$@"\n' "$(command -v perl)" > "$WORK/nosync/perl"
    chmod +x "$WORK/nosync/perl"
    put_reg MISSING
    PATH="$WORK/nosync:$PATH" ensure_home
    [ ! -e "$REG" ] || { echo "  ensure_home published a skeleton it could not fsync"; return 1; }
    set -- "$REG".new.*; [ ! -e "$1" ] || { echo "  ensure_home left its tmp: $1"; return 1; }
    put_reg MISSING; mkdir "$REG"
    ensure_home
    set -- "$REG"/* "$REG".new.*; rmdir "$REG" || { echo "  ensure_home linked into a directory at the path"; return 1; }
    [ ! -e "$2" ] || { echo "  ensure_home left its tmp: $2"; return 1; }
}

heartbeat() {  # KEY — one heartbeat past its throttle (a fresh tick key)
    printf '{"tool_name":"Bash"}' | in_root env SOT_WORKSPACE_ID="$1" bash "$BIN/comm-status-heartbeat.sh" >/dev/null 2>&1
}

case_the_heartbeat_on_an_unreadable_registry_writes_nothing() {
    local rc body n=0
    for body in "" "$TWO_ME_LAST"; do
        n=$((n + 1)); put_reg "$body"; snap
        rc=0; heartbeat "hb-unreadable-$n" || rc=$?
        [ "$rc" -eq 0 ] || { echo "  [${body:0:20}] the heartbeat exited $rc"; return 1; }
        same "the heartbeat [${body:0:20}]" || return 1
    done
    # The control: the same call on a valid registry with a stale stamp writes.
    put_reg "$VALID"
    heartbeat hb-valid || { echo "  the heartbeat on a valid registry exited nonzero"; return 1; }
    jq -e '.agents.me.status_at != "2020-01-01T00:00:00Z"' "$REG" >/dev/null \
        || { echo "  the heartbeat on a valid registry did not stamp: $(cat "$REG")"; return 1; }
}

case_the_stop_hook_on_an_unreadable_registry_reaches_the_mail_gate() {
    local out body n=0
    printf '{"to":"me","from":"peer","msg":"hi"}\n' > "$SOT_COMM_HOME/inbox/me.jsonl"
    for body in "" "$TWO_ME_FIRST" "$NOT_ME"; do
        # A fresh key each time: the mail nudge fires once per key's tick.
        n=$((n + 1)); put_reg "$body"
        out="$(printf '{"hook_event_name":"Stop"}' | in_root env SOT_WORKSPACE_ID="stop-mail-$n" bash "$BIN/comm-status-idle.sh" 2>/dev/null)"
        if [ "$body" = "$NOT_ME" ]; then
            [ -z "$out" ] || { echo "  no row: the Stop hook did not end the turn: '$out'"; return 1; }
        else
            case "$out" in *'"block"'*) ;; *) echo "  [${body:0:20}]: unread mail did not block: '$out'"; return 1 ;; esac
        fi
    done
    rm -f "${SOT_COMM_HOME:?}/inbox/me.jsonl"
}

# empty_for SECS — the registry is 0 bytes now and $VALID after SECS, renamed
# over it from a scratch file by a helper ($SWAP) the caller reaps.
empty_for() {
    put_reg ""; printf '%s' "$VALID" > "$WORK/whole"
    ( sleep "$1"; mv "$WORK/whole" "$REG" ) & SWAP=$!
}

case_a_zero_byte_read_is_re_read_and_a_lasting_one_is_unreadable() {
    local rc out held
    empty_for 0.05
    rc=0; out="$(sot_registry_read me)" || rc=$?; wait "$SWAP"
    [ "$rc" -eq 0 ] && [ "$(printf '%s' "$out" | jq -r .root)" = "$ROOT" ] \
        || { echo "  reader, empty for 50 ms: rc $rc out '$out', want my row"; return 1; }
    empty_for 0.05
    rc=0; with_lock registry_put x '{"host":"testhost"}' 2>"$WORK/err" || rc=$?; wait "$SWAP"
    [ "$rc" -eq 0 ] && jq -e '.agents.x.host == "testhost" and .agents.me.root != null' "$REG" >/dev/null \
        || { echo "  writer, empty for 50 ms: rc $rc: $(cat "$WORK/err")"; return 1; }
    empty_for 1
    rc=0; out="$(sot_registry_read me)" || rc=$?; wait "$SWAP"
    [ "$rc" -eq 2 ] && [ -z "$out" ] || { echo "  reader, empty for 1 s: rc $rc out '$out', want 2 and nothing"; return 1; }
    empty_for 1; snap
    rc=0; with_lock registry_put x '{"host":"testhost"}' 2>"$WORK/err" || rc=$?
    held=0; same "writer, empty for 1 s" > "$WORK/same" || held=1   # before the helper's rename
    wait "$SWAP"
    [ "$rc" -ne 0 ] && grep -q FAILED "$WORK/err" && [ "$held" -eq 0 ] \
        || { echo "  writer, empty for 1 s: rc $rc: $(cat "$WORK/err" "$WORK/same")"; return 1; }
}

export SOT_COMM_TEST_RETRY_LOG="$WORK/retries"
retries() { grep -c "^$1" "$SOT_COMM_TEST_RETRY_LOG" 2>/dev/null; }   # PREFIX — lines logged

case_a_missing_registry_is_absent_and_unparseable_bytes_are_not_retried() {
    local rc out
    put_reg MISSING; : > "$SOT_COMM_TEST_RETRY_LOG"
    rc=0; sot_registry_bytes > "$WORK/out" || rc=$?
    [ "$rc" -eq 1 ] && [ ! -s "$WORK/out" ] && [ "$(retries retry)" -eq 0 ] \
        || { echo "  missing: rc $rc, $(retries retry) retries, want 1 (absent) and none"; return 1; }
    with_lock registry_put x '{"host":"testhost"}' 2>/dev/null; rm -f "${REG:?}"
    [ "$(retries retry)" -eq 0 ] || { echo "  missing, put: $(retries retry) retries, want none"; return 1; }
    put_reg '{"agents": {"me": {"host"'
    rc=0; out="$(sot_registry_read me)" || rc=$?
    [ "$rc" -eq 2 ] && [ -z "$out" ] && [ "$(retries retry)" -eq 0 ] \
        || { echo "  unparseable: rc $rc out '$out', $(retries retry) retries, want 2 and none"; return 1; }
    rc=0; with_lock registry_put x '{"host":"testhost"}' 2>"$WORK/err" || rc=$?
    [ "$rc" -ne 0 ] && grep -q FAILED "$WORK/err" && [ "$(retries retry)" -eq 0 ] \
        || { echo "  unparseable, put: rc $rc, $(retries retry) retries: $(cat "$WORK/err")"; return 1; }
}

# fail_for SECS [vanish] — the registry is $VALID with mode 000 (its open
# fails) now, and after SECS readable again, or removed; by a helper ($SWAP)
# the caller reaps.
fail_for() {
    put_reg "$VALID"; chmod 000 "$REG"
    if [ "${2-}" = vanish ]; then ( sleep "$1"; rm -f "${REG:?}" ) & SWAP=$!
    else ( sleep "$1"; chmod 644 "$REG" ) & SWAP=$!; fi
}

case_a_failed_read_is_retried_and_a_lasting_one_is_unreadable() {
    local rc out held
    put_reg "$VALID"; chmod 000 "$REG"
    if cat "$REG" >/dev/null 2>&1; then echo "SKIP: mode 000 does not stop this user's open (root)"; return 0; fi
    fail_for 0.05; : > "$SOT_COMM_TEST_RETRY_LOG"
    rc=0; out="$(sot_registry_read me)" || rc=$?; wait "$SWAP"
    [ "$rc" -eq 0 ] && [ "$(printf '%s' "$out" | jq -r .root)" = "$ROOT" ] && [ "$(retries resolved)" -eq 1 ] \
        || { echo "  reader, failing for 50 ms: rc $rc out '$out', log: $(tr '\n' ' ' < "$SOT_COMM_TEST_RETRY_LOG"), want my row, resolved"; return 1; }
    fail_for 0.05
    rc=0; with_lock registry_put x '{"host":"testhost"}' 2>"$WORK/err" || rc=$?; wait "$SWAP"
    [ "$rc" -eq 0 ] && jq -e '.agents.x.host == "testhost" and .agents.me.root != null' "$REG" >/dev/null \
        || { echo "  writer, failing for 50 ms: rc $rc: $(cat "$WORK/err")"; return 1; }
    fail_for 1; : > "$SOT_COMM_TEST_RETRY_LOG"
    rc=0; out="$(sot_registry_read me)" || rc=$?; wait "$SWAP"
    [ "$rc" -eq 2 ] && [ -z "$out" ] && [ "$(retries retry)" -eq 3 ] && [ "$(retries resolved)" -eq 0 ] \
        || { echo "  reader, failing for 1 s: rc $rc out '$out', log: $(tr '\n' ' ' < "$SOT_COMM_TEST_RETRY_LOG"), want 2, 3 retries, none resolved"; return 1; }
    fail_for 1; chmod 644 "$REG"; snap; chmod 000 "$REG"
    rc=0; with_lock registry_put x '{"host":"testhost"}' 2>"$WORK/err" || rc=$?
    wait "$SWAP"   # the helper's chmod keeps the bytes and the inode; `same` needs them readable
    held=0; same "writer, failing for 1 s" > "$WORK/same" || held=1
    [ "$rc" -ne 0 ] && grep -q FAILED "$WORK/err" && [ "$held" -eq 0 ] \
        || { echo "  writer, failing for 1 s: rc $rc: $(cat "$WORK/err" "$WORK/same")"; return 1; }
    fail_for 0.05 vanish
    rc=0; sot_registry_bytes > "$WORK/out" || rc=$?; wait "$SWAP"
    [ "$rc" -eq 2 ] && [ ! -s "$WORK/out" ] || { echo "  vanishing mid-retry: rc $rc, want 2 (unreadable), never 1"; return 1; }
}

# dir_for SECS — a directory at the registry's path (the open succeeds, the
# read fails) now, and after SECS $VALID again; by a helper ($SWAP) the caller reaps.
dir_for() {
    put_reg "$VALID"; mv "$REG" "$WORK/valid"; mkdir "$REG"
    ( sleep "$1"; rmdir "$REG"; mv "$WORK/valid" "$REG" ) & SWAP=$!
}

case_a_registry_that_opens_but_will_not_read_is_retried_and_a_lasting_one_is_unreadable() {
    local rc out
    dir_for 0.05; : > "$SOT_COMM_TEST_RETRY_LOG"
    rc=0; out="$(sot_registry_read me)" || rc=$?; wait "$SWAP"
    [ "$rc" -eq 0 ] && [ "$(printf '%s' "$out" | jq -r .root)" = "$ROOT" ] && [ "$(retries resolved)" -eq 1 ] \
        || { echo "  reader, a directory for 50 ms: rc $rc out '$out', log: $(tr '\n' ' ' < "$SOT_COMM_TEST_RETRY_LOG"), want my row, resolved"; return 1; }
    dir_for 1; : > "$SOT_COMM_TEST_RETRY_LOG"
    rc=0; out="$(sot_registry_read me)" || rc=$?; wait "$SWAP"
    [ "$rc" -eq 2 ] && [ -z "$out" ] && [ "$(retries retry)" -eq 3 ] && [ "$(retries resolved)" -eq 0 ] \
        || { echo "  reader, a directory for 1 s: rc $rc out '$out', log: $(tr '\n' ' ' < "$SOT_COMM_TEST_RETRY_LOG"), want 2, 3 retries, none resolved"; return 1; }
    dir_for 1; cp "$WORK/valid" "$WORK/snap"
    rc=0; with_lock registry_put x '{"host":"testhost"}' 2>"$WORK/err" || rc=$?; wait "$SWAP"
    [ "$rc" -ne 0 ] && grep -q FAILED "$WORK/err" && cmp -s "$REG" "$WORK/snap" && [ ! -e "$REG.tmp" ] \
        || { echo "  writer, a directory for 1 s: rc $rc: $(cat "$WORK/err")"; return 1; }
}

PASS=0; FAIL=0
for c in case_reader_table \
         case_send_on_a_lasting_empty_registry_is_unreadable \
         case_a_parsed_registry_without_my_row_is_no_row \
         case_every_writer_on_an_empty_registry_writes_nothing \
         case_registry_replace_refuses_a_non_registry \
         case_the_fsync_gates_the_rename \
         case_ensure_home_never_truncates \
         case_the_heartbeat_on_an_unreadable_registry_writes_nothing \
         case_the_stop_hook_on_an_unreadable_registry_reaches_the_mail_gate \
         case_a_zero_byte_read_is_re_read_and_a_lasting_one_is_unreadable \
         case_a_missing_registry_is_absent_and_unparseable_bytes_are_not_retried \
         case_a_failed_read_is_retried_and_a_lasting_one_is_unreadable \
         case_a_registry_that_opens_but_will_not_read_is_retried_and_a_lasting_one_is_unreadable; do
    # Each case runs in a subshell with the cleanup trap cleared: with_lock
    # saves and restores the EXIT trap it sees, and a subshell sees this one.
    if out="$(trap - EXIT; "$c" 2>&1)"; then
        case "$out" in SKIP:*) echo "SKIP $c${out#SKIP}" ;; *) echo "PASS $c"; PASS=$((PASS + 1)) ;; esac
    else echo "FAIL $c"; printf '%s\n' "$out"; FAIL=$((FAIL + 1)); fi
done
echo "registry-io: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
