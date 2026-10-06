#!/usr/bin/env bash
# Native proof controller: every child is owned by ISO; no live service is contacted.
set -euo pipefail
umask 022
mode=${1:?local, linux, macos, windows or checkpoint}
proof_root=${2:?proof tree}
head_sha=${3:?A1c identity}
proof_root=$(python3 -c 'from pathlib import Path; import sys; print(Path(sys.argv[1]).resolve().as_posix())' "$proof_root")
mkdir -p "${CARGO_TARGET_DIR:?}/logs/a1c"
run() {
    local label=$1 tree=$2 expected=$3 needle=$4 rc
    shift 4
    local log="$CARGO_TARGET_DIR/logs/a1c/$label.log"
    set +e
    ( cd "$proof_root/$tree/rust" && CARGO_NET_OFFLINE=true CARGO_PROFILE_DEV_DEBUG=line-tables-only GIT_CEILING_DIRECTORIES="$proof_root" SOT_BUILD_ID="a1c-$head_sha-$tree" nice -n 10 timeout 9m cargo "$@" ) > "$log" 2>&1
    rc=$?
    set -e
    printf '%s exit %s expected %s\n' "$label" "$rc" "$expected"
    [[ "$rc" == "$expected" ]] || { tail -n 50 "$log"; return 1; }
    grep -Fq -- "$needle" "$log" || { tail -n 50 "$log"; return 1; }
    if [[ "$expected" == 101 ]]; then
        grep -Fq 'test result: FAILED' "$log" || return 1
        grep -Fq '1 failed' "$log" || return 1
    else
        grep -Eq 'test result: ok\. [1-9][0-9]* passed' "$log" || return 1
    fi
    grep -E 'test result:|fixture-finalization |fixture-readiness |fixture-entry |proof-observation |proof-recovery |entry-proof |child-status=|intended child diagnostic|checkpoint-proof |stack backtrace:' "$log" || true
}
iso() {
    run "$1" "$2" "$3" "$4" test -p sot-log --lib -j 8 --locked -- "test_isolated::tests::$5" --exact --nocapture
}
socket() {
    run "$1" "$2" "$3" "$4" test -p sot-log --test socket_unix -j 8 --locked -- "diagnostics::$5" --exact --nocapture
}
if [[ "$mode" == checkpoint ]]; then
    for iteration in $(seq 1 300); do
        run "checkpoint-$iteration" head 0 'test result: ok. 1 passed;' test -p sot-log --test socket_unix -j 8 --locked -- close::server_close_yields_client_eof_and_client_drop_yields_server_closed --exact --nocapture
        printf 'checkpoint iteration=%s sha=%s status=0\n' "$iteration" "$head_sha"
    done
    run checkpoint-siblings head 0 'test result: ok.' test -p sot-log --test socket_unix -j 8 --locked -- --nocapture
    # An assertion-red child must retain the hosted job's RUST_BACKTRACE=1.
    iso checkpoint-backtrace head 0 'intended child diagnostic' invalid_utf8_stderr_preserves_the_child_diagnostic
    grep -Fq 'intended child failure' "$CARGO_TARGET_DIR/logs/a1c/checkpoint-backtrace.log"
    grep -Fq 'stack backtrace:' "$CARGO_TARGET_DIR/logs/a1c/checkpoint-backtrace.log"
    printf 'backtrace-proof sha=%s failing-child-stderr=observed\n' "$head_sha"
    printf 'checkpoint sha=%s completed=300 cause=open\n' "$head_sha"
    exit 0
fi
for tree in parent-ready head revert-ready; do
    expected=101; needle='readiness failure bypassed owned-child finalization'
    if [[ "$tree" == head ]]; then expected=0; needle='test result: ok. 1 passed;'; fi
    iso "$tree-ready-iso" "$tree" "$expected" "$needle" readiness_failure_finishes_owned_child_checks
    if [[ "$mode" != windows ]]; then
        socket "$tree-ready-socket" "$tree" "$expected" "$needle" readiness_failure_finishes_owned_child_checks
    fi
done
for tree in parent-utf8 head revert-utf8; do
    expected=101; needle='child output lost escaped invalid UTF-8 diagnostic'
    if [[ "$tree" == head ]]; then expected=0; needle='intended child diagnostic \xFF\xFE\xC3('; fi
    iso "$tree-utf8" "$tree" "$expected" "$needle" invalid_utf8_stderr_preserves_the_child_diagnostic
    if [[ "$tree" != head ]]; then
        grep -Fq "reading the child's stderr: stream did not contain valid UTF-8" "$CARGO_TARGET_DIR/logs/a1c/$tree-utf8.log"
    fi
done
run iso-suite head 0 'test result: ok.' test -p sot-log --lib -j 8 --locked -- test_isolated::tests --nocapture
if [[ "$mode" == local ]]; then exit 0; fi
if [[ "$mode" != windows ]]; then
    for tree in parent-history head revert-history; do
        expected=101; needle='wait diagnostic missing emitted progress snapshot'
        if [[ "$tree" == head ]]; then expected=0; needle='test result: ok. 1 passed;'; fi
        for case in absent_event_reports_named_wait unrelated_event_reports_named_wait; do
            socket "$tree-$case" "$tree" "$expected" "$needle" "$case"
        done
    done
    socket history-negative-output head 0 'test result: ok. 1 passed;' history_blocks_reject_missing_or_malformed_output
    run recorder-controls head 0 'test result: ok.' test -p sot-log --lib -j 8 --locked -- lane::test_progress::tests --nocapture
    run recorder-real-transport head 0 'test result: ok. 1 passed;' test -p sot-log --lib -j 8 --locked -- lane::socket_unix::server::tests::transport_progresses_while_recorder_is_busy --exact --nocapture
    run socket-diagnostics head 0 'test result: ok.' test -p sot-log --test socket_unix -j 8 --locked -- diagnostics:: --nocapture
    run socket-siblings head 0 'test result: ok.' test -p sot-log --test socket_unix -j 8 --locked -- --nocapture
fi
# Preserve A1b's actual listener owners on the A1c merge candidate.
for case in a_connection_the_owner_check_refuses_reaches_no_handler foreign_or_unknown_owner_is_refused_before_handler; do
    run "page-$case" head 0 'test result: ok. 1 passed;' test -p sot-log --lib -j 8 --locked -- "identity::peer_owner::tests::$case" --exact --nocapture
done
if [[ "$mode" == windows ]]; then
    for case in session_pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags session_pipe_denies_a_token_without_the_owner_grant; do
        run "$case" head 0 'test result: ok. 1 passed;' test -p sot-backend --bin sotd -j 8 --locked -- "server::listen::tests::$case" --exact --nocapture
    done
    run pipe-siblings head 0 'test result: ok.' test -p sot-log --test pipe_win -j 8 --locked -- --nocapture
else
    for case in a_refused_os_peer_reaches_no_connection_handler admit_peer_admits_this_process_with_its_pid_and_creation_time; do
        run "$case" head 0 'test result: ok. 1 passed;' test -p sot-backend --bin sotd -j 8 --locked -- "server::listen::tests::$case" --exact --nocapture
    done
    if [[ "$mode" == macos ]]; then
        run challenge-macos head 0 'test result: ok.' test -p sot-log --test challenge_macos -j 8 --locked -- --nocapture
        run macos-kernel head 0 'test result: ok.' test -p sot-log --test macos_kernel_facts -j 8 --locked -- --nocapture
    else
        run challenge-linux head 0 'test result: ok.' test -p sot-log --test challenge_unix -j 8 --locked -- --nocapture
    fi
fi
printf 'matrix sha=%s platform=%s complete\n' "$head_sha" "$mode"
