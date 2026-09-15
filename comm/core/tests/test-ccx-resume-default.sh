#!/usr/bin/env bash
# test-ccx-resume-default.sh — ccx's resume default is keyed on the
# `--capsule` ARGV flag alone, never an inherited env var. Runs the REAL
# ccx as a subprocess with a stub `codex` on PATH recording its argv.
#
# Usage: comm/core/tests/test-ccx-resume-default.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CCX="$(cd "$SCRIPT_DIR/../../adapters/codex/bin" && pwd)/ccx"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-ccx-resume-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
trap 'rm -rf "$WORK"' EXIT

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

# A `codex` stub: records its own argv, one per line, and exits 0.
STUB_DIR="$WORK/stubbin"
mkdir -p "$STUB_DIR"
ARGV_LOG="$WORK/argv.log"
cat > "$STUB_DIR/codex" <<'EOF'
#!/bin/sh
: > "$ARGV_LOG_PATH"
for a in "$@"; do
    printf '%s\n' "$a" >> "$ARGV_LOG_PATH"
done
EOF
chmod +x "$STUB_DIR/codex"

# A project dir with one rollout recorded for it, for the scan to find.
PROJECT_DIR="$WORK/project"
mkdir -p "$PROJECT_DIR"
CODEX_HOME_DIR="$WORK/codex-home"
mkdir -p "$CODEX_HOME_DIR/sessions/2026/09/14"
ROLLOUT_ID="11111111-1111-1111-1111-111111111111"
printf '{"id":"%s","cwd":"%s"}\n' "$ROLLOUT_ID" "$PROJECT_DIR" \
    > "$CODEX_HOME_DIR/sessions/2026/09/14/rollout-1.jsonl"

# An empty $SOT_COMM_HOME (no comm-join.sh) skips ccx's comm bootstrap.
EMPTY_COMM_HOME="$WORK/empty-comm-home"
mkdir -p "$EMPTY_COMM_HOME/bin"

# A comm home WITH stub comm-join.sh + codex-watch.sh (argv-recording),
# for proving --capsule starts the watcher in capsule mode (no pane arg).
CAPSULE_COMM_HOME="$WORK/capsule-comm-home"
mkdir -p "$CAPSULE_COMM_HOME/bin"
cat > "$CAPSULE_COMM_HOME/bin/comm-join.sh" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$CAPSULE_COMM_HOME/bin/comm-join.sh"
WATCH_ARGV_LOG="$WORK/watch-argv.log"
cat > "$CAPSULE_COMM_HOME/bin/codex-watch.sh" <<'EOF'
#!/bin/sh
: > "$WATCH_ARGV_LOG_PATH"
for a in "$@"; do
    printf '%s\n' "$a" >> "$WATCH_ARGV_LOG_PATH"
done
EOF
chmod +x "$CAPSULE_COMM_HOME/bin/codex-watch.sh"

run_ccx() {  # extra ccx args...
    (
        cd "$PROJECT_DIR" || exit 1
        ARGV_LOG_PATH="$ARGV_LOG" \
        HOME="$WORK/empty-home" \
        PATH="$STUB_DIR:$PATH" \
        SOT_COMM_HOME="$EMPTY_COMM_HOME" \
        SOT_COMM_NAME="ccx-resume-test" \
        CODEX_HOME="$CODEX_HOME_DIR" \
        "$CCX" "$@" >/dev/null 2>"$WORK/stderr.log"
    )
}

resumed_this_id() {  # -> 0 iff argv.log's 2nd line is "resume" and 3rd is $ROLLOUT_ID
    [ -f "$ARGV_LOG" ] || { echo "  codex stub never ran"; return 1; }
    [ "$(sed -n 1p "$ARGV_LOG")" = "resume" ] && [ "$(sed -n 2p "$ARGV_LOG")" = "$ROLLOUT_ID" ]
}

started_fresh() {  # -> 0 iff argv.log's 1st line is NOT "resume"
    [ -f "$ARGV_LOG" ] || { echo "  codex stub never ran"; return 1; }
    [ "$(sed -n 1p "$ARGV_LOG")" != "resume" ]
}

case_row_env_resumes_by_default() {
    rm -f "$ARGV_LOG"
    (
        cd "$PROJECT_DIR" || exit 1
        ARGV_LOG_PATH="$ARGV_LOG" \
        HOME="$WORK/empty-home" \
        PATH="$STUB_DIR:$PATH" \
        SOT_COMM_HOME="$EMPTY_COMM_HOME" \
        SOT_COMM_NAME="ccx-resume-test" \
        CODEX_HOME="$CODEX_HOME_DIR" \
        SOT_WORKSPACE_ID="ws-row-1" \
        "$CCX" >/dev/null 2>"$WORK/stderr.log"
    )
    resumed_this_id
}

case_hand_run_ccx_with_no_env_resumes_by_default() {
    rm -f "$ARGV_LOG"
    run_ccx
    resumed_this_id
}

# A real capsule leg's full inherited env, minus --capsule -- plus a
# stale SOT_RUNTIME=capsule, which must have ZERO effect (ccx no longer reads it).
case_hand_run_ccx_inside_a_capsule_row_keeps_resume_default() {
    rm -f "$ARGV_LOG"
    (
        cd "$PROJECT_DIR" || exit 1
        ARGV_LOG_PATH="$ARGV_LOG" \
        HOME="$WORK/empty-home" \
        PATH="$STUB_DIR:$PATH" \
        SOT_COMM_HOME="$EMPTY_COMM_HOME" \
        SOT_COMM_NAME="ccx-resume-test" \
        SOT_COMM_SELF_FILE="$EMPTY_COMM_HOME/self/host__ws-capsule-row-1.txt" \
        CODEX_HOME="$CODEX_HOME_DIR" \
        SOT_WORKSPACE_ID="ws-capsule-row-1" \
        SOT_RUNTIME="capsule" \
        "$CCX" >/dev/null 2>"$WORK/stderr.log"
    )
    resumed_this_id
}

case_capsule_flag_bare_ccx_starts_fresh() {
    rm -f "$ARGV_LOG"
    run_ccx --capsule
    started_fresh
}

case_capsule_flag_with_continue_resumes() {
    rm -f "$ARGV_LOG"
    run_ccx --capsule --continue
    resumed_this_id
}

case_capsule_flag_with_fresh_flag_stays_fresh_even_with_continue() {
    rm -f "$ARGV_LOG"
    run_ccx --capsule --continue --fresh
    started_fresh
}

case_capsule_flag_starts_the_capsule_watcher() {
    rm -f "$WATCH_ARGV_LOG"
    (
        cd "$PROJECT_DIR" || exit 1
        ARGV_LOG_PATH="$ARGV_LOG" \
        HOME="$WORK/empty-home" \
        PATH="$STUB_DIR:$PATH" \
        SOT_COMM_HOME="$CAPSULE_COMM_HOME" \
        SOT_COMM_NAME="ccx-resume-test" \
        WATCH_ARGV_LOG_PATH="$WATCH_ARGV_LOG" \
        CODEX_HOME="$CODEX_HOME_DIR" \
        TMUX_PANE="" \
        "$CCX" --capsule >/dev/null 2>"$WORK/stderr.log"
    )
    # nohup-launched: may still be writing just after ccx exec's the codex stub.
    local n=0
    while [ ! -f "$WATCH_ARGV_LOG" ] && [ "$n" -lt 20 ]; do
        sleep 0.1
        n=$((n + 1))
    done
    [ -f "$WATCH_ARGV_LOG" ] || { echo "  codex-watch.sh stub never ran"; return 1; }
    [ "$(sed -n 1p "$WATCH_ARGV_LOG")" = "ccx-resume-test" ] || { echo "  argv[1] != handle: $(cat "$WATCH_ARGV_LOG")"; return 1; }
    [ "$(wc -l < "$WATCH_ARGV_LOG")" -eq 1 ] || { echo "  a second arg was passed, want the handle only: $(cat "$WATCH_ARGV_LOG")"; return 1; }
    return 0
}

case_capsule_flag_starts_the_capsule_watcher_even_with_a_leaked_pane_var() {
    rm -f "$WATCH_ARGV_LOG"
    (
        cd "$PROJECT_DIR" || exit 1
        ARGV_LOG_PATH="$ARGV_LOG" \
        HOME="$WORK/empty-home" \
        PATH="$STUB_DIR:$PATH" \
        SOT_COMM_HOME="$CAPSULE_COMM_HOME" \
        SOT_COMM_NAME="ccx-resume-test" \
        WATCH_ARGV_LOG_PATH="$WATCH_ARGV_LOG" \
        CODEX_HOME="$CODEX_HOME_DIR" \
        TMUX_PANE="%99" \
        "$CCX" --capsule >/dev/null 2>"$WORK/stderr.log"
    )
    local n=0
    while [ ! -f "$WATCH_ARGV_LOG" ] && [ "$n" -lt 20 ]; do
        sleep 0.1
        n=$((n + 1))
    done
    [ -f "$WATCH_ARGV_LOG" ] || { echo "  codex-watch.sh stub never ran"; return 1; }
    [ "$(wc -l < "$WATCH_ARGV_LOG")" -eq 1 ] || { echo "  a second arg was passed despite --capsule: $(cat "$WATCH_ARGV_LOG")"; return 1; }
    return 0
}

check "a row env (SOT_WORKSPACE_ID set, no --capsule) resumes by default" case_row_env_resumes_by_default
check "a hand-run ccx with no SOT_* env at all keeps resuming by default" case_hand_run_ccx_with_no_env_resumes_by_default
check "a hand-run ccx inside a capsule row's env, without --capsule, keeps its resume default" case_hand_run_ccx_inside_a_capsule_row_keeps_resume_default
check "--capsule: a bare ccx starts fresh" case_capsule_flag_bare_ccx_starts_fresh
check "--capsule: --continue triggers the resume scan" case_capsule_flag_with_continue_resumes
check "--capsule: --fresh wins even alongside --continue" case_capsule_flag_with_fresh_flag_stays_fresh_even_with_continue
check "--capsule starts codex-watch.sh in capsule mode (no pane arg)" case_capsule_flag_starts_the_capsule_watcher
check "--capsule starts the capsule watcher even with a leaked TMUX_PANE" case_capsule_flag_starts_the_capsule_watcher_even_with_a_leaked_pane_var

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
