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
    local desc="$1"
    shift
    if "$@"; then
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
if [ "$*" = --help ] || [ "$*" = '--no-daemon --help' ]; then
    echo "$# $*" >> "$CODEX_LOG_PATH.help"
    echo 'probe stdout'
    echo 'probe stderr' >&2
    if [ "$*" = --help ]; then
        printf '%s\n' '  --other-option  Use this when' '                  --no-daemon is unavailable.'
        exit 0
    fi
    case "${PROBE_RESULT:-accept}" in
        reject) exit 2 ;;
        failed) exit 1 ;;
        signal) kill -TERM "$$" ;;
    esac
    [ -z "${REPLACE_AFTER_HELP:-}" ] || mv "$REPLACE_AFTER_HELP" "$0" || exit 1
    exit 0
fi
for a in "$@"; do printf '%s\n' "$a"; done > "$CODEX_LOG_PATH"
printf '%s\n' "${SOT_SELECTION_FILE-<unset>}" > "$CODEX_LOG_PATH.sel"
printf '%s\n' "${SOT_CODEX_THREAD_FILE-<unset>}" > "$CODEX_LOG_PATH.binding"
[ ! -f "${OBSERVE_MARKER:-}" ] || printf 'marked\n' > "$CODEX_LOG_PATH.marked"
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
    mkdir -p "$proj" "$WORK/codex-home"
    rm -f "${CODEX_LOG:?}" "$CODEX_LOG.help" "$CODEX_LOG.marked" "${JOIN_LOG:?}"
    (
        cd "$proj" || exit 1
        env CODEX_LOG_PATH="$CODEX_LOG" JOIN_LOG_PATH="$JOIN_LOG" CONTEXT_NAME="$name" \
            PATH="$STUB_DIR:$PATH" CODEX_HOME="$WORK/codex-home" "$@" \
            bash "$CCX" --fresh ${CCX_FLAG-} --selection-marker "$MARKER" >"$WORK/out" 2>"$WORK/err"
    )
}

MARKER="$WORK/marker outside row"
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
        && [ "$(grep -Fxc -- '--no-alt-screen' "$CODEX_LOG")" = 1 ] \
        && grep -q 'gave this session no handle' "$WORK/err"
}

case_missing_codex_opens_a_shell() {
    local bin="$WORK/shell-only" runner
    runner=$(command -v bash) || return 1
    mkdir -p "$bin" || return 1
    cat > "$bin/bash" <<'STUB'
#!/bin/sh
printf 'shell argc=%s\n' "$#"
STUB
    chmod +x "$bin/bash" || return 1
    HOME="$WORK/empty-home" PATH="$bin" "$runner" "$CCX" --capsule >"$WORK/out" 2>"$WORK/err" || return 1
    [ "$(cat "$WORK/out")" = "shell argc=0" ] && grep -q 'ccx: codex not found' "$WORK/err"
}

case_a_capsule_leg_keeps_its_rows_selection_file() {
    CCX_FLAG=--capsule run_ccx proj-box1 SOT_SELECTION_FILE=/row/selection.json || return 1
    [ "$(cat "$CODEX_LOG.sel")" = "/row/selection.json" ] || { echo "  codex saw: $(cat "$CODEX_LOG.sel")"; return 1; }
}

case_a_ccx_started_inside_a_row_drops_the_rows_selection_file() {
    run_ccx proj-box1 SOT_SELECTION_FILE=/row/selection.json || return 1
    [ "$(cat "$CODEX_LOG.sel")" = "<unset>" ] || { echo "  codex saw: $(cat "$CODEX_LOG.sel")"; return 1; }
}

case_fresh_row_enables_its_skills() {
    run_ccx proj-box1 && awk 'last == "-c" && $0 == "plugins.sot-row@sot-local.enabled=true" { n++ }
        { last = $0 } END { exit n != 1 }' "$CODEX_LOG"
}

check "a fresh ccx enables the row skills once" case_fresh_row_enables_its_skills
check "ccx joins with no name and the first prompt names the handle comm-context reads" case_the_join_names_no_handle_and_the_prompt_names_the_sessions
check "SOT_COMM_NAME chooses nothing" case_sot_comm_name_chooses_nothing
check "a join that gives no handle still starts Codex inline and reports the missing handle" case_no_handle_still_starts_codex_and_says_so
check "a missing Codex reports the cause and opens a shell with no arguments" case_missing_codex_opens_a_shell
check "ccx --capsule keeps its row's selection file" case_a_capsule_leg_keeps_its_rows_selection_file
check "a ccx started without --capsule drops the selection file it inherited" case_a_ccx_started_inside_a_row_drops_the_rows_selection_file

case_nested_drops_binding() {
    run_ccx proj-box1 SOT_CODEX_THREAD_FILE="$WORK/outer binding" &&
        [ "$(cat "$CODEX_LOG.binding")" = '<unset>' ]
}
check "a hand-run ccx drops an inherited Codex binding" case_nested_drops_binding

case_marker_condition() (
    local change="$1" cache="$WORK/codex-home/plugins/cache/sot-local/sot-row/0.1.0"
    local later="$cache/../0.2.0" probe=accept replacement= flag_count=1
    local src="$SCRIPT_DIR/../codex/row-hooks.json" script="$STAGE/sot-selection-hook.sh" path="$WORK/row selection"
    mkdir -p "$cache/hooks" "$cache/.codex-plugin" || exit 1
    cp "$src" "$cache/hooks/hooks.json" || exit 1
    printf '%s\n' '{"name":"sot-row","version":"0.1.0","hooks":"./hooks/hooks.json"}' > "$cache/.codex-plugin/plugin.json"
    chmod +x "$script" || exit 1
    rm -f "$MARKER"
    CCX_FLAG=--capsule
    case "$change" in
        path) path= ;;
        override) sed "s/ -c 'plugins.sot-row@sot-local.enabled=true'\$//" "$CCX" > "$WORK/ccx-no-override"; CCX="$WORK/ccx-no-override" ;;
        script) chmod -x "$script" ;;
        command) jq '.hooks.UserPromptSubmit[0].hooks[0].command += " || true"' "$src" > "$cache/hooks/hooks.json" ;;
        binding-command) jq '.hooks.SessionStart[0].hooks[0].command += " || true"' "$src" > "$cache/hooks/hooks.json" ;;
        undeclared) printf '{}' > "$cache/.codex-plugin/plugin.json" ;;
        extra-version) cp -R "$cache" "$later" &&
            jq '.version = "0.2.0"' "$cache/.codex-plugin/plugin.json" > "$later/.codex-plugin/plugin.json" || exit 1 ;;
        missing-hook) rm "$cache/hooks/hooks.json" ;;
        malformed) printf '{' > "$cache/hooks/hooks.json" ;;
        hand) CCX_FLAG= ;;
        unwritable) MARKER="$WORK/missing-parent/marker" ;;
        old-codex) probe=reject; flag_count=0 ;;
        failed-help) probe=failed; flag_count=0 ;;
        signaled-probe) probe=signal; flag_count=0 ;;
        argv-no-flag) sed 's/^    set -- "\$@" --no-daemon$/    :/' "$CCX" > "$WORK/ccx-no-flag" || exit 1
            CCX="$WORK/ccx-no-flag"; flag_count=0 ;;
        failed-exec) cp "$STUB_DIR/codex" "$WORK/good-codex" || exit 1
            replacement="$WORK/broken-codex"
            printf '#!/no/such/interpreter\n' > "$replacement" && chmod +x "$replacement" || exit 1 ;;
    esac
    run_ccx proj-box1 SOT_SELECTION_FILE="$path" OBSERVE_MARKER="$MARKER" PROBE_RESULT="$probe" REPLACE_AFTER_HELP="$replacement"
    local status=$?
    chmod +x "$script"
    [ "$change" != extra-version ] || rm -rf -- "$later" || exit 1
    [ "$(cat "$CODEX_LOG.help")" = '2 --no-daemon --help' ] || exit 1
    if [ "$change" != failed-exec ]; then
        [ -f "$CODEX_LOG" ] && [ "$(grep -Fxc -- --no-daemon "$CODEX_LOG")" = "$flag_count" ] || exit 1
    fi
    if [ "$change" = failed-exec ]; then
        mv "$WORK/good-codex" "$STUB_DIR/codex"
        [ "$status" -ne 0 ] && [ ! -e "$MARKER" ] && [ -s "$WORK/err" ]
    elif [ "$change" = fresh ]; then
        [ "$status" = 0 ] && [ -f "$MARKER" ] && [ ! -s "$MARKER" ] && [ -f "$CODEX_LOG.marked" ]
    else
        [ "$status" = 0 ] && [ -f "$CODEX_LOG" ] && [ ! -e "$MARKER" ] && [ ! -e "$CODEX_LOG.marked" ]
    fi
)
for condition in fresh path override script command binding-command undeclared extra-version missing-hook malformed hand unwritable old-codex failed-help signaled-probe argv-no-flag failed-exec; do
    check "selection marker at native exec ($condition)" case_marker_condition "$condition"
done

case_probe_is_quiet() {
    local result
    for result in accept reject signal; do
        run_ccx proj-box1 PROBE_RESULT="$result" && [ -f "$CODEX_LOG" ] &&
            [ ! -s "$WORK/out" ] && [ ! -s "$WORK/err" ] || { cat "$WORK/out" "$WORK/err"; return 1; }
    done
}
check "a successful, rejected or signaled probe prints nothing to the terminal" case_probe_is_quiet

case_native_cannot_run() (
    mv "$STUB_DIR/codex" "$WORK/good-codex" || exit 1
    trap 'mv "$WORK/good-codex" "$STUB_DIR/codex"' EXIT
    printf '#!/no/such/interpreter\n' > "$STUB_DIR/codex" && chmod +x "$STUB_DIR/codex" || exit 1
    rm -f "$MARKER"
    # The native exec should give its ordinary diagnostic, once. The probe can also fail to exec.
    bash -c 'exec "$1"' _ "$STUB_DIR/codex" 2>"$WORK/native-err"
    local status=$?
    [ "$status" -ne 0 ] || exit 1
    CCX_FLAG=--capsule run_ccx proj-box1 SOT_SELECTION_FILE="$WORK/selection"
    [ "$?" = "$status" ] && [ ! -e "$MARKER" ] && [ -s "$WORK/err" ] &&
        [ "$(grep -Fc "$STUB_DIR/codex" "$WORK/err")" = "$(grep -Fc "$STUB_DIR/codex" "$WORK/native-err")" ]
)
check "an unrunnable native binary reports the launch failure once and leaves no marker" case_native_cannot_run

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
