#!/usr/bin/env bash
# test-ccx-launch.sh — ccx chooses no handle (decision 0031): it joins with no name, and codex's first prompt names
# the handle comm-context reads back, whatever SOT_COMM_NAME says. A join that gives no handle still starts codex,
# with a prompt that names none and one stderr line saying so.
# Runs the REAL ccx with a stub `codex`, a recording `comm-join.sh` and a `comm-context.sh` that names a handle.
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

STUB_DIR="$WORK/stubbin"
mkdir -p "$STUB_DIR"
CODEX_LOG="$WORK/codex.log"; JOIN_LOG="$WORK/join.log"
cat > "$STUB_DIR/codex" <<'STUB'
#!/bin/sh
for a in "$@"; do printf '%s\n' "$a"; done > "$CODEX_LOG_PATH"
STUB
chmod +x "$STUB_DIR/codex"
cat > "$STAGE/comm-join.sh" <<'STUB'
#!/bin/sh
printf 'args=%s\n' "$*" > "$JOIN_LOG_PATH"
STUB
chmod +x "$STAGE/comm-join.sh"
# comm-context.sh's eval lines, naming CONTEXT_NAME (empty: the join gave no handle).
cat > "$STAGE/comm-context.sh" <<'STUB'
#!/bin/sh
printf 'NAME=%s\n' "$CONTEXT_NAME"
STUB
chmod +x "$STAGE/comm-context.sh"

# run_ccx NAME [ENV=VALUE...] : ccx in a fresh project dir, comm-context naming NAME; stderr to $WORK/err
run_ccx() {
    local name="$1" proj="$WORK/projects/p"
    shift
    mkdir -p "$proj"
    rm -f "${CODEX_LOG:?}" "${JOIN_LOG:?}"
    (
        cd "$proj" || exit 1
        env CODEX_LOG_PATH="$CODEX_LOG" JOIN_LOG_PATH="$JOIN_LOG" CONTEXT_NAME="$name" \
            PATH="$STUB_DIR:$PATH" CODEX_HOME="$WORK/codex-home" "$@" \
            "$CCX" --fresh >/dev/null 2>"$WORK/err"
    )
}

case_the_join_names_no_handle_and_the_prompt_names_the_sessions() {
    run_ccx proj-box1 || { cat "$WORK/err"; return 1; }
    [ "$(cat "$JOIN_LOG")" = "args=" ] || { echo "  join: $(cat "$JOIN_LOG")"; return 1; }
    grep -q '^You are the comm-aware agent @proj-box1 on the sot-comm network\.' "$CODEX_LOG" \
        || { echo "  prompt: $(head -c 200 "$CODEX_LOG")"; return 1; }
}

case_sot_comm_name_chooses_nothing() {
    run_ccx proj-box1 SOT_COMM_NAME=chosen-name || return 1
    [ "$(cat "$JOIN_LOG")" = "args=" ] && grep -q '@proj-box1 ' "$CODEX_LOG" && ! grep -q chosen-name "$CODEX_LOG"
}

case_no_handle_still_starts_codex_and_says_so() {
    run_ccx "" || return 1
    grep -q '^You are the comm-aware agent on the sot-comm network\.' "$CODEX_LOG" \
        && grep -q 'gave this session no handle' "$WORK/err"
}

check "ccx joins with no name and the first prompt names the handle comm-context reads" case_the_join_names_no_handle_and_the_prompt_names_the_sessions
check "SOT_COMM_NAME chooses nothing" case_sot_comm_name_chooses_nothing
check "a join that gives no handle still starts codex and says so" case_no_handle_still_starts_codex_and_says_so

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
