#!/usr/bin/env bash
# test-hub-files.sh — the inbox append (0031 B1): one lock, both writers,
# fail-closed.
#
#   1. Two writers through `sot_inbox_append` on one inbox give every line
#      whole: none torn, none interleaved, none lost.
#      A writer killed mid-line leaves a partial that the next writer CUTS
#      back to the last newline; a reader never counts it.
#   2. The lock is the kernel's: a holder killed with -9 frees it at once, so
#      the next send files with nothing to time out; a FROZEN holder makes the
#      next send wait its bound and then report FAILED, never `filed`, and when
#      the holder resumes the inbox has no torn line.
#   3. A script appends locally only when flock(1) and perl exist, this is
#      Linux, and its lock identity for the inbox equals the daemon's record;
#      NFSv3 or an unknown mount is `none@<machine-id>`, local only against a
#      record this machine wrote. Another machine's `none@…`, a mismatched
#      export, a host mounting the daemon's local disk, no record, a bare
#      `none` record, no flock(1) and no perl all go to the wire —
#      the fake daemon gets exactly one `comm.file` and the inbox is unchanged.
#      The wire is this box's own daemon, else the relay endpoint, and one that
#      does not answer is FAILED with no second route tried.
#   4. The wait is ONE number, 10, in both languages, and no stale/patience/
#      reclaim constant is spelled at all — the lease is deleted and this
#      keeps it deleted.
#   5. The lock identity: the same fixture set comm/mail/inbox_tests.rs's unit test reads
#      gives the same strings here; a broadcast copy says so on the wire.
#   6. A wire send (comm-relay.sh's send_frame) is one `comm.file` frame with
#      no id, and prints the hub's answer: `filed -> @h`, or `FAILED -> @h:`
#      and the daemon's own sentence, code or no code, or the no-answer
#      sentence; `not_here` alone falls back to `agent.send`. The read window
#      outlasts the hub's lock wait, so a line filed after it is not FAILED.
#   7. A hub-filed line and a locally-filed line read the same through
#      comm-poll.sh, and both advance the one cursor.
#   8. Every inbox lock descriptor is opened read-write (an NFS client
#      refuses a shared lock without read access); the cursor hashes the
#      bytes the reader held, so a line filed after a cut-back is shown; a
#      hashed cursor one past the end steps back one; a poll shows its batch
#      after letting go of the lock, so a slow display never holds off a writer.
#
# No bats dependency. HERMETIC: a temp $SOT_COMM_HOME, per-case self files, a
# pinned $SOT_COMM_TEST_HOST, and a COPY of the scripts dir whose comm-lib.sh
# gets its endpoints and mount lookup APPENDED (a sourced file's later
# definitions win) so no real daemon is ever dialled and the route does not
# depend on this box's disks. Never the real ~/.sot-comm.
#
# Usage: comm/tests/test-hub-files.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/lib-wait.sh"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-hub-files-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
mkdir -p "$SOT_COMM_HOME/inbox"
HOLDERS=()
trap 'for p in "${HOLDERS[@]}"; do kill -9 "$p" 2>/dev/null; done; rm -rf "${WORK:?}"' EXIT

BIN="$WORK/bin"
cp -r "$SCRIPTS_DIR" "$BIN" && chmod -R u+w "$BIN" || { echo "FATAL: cannot copy the scripts" >&2; exit 1; }
cat >> "$BIN/comm-lib.sh" <<'STUB'

# ---- no daemon, a fixture mount (test only) ---------------------------------
sot_daemon_endpoint() { return 1; }
sot_relay_endpoint() { [ -n "${1:-}" ] || return 1; printf '%s\n' "$1"; }  # an explicit one is used as given
_sot_findmnt() { printf '%s\n' "${FAKE_MNT-nfs4 rw,vers=4.2,local_lock=none filer.example:/export/home}"; }
_sot_machine_id() { printf '0123456789abcdef0123456789abcdef'; }
STUB
grep -q '^_sot_machine_id() { printf' "$BIN/comm-lib.sh" || { echo "FATAL: the fixture stub did not land in the copy" >&2; exit 1; }
RECORD="nfs4 filer.example:/export/home"
printf '%s\n' "$RECORD" > "$SOT_COMM_HOME/inbox-lock-manager"
SEND="$BIN/comm-send.sh"
JOIN="$BIN/comm-join.sh"
LIB="$BIN/comm-lib.sh"
INBOX="$SOT_COMM_HOME/inbox"

HOST_PIN="testhost"
SENDER="t-sender"
PEER="t-peer"

PASS=0; FAIL=0
check() {
    local desc="$1" fn="$2" rc
    "$fn"; rc=$?
    case "$rc" in
        0) echo "PASS: $desc"; PASS=$((PASS + 1)) ;;
        *) echo "FAIL: $desc"; FAIL=$((FAIL + 1)) ;;
    esac
}
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

setup_rows() {
    rm -f "${SOT_COMM_HOME:?}/registry.json" "${INBOX:?}"/* "${SOT_COMM_HOME:?}"/read/*.cursor
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
        "$JOIN" --name "$PEER" ) >/dev/null 2>&1 || return 1
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-sender.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
        "$JOIN" --name "$SENDER" ) >/dev/null 2>&1 || return 1
    : > "$INBOX/$PEER.jsonl"
}

SEND_OUT=""; SEND_ERR=""; SEND_RC=0
run_send() {
    SEND_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-sender.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
        "$SEND" "$@" 2>"$WORK/err.txt")"
    SEND_RC=$?
    SEND_ERR="$(cat "$WORK/err.txt" 2>/dev/null)"
    return 0
}

# One line through the copy's own helper. $1 = handle, $2 = the JSON line.
append_one() {
    printf '%s\n' "$2" | bash -c 'source "$1/comm-lib.sh"; sot_inbox_append "$2"' _ "$BIN" "$1"
}

# Every line of $1 is one JSON object; prints the count, fails on any other.
whole_lines() {
    local f="$1" n bad
    n="$(wc -l < "$f")"
    bad="$(jq -c 'select(type != "object")' "$f" 2>&1 >/dev/null | wc -l)"
    [ "$(jq -c . "$f" 2>/dev/null | wc -l)" -eq "$n" ] && [ "$bad" -eq 0 ] || return 1
    [ -z "$(tail -c1 "$f")" ] || return 1   # LF-terminated, no partial tail
    printf '%s' "$n"
}

# A holder of the peer's inbox lock, in the background: takes the lock, then
# runs $1 (`exec sleep 60` holds it; the frozen case writes half a line and
# stops itself). `exec` so the recorded pid IS the lock holder — a child that
# inherited fd 9 would keep the lock past the kill. `ready` means the lock is held; a
# case that needs the body's own writes first has the body stop itself (`kill -STOP $$`)
# and awaits `stopped`.
start_holder() {
    local body="$1"
    rm -f "${WORK:?}/ready"
    bash -c 'exec 9>> "$1/$2.lock"; flock 9; exec 8>> "$1/$2.jsonl"; touch "$3"; '"$body" \
        _ "$INBOX" "$PEER" "$WORK/ready" &
    HOLDER=$!
    HOLDERS+=("$HOLDER")
    await test -e "$WORK/ready"
}

. "$(dirname "${BASH_SOURCE[0]}")/hub_files/lock_shell.sh"
. "$(dirname "${BASH_SOURCE[0]}")/hub_files/routes.sh"
. "$(dirname "${BASH_SOURCE[0]}")/hub_files/wire.sh"
. "$(dirname "${BASH_SOURCE[0]}")/hub_files/reader.sh"
. "$(dirname "${BASH_SOURCE[0]}")/hub_files/lock_faults.sh"

check "the home guard refuses a live comm home, and only that" case_the_home_guard_refuses_a_live_comm_home
check "two writers through the lock give 400 whole lines" case_two_writers_give_400_whole_lines
check "a holder killed with -9 frees the lock and the send files" case_a_killed_holder_frees_the_lock_at_once
check "a frozen holder makes the send wait its bound and report FAILED, never filed" case_a_frozen_holder_makes_the_send_wait_then_fail
check "an append that fails under the lock is FAILED with its error" case_a_failed_write_is_failed_not_filed
check "a script whose lock identity equals the record appends locally" case_a_shared_nfs4_lock_manager_appends_locally
check "a two-line record whose line 1 matches appends locally" case_a_two_line_record_whose_line_1_matches_appends_locally
check "S2: a torn tail is cut back to the last newline and the new line is whole" case_a_torn_tail_is_cut_before_the_new_line
check "S2: a write cut short by the file-size limit is FAILED and leaves the file byte-identical" case_a_write_cut_short_leaves_the_file_byte_identical
check "S-a: a NUL-filled tail is cut, and a file of only NULs is cut to empty" case_a_nul_tail_is_cut_before_the_new_line
check "the Windows account is the SID from the first probe that prints one, and no probe fails loudly" case_the_windows_account_is_the_sid_from_the_first_probe_that_prints_one
check "a refused hello is named by sot_oneshot_request: the daemon's message on stderr, no reply, rc 1" case_a_refused_hello_is_named_by_the_oneshot_request
check "a hello refusal that is not about the protocol stops sot_oneshot_request at once, though the connection stays open" case_a_hello_refusal_stops_the_oneshot_request_at_once
check "a protocol refusal does not decide sot_oneshot_request: the request's own reply is returned" case_a_protocol_refusal_does_not_decide_the_oneshot_request
check "S4: a directed wire send with no daemon found is FAILED -> @h, exit 1" case_a_wire_send_with_no_daemon_is_failed
check "T5 (faked Windows): a send is one comm.file frame over the pipe, never a local append" case_a_windows_send_is_one_comm_file_over_the_pipe
check "a v3 record naming this machine's own none@<machine-id> appends locally" case_a_v3_record_naming_this_machine_appends_locally
check "v3 against another machine's record, unknown, a mismatched export, the hub's disk over NFS, no or a bare none record, and no flock(1) all go to the wire" case_anything_unproven_goes_to_the_wire
check "no perl on the PATH goes to the wire, never a local append" case_no_perl_goes_to_the_wire
check "the wire is this box's daemon, else the relay; one that does not answer is FAILED with no second route" case_the_wire_is_the_own_daemon_else_the_relay_and_only_one
check "the wire frame says whether the line was a broadcast copy" case_the_wire_frame_carries_the_broadcast_flag
check "the lock identity matches the fixture set the Rust test reads" case_the_lock_identity_matches_the_shared_fixtures
check "the wait is one number, 10, in both languages, and no lease constant is spelled" case_the_wait_is_one_number_and_no_lease_survives
check "T5: a wire send is one comm.file frame and prints the hub's answer; not_here alone falls back" case_a_wire_send_prints_the_hubs_answer
check "the comm.file read window outlasts the hub's lock wait: a line filed after it is filed" case_the_read_window_outlasts_the_hubs_lock_wait
check "T6: a hub-filed and a locally-filed line read alike and both advance the cursor" case_a_hub_line_and_a_local_line_read_alike

check "a dead writer's partial line is never counted; the next send cuts it and says so; nothing is skipped" case_a_dead_writers_partial_line_is_never_counted_and_is_cut
check "a frozen writer makes a poll and the end-of-turn hook say try again after their read bound; nothing is skipped" case_a_frozen_writer_makes_a_reader_try_again_never_skip_or_hang
check "stubbed fsync failure (shell arm), locked reader: waits, counts nothing, skips nothing" case_a_reader_on_the_shared_lock_never_counts_a_line_that_is_cut_back
check "stubbed fsync failure (shell arm), unlocked reader: steps back one line and skips nothing" case_an_unlocked_reader_steps_back_one_line_after_a_cut_back
check "the cursor takes a bare count, a ts and a hash, and a mismatch steps back exactly one line" case_the_cursor_takes_a_bare_count_a_ts_and_a_hash_and_steps_back_one

check "B-1: the reader's and the writer's lock descriptor are both opened read-write" case_every_inbox_lock_descriptor_is_read_write
check "B-2: the cursor hashes the line the reader held; a line filed after a cut-back is shown" case_a_cursor_hashes_the_line_it_read_not_the_one_filed_after
check "S-1: a hashed cursor one past the end steps back one; further past, or a bare count, gives 0" case_a_cursor_one_past_the_end_steps_back_one_and_further_gives_zero
check "S-2: a slow display does not hold off a writer" case_a_slow_display_does_not_hold_off_a_writer

check "the lock wait is chosen by lock kind: nfs4 polls, local and none@ block" case_the_lock_wait_is_chosen_by_lock_kind

check "S-A: a shared lock failing 71 is named on the poll's stdout, read unlocked, every line once; the hook blocks once" case_a_lock_error_71_is_named_and_the_inbox_read_unlocked
check "S-A: a shared lock failing 65 is named on the poll's stdout, read unlocked, every line once; the hook blocks once" case_a_lock_error_65_is_named_and_the_inbox_read_unlocked
check "S-A: a shared lock failing 65 with EIO is named as flock names it, never as a bad descriptor" case_a_lock_error_65_eio_is_named_as_flock_names_it
check "S-A: a lock file that will not open is named, read unlocked, every line once; the hook blocks once" case_a_lock_file_that_will_not_open_is_named
check "flock inside a subshell leaves the shared lock held in the calling shell" case_a_shared_lock_taken_inside_a_subshell_holds_in_the_caller
check "a lock fault blocks a marker turn once, unstamped, never in a continuation, once per session, and prefixes every nudge" case_a_lock_fault_blocks_a_marker_turn_once_and_prefixes_every_nudge
check "S-A: a real held lock is still exit 75 and being written, never a warning" case_a_held_lock_is_still_try_again
check "H3: a count that fails is a fault named once, never a zero" case_a_count_that_fails_is_a_fault_named_once_never_a_zero
check "S-B: a last line holding a NUL, ending in CR, or empty is shown once and never stepped back over" case_a_nul_a_cr_or_an_empty_last_line_is_shown_once

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
