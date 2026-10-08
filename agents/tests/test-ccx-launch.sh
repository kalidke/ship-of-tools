#!/usr/bin/env bash
# test-ccx-launch.sh — ccx's default handle is built from the comm library's own
# safe pieces (sot_sanitize_component, _sot_handle_host), so it is always a
# name workspace.create accepts and the host piece matches sot_derive_handle's.
# A derivation that cannot run stops the launch before the join and before codex.
# Runs the REAL ccx with a stub `codex` and a recording `comm-join.sh`.
#
# Usage: agents/tests/test-ccx-launch.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CCX="$(cd "$SCRIPT_DIR/../codex/bin" && pwd)/ccx"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-ccx-launch-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/commhome"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
trap 'rm -rf "${WORK:?}"' EXIT
STAGE="$(guard_stage_bin "$WORK")" || exit 2
# The comm home's bin is the staged folder itself (a symlink is a copy under MSYS, so edits to the stage would not reach it).
mkdir -p "$SOT_COMM_HOME" && mv "$STAGE" "$SOT_COMM_HOME/bin" && STAGE="$SOT_COMM_HOME/bin" || exit 2

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then
        echo "PASS: $desc"; PASS=$((PASS + 1))
    else
        echo "FAIL: $desc"; FAIL=$((FAIL + 1))
    fi
}

STUB_DIR="$WORK/stubbin"; FAIL_DIR="$WORK/failbin"
mkdir -p "$STUB_DIR" "$FAIL_DIR"
CODEX_LOG="$WORK/codex.log"; JOIN_LOG="$WORK/join.log"
cat > "$STUB_DIR/codex" <<'STUB'
#!/bin/sh
printf '%s\n' "$SOT_COMM_NAME" > "$CODEX_LOG_PATH"
STUB
chmod +x "$STUB_DIR/codex"
cat > "$STAGE/comm-join.sh" <<'STUB'
#!/bin/sh
printf '%s\n' "$2" > "$JOIN_LOG_PATH"
STUB
chmod +x "$STAGE/comm-join.sh"
for t in sha256sum shasum hostname; do
    printf '#!/bin/sh\nexit 1\n' > "$FAIL_DIR/$t"; chmod +x "$FAIL_DIR/$t"
done

# run_ccx DIRNAME HOST [EXTRA_PATH_DIR] : ccx in a fresh project dir, stdout/stderr to $WORK/out
run_ccx() {
    local proj="$WORK/projects/$1" host="$2" front="${3:-}"
    mkdir -p "$proj"
    rm -f "${CODEX_LOG:?}" "${JOIN_LOG:?}"
    (
        cd "$proj" || exit 1
        CODEX_LOG_PATH="$CODEX_LOG" JOIN_LOG_PATH="$JOIN_LOG" \
        PATH="${front:+$front:}$STUB_DIR:$PATH" \
        CODEX_HOME="$WORK/codex-home" \
        SOT_COMM_TEST_HOST="$host" \
        "$CCX" --fresh >"$WORK/out" 2>&1
    )
}

LONG_A="averyveryverylonghostname-one"
LONG_B="averyveryverylonghostname-two"
digest() { printf '%s' "$1" | sha256sum | cut -c1-6; }
name_ok() { [[ "$1" =~ ^[A-Za-z0-9._-]{1,64}$ ]]; }
codex_name() { cat "$CODEX_LOG" 2>/dev/null; }

case_unsafe_repo_and_long_host_give_a_safe_name() {
    run_ccx 'my repo; $(x) é' "$LONG_A" || { cat "$WORK/out"; return 1; }
    local n; n="$(codex_name)"
    name_ok "$n" || { echo "  unsafe or missing handle: '$n'"; return 1; }
    [ "$n" = "my-repo-x-cx-averyveryver-$(digest "$LONG_A")" ] \
        || { echo "  unexpected handle: '$n'"; return 1; }
    [ "$(cat "$JOIN_LOG")" = "$n" ] || { echo "  join named '$(cat "$JOIN_LOG")'"; return 1; }
}

case_hosts_differing_past_the_clamp_stay_distinct() {
    run_ccx same "$LONG_A" || return 1; local a; a="$(codex_name)"
    run_ccx same "$LONG_B" || return 1; local b; b="$(codex_name)"
    [ -n "$a" ] && [ "$a" != "$b" ] || { echo "  '$a' vs '$b'"; return 1; }
}

case_a_short_host_is_kept_as_is() {
    run_ccx proj short-box || return 1
    [ "$(codex_name)" = "proj-cx-short-box" ]
}

case_an_empty_component_becomes_x() {
    run_ccx -- short-box || return 1
    [ "$(codex_name)" = "x-cx-short-box" ]
}

case_the_host_piece_is_the_one_derive_handle_uses() {
    run_ccx proj "$LONG_A" || return 1
    local proj="$WORK/projects/proj" derived
    derived="$(SOT_COMM_HOME="$SOT_COMM_HOME" bash -c '. "$1/comm-lib.sh" && ensure_home && sot_derive_handle reclaim "$2" "$3" | head -n1' _ "$STAGE" "$proj" "$LONG_A")"
    local c; c="$(codex_name)"
    [ -n "$derived" ] && [ "${derived#proj-}" = "${c#proj-cx-}" ] || { echo "  derive '$derived' vs ccx '$c'"; return 1; }
}

case_an_explicit_pin_wins_verbatim() {
    local proj="$WORK/projects/pin"; mkdir -p "$proj"; rm -f "${CODEX_LOG:?}" "${JOIN_LOG:?}"
    (
        cd "$proj" || exit 1
        CODEX_LOG_PATH="$CODEX_LOG" JOIN_LOG_PATH="$JOIN_LOG" PATH="$STUB_DIR:$PATH" \
        CODEX_HOME="$WORK/codex-home" SOT_COMM_NAME="my pinned.name" "$CCX" --fresh >/dev/null 2>&1
    )
    [ "$(codex_name)" = "my pinned.name" ]
}

refused_before_join_and_codex() {  # extra stderr pattern in $WORK/out
    [ ! -e "$CODEX_LOG" ] && [ ! -e "$JOIN_LOG" ] && grep -q "cannot derive the default handle" "$WORK/out"
}

case_a_failing_digest_stops_the_launch() {
    run_ccx proj "$LONG_A" "$FAIL_DIR" && { echo "  ccx exited 0"; return 1; }
    refused_before_join_and_codex
}

case_a_failing_host_read_stops_the_launch() {
    run_ccx proj "" "$FAIL_DIR" && { echo "  ccx exited 0"; return 1; }
    refused_before_join_and_codex
}

case_a_missing_library_stops_the_launch() {
    mv "$STAGE/comm-lib.sh" "$STAGE/comm-lib.sh.away" || return 1
    run_ccx proj short-box; local rc=$?
    mv "$STAGE/comm-lib.sh.away" "$STAGE/comm-lib.sh"
    [ "$rc" -ne 0 ] || { echo "  ccx exited 0"; return 1; }
    refused_before_join_and_codex
}

check "an unsafe repo name and a long host give a name workspace.create accepts, and the join uses it" case_unsafe_repo_and_long_host_give_a_safe_name
check "two hosts that differ only past the clamp get different handles" case_hosts_differing_past_the_clamp_stay_distinct
check "a short host is kept as is" case_a_short_host_is_kept_as_is
check "a repo name with nothing usable becomes x" case_an_empty_component_becomes_x
check "ccx and sot_derive_handle build the same host piece" case_the_host_piece_is_the_one_derive_handle_uses
check "an explicit SOT_COMM_NAME wins verbatim" case_an_explicit_pin_wins_verbatim
check "a failing digest stops the launch before the join and codex" case_a_failing_digest_stops_the_launch
check "a failing host read stops the launch before the join and codex" case_a_failing_host_read_stops_the_launch
check "a missing comm library stops the launch before the join and codex" case_a_missing_library_stops_the_launch

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
