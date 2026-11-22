#!/usr/bin/env bash
# test-selection-hook.sh — sot-selection-hook.sh prints the file $SOT_SELECTION_FILE names when it is a readable regular
# file, prints nothing in every other case, and always exits 0 with nothing on stderr. It is run from the staged bin, the
# comm scripts laid out flat as the installer lays them, which also proves it lands there executable. The shell is
# whichever bash this suite runs under: the hosted runs of `scripts/tests/under-bash.sh` give it bash 3.2 on macOS and
# on Linux, and bash 5 runs it here; no run tests it under Git Bash on Windows (the script is plain `[ -f ]`, `[ -r ]`
# and `cat`, and is read, not run, there).
#
# HERMETIC: its own HOME and comm folder; no daemon, no agent.
#
# Usage: agents/tests/test-selection-hook.sh     Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

WORK="$(mktemp -d "${TMPDIR:-/tmp}/selection-hook-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi
WORK="$(cd "$WORK" && pwd -P)" || exit 1
trap 'chmod -R u+rw "$WORK" 2> /dev/null; rm -rf "${WORK:?}"' EXIT
guard_fresh_home "$WORK"
BIN="$(guard_stage_bin "$WORK")" || exit 2
HOOK="$BIN/sot-selection-hook.sh"

PASS=0
FAIL=0
check() {
    local desc="$1"; shift
    if "$@"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}

# Runs the hook with SOT_SELECTION_FILE set to $1 (not set at all when the argument is absent); out in $WORK/out, errors
# in $WORK/err, status in $RC.
run_hook() {
    if [ "$#" -gt 0 ]; then
        SOT_SELECTION_FILE="$1" bash "$HOOK" > "$WORK/out" 2> "$WORK/err" < /dev/null
    else
        env -u SOT_SELECTION_FILE bash "$HOOK" > "$WORK/out" 2> "$WORK/err" < /dev/null
    fi
    RC=$?
}

# The hook printed nothing on either stream and exited 0.
silent() {
    [ "$RC" -eq 0 ] || { echo "  exit $RC"; return 1; }
    [ ! -s "$WORK/out" ] || { echo "  stdout: $(cat "$WORK/out")"; return 1; }
    [ ! -s "$WORK/err" ] || { echo "  stderr: $(cat "$WORK/err")"; return 1; }
}

case_the_script_is_staged_executable() { [ -x "$HOOK" ]; }
check "the script is laid flat in the staged bin and executable" case_the_script_is_staged_executable

case_unset_prints_nothing() { run_hook; silent; }
check "an unset variable prints nothing" case_unset_prints_nothing

case_empty_prints_nothing() { run_hook ""; silent; }
check "an empty variable prints nothing" case_empty_prints_nothing

case_missing_prints_nothing() { run_hook "$WORK/no-such-file"; silent; }
check "a missing file prints nothing" case_missing_prints_nothing

case_directory_prints_nothing() { mkdir -p "$WORK/dir"; echo x > "$WORK/dir/f"; run_hook "$WORK/dir"; silent; }
check "a directory prints nothing" case_directory_prints_nothing

case_empty_file_prints_nothing() { : > "$WORK/empty.json"; run_hook "$WORK/empty.json"; silent; }
check "an empty file prints nothing" case_empty_file_prints_nothing

case_the_rendering_is_printed_byte_for_byte() {
    # no trailing newline, a quote, a backslash and a multibyte character: cat's bytes, nothing re-encoded
    printf '{"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": "a\\n\\"b\\" \xc3\xa9"}, "systemMessage": "Selection: f lines 1-2"}' > "$WORK/sel.json"
    run_hook "$WORK/sel.json"
    [ "$RC" -eq 0 ] || { echo "  exit $RC"; return 1; }
    [ ! -s "$WORK/err" ] || { echo "  stderr: $(cat "$WORK/err")"; return 1; }
    cmp -s "$WORK/sel.json" "$WORK/out" || { echo "  stdout differs from the file"; return 1; }
}
check "a file with the rendering is printed byte for byte" case_the_rendering_is_printed_byte_for_byte

case_a_path_with_a_space_is_read() {
    mkdir -p "$WORK/a dir"; printf '{"x":1}' > "$WORK/a dir/selection.json"
    run_hook "$WORK/a dir/selection.json"
    [ "$RC" -eq 0 ] && [ "$(cat "$WORK/out")" = '{"x":1}' ]
}
check "a path with a space is read" case_a_path_with_a_space_is_read

case_unreadable_prints_nothing() {
    case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) return 0 ;; esac
    [ "$(id -u)" != "0" ] || return 0   # root and Windows read a mode 000 file: the case has nothing to prove there
    printf '{"x":1}' > "$WORK/locked.json"; chmod 000 "$WORK/locked.json"
    run_hook "$WORK/locked.json"
    chmod 600 "$WORK/locked.json"
    silent
}
check "an unreadable file prints nothing" case_unreadable_prints_nothing

# The host writes the whole prompt to the hook's stdin: a hook that exits without reading it meets a closed pipe on a long
# prompt (SIGPIPE here, EPIPE in the host).
case_a_long_input_on_stdin_is_read_whole() {
    printf '{"x":1}' > "$WORK/big.json"
    head -c 1048576 /dev/zero | tr '\0' a | SOT_SELECTION_FILE="$WORK/big.json" bash "$HOOK" > "$WORK/out" 2> "$WORK/err"
    local st=("${PIPESTATUS[@]}")
    [ "${st[*]}" = "0 0 0" ] || { echo "  statuses: ${st[*]}"; return 1; }
    [ "$(cat "$WORK/out")" = '{"x":1}' ] || { echo "  stdout: $(cat "$WORK/out")"; return 1; }
}
check "a 1 MiB input on stdin is read whole, the file still printed, no writer meets a closed pipe" case_a_long_input_on_stdin_is_read_whole

case_codex_reads_the_prompt_and_prints_the_selection() {
    local cmd comm="$WORK/comm with space"
    cmd=$(jq -er '.hooks.UserPromptSubmit[0].hooks[0].command' "$(dirname "${BASH_SOURCE[0]}")/../codex/row-hooks.json") || return 1
    mkdir -p "$comm/bin" && cp "$HOOK" "$comm/bin/" || return 1
    printf '{"systemMessage":"Selection: test"}' > "$WORK/selection.json"
    head -c 1048576 /dev/zero | SOT_COMM_HOME="$comm" SOT_SELECTION_FILE="$WORK/selection.json" bash -c "$cmd" > "$WORK/out" 2> "$WORK/err" || return 1
    cmp "$WORK/selection.json" "$WORK/out" && [ ! -s "$WORK/err" ]
}
check "the serialized Codex command drains a large prompt and delivers the selection through the runtime comm home" case_codex_reads_the_prompt_and_prints_the_selection

# The installed hook calls the local identity check. Model its three outcomes without consulting any live row.
hooks_json="$(dirname "${BASH_SOURCE[0]}")/../codex/row-hooks.json"
binding_comm="$WORK/binding comm"
mkdir -p "$binding_comm" && cp -R "$BIN" "$binding_comm/bin" || exit 2
cat > "$WORK/gate" <<'SH'
#!/bin/sh
[ "$*" = 'comm require-agent' ] || exit 3
exit "${GATE_STATUS:-0}"
SH
chmod +x "$WORK/gate"
first=12345678-1234-4567-8123-123456789abc
second=98765432-1234-4567-8123-123456789abc
binding_event() {
    local cmd
    cmd=$(jq -er ".hooks.$1[0].hooks[$2].command" "$hooks_json") || return 1
    printf '%s' "$3" | SOTD_BIN="$WORK/gate" SOT_COMM_HOME="$binding_comm" bash -c "$cmd" > "$WORK/out" 2> "$WORK/err"
    RC=$?
    silent
}
case_binding_events() (
    export SOT_AGENT_KIND=codex SOT_WORKSPACE_ID=own SOT_COMM_HOOKS=on SOT_CODEX_THREAD_FILE="$WORK/current thread"
    binding_event SessionStart 0 "{\"session_id\":\"$first\"}" && [ "$(cat "$SOT_CODEX_THREAD_FILE")" = "$first" ] || exit 1
    ln "$SOT_CODEX_THREAD_FILE" "$WORK/previous inode" || exit 1
    binding_event UserPromptSubmit 1 "{\"session_id\":\"$second\",\"agent_id\":\"$first\"}" &&
        [ "$(cat "$WORK/previous inode")" = "$first" ] && [ "$(cat "$SOT_CODEX_THREAD_FILE")" = "$second" ]
)
check "both Codex events publish the root id silently and replace the whole file" case_binding_events
case_binding_invalid() (
    export SOT_AGENT_KIND=codex SOT_WORKSPACE_ID=own SOT_COMM_HOOKS=on SOT_CODEX_THREAD_FILE="$WORK/valid binding"
    printf '%s\n' "$first" > "$SOT_CODEX_THREAD_FILE"
    for input in '' '{' '{}' 'null' '{"session_id":123}' '{"session_id":"anything"}' "{\"session_id\":\"$second\"} {}"; do
        binding_event SessionStart 0 "$input" && [ "$(cat "$SOT_CODEX_THREAD_FILE")" = "$first" ] || exit 1
    done
    unset SOT_CODEX_THREAD_FILE
    binding_event UserPromptSubmit 1 "{\"session_id\":\"$second\"}"
)
check "malformed inputs preserve the previous binding and an unnamed file is skipped silently" case_binding_invalid
case_binding_owner() (
    export SOT_AGENT_KIND=codex SOT_WORKSPACE_ID=own SOT_COMM_HOOKS=on SOT_CODEX_THREAD_FILE="$WORK/owner only"
    for GATE_STATUS in 1 2; do
        export GATE_STATUS
        binding_event UserPromptSubmit 1 "{\"session_id\":\"$first\"}" && [ ! -e "$SOT_CODEX_THREAD_FILE" ] || exit 1
    done
    unset GATE_STATUS SOT_WORKSPACE_ID
    binding_event SessionStart 0 "{\"session_id\":\"$first\"}" && [ ! -e "$SOT_CODEX_THREAD_FILE" ]
)
check "a nested agent, a different row and a process outside a row cannot publish a binding" case_binding_owner

case_unwritable_binding_folder() (
    local dir="$WORK/read-only state"
    mkdir "$dir" && chmod 555 "$dir" || exit 1
    export SOT_AGENT_KIND=codex SOT_WORKSPACE_ID=own SOT_COMM_HOOKS=on SOT_CODEX_THREAD_FILE="$dir/thread"
    # Root and some filesystems can still create files there. A missing parent also refuses temporary-file creation.
    [ ! -w "$dir" ] || SOT_CODEX_THREAD_FILE="$dir/missing/thread"
    binding_event SessionStart 0 "{\"session_id\":\"$second\"}" && [ -z "$(ls -A "$dir")" ]
)
check "an unwritable binding folder leaves no file and the hook silently succeeds" case_unwritable_binding_folder

# Inject faults in the hook's own shell. At entry to mv the write has finished and the real rename has yet to run.
cat > "$WORK/binding-fault" <<'SH'
if [ "$fault" = write ]; then
    printf() {
        if [ "${2:-}" != "$new_id" ]; then builtin printf "$@"; return; fi
        builtin printf 'partial'
        builtin printf 'write failed\n' >&2
        : > "$hit"
        return 1
    }
else
    mv() {
        cat -- "${@: -2:1}" > "$hit" || return 1
        if [ "$fault" = rename ]; then echo 'rename failed' >&2; return 1; fi
        kill -TERM "$$"
        command mv "$@"
    }
fi
SH
case_binding_publish_failure() (
    local dir="$WORK/state-$1"
    mkdir "$dir" || exit 1
    export fault="$1" hit="$WORK/hit-$1" new_id="$second" BASH_ENV="$WORK/binding-fault"
    export SOT_AGENT_KIND=codex SOT_WORKSPACE_ID=own SOT_COMM_HOOKS=on SOT_CODEX_THREAD_FILE="$dir/thread"
    printf '%s\n' "$first" > "$dir/thread"
    binding_event UserPromptSubmit 1 "{\"session_id\":\"$second\"}" || exit 1
    [ -f "$hit" ] || { echo "  the hook never reached the $fault fault"; exit 1; }
    [ "$fault" = write ] || [ "$(cat "$hit")" = "$second" ] || { echo "  the temporary file lacked the new id"; exit 1; }
    [ "$(cat "$dir/thread")" = "$first" ] || { echo "  the previous binding changed"; exit 1; }
    [ "$(ls -A "$dir")" = thread ] || { echo "  leftover files: $(ls -A "$dir")"; exit 1; }
)
check "a partial write silently preserves the old binding and removes the temporary file" case_binding_publish_failure write
check "a refused rename silently preserves the old binding and removes the temporary file" case_binding_publish_failure rename
check "TERM after the complete write and before rename silently preserves the old binding and removes the temporary file" case_binding_publish_failure terminate

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
