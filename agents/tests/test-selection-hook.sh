#!/usr/bin/env bash
# test-selection-hook.sh — sot-selection-hook.sh prints the file $SOT_SELECTION_FILE names when it is a readable regular
# file, prints nothing in every other case, and always exits 0 with nothing on stderr. It is run from the staged bin, the
# comm scripts laid out flat as the installer lays them, which also proves it lands there executable. The shell is
# whichever bash this suite runs under: macOS's /bin/bash 3.2 and Git Bash on the hosted runs
# (`scripts/tests/under-bash.sh`), bash here.
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
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
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

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
