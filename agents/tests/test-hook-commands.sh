#!/usr/bin/env bash
# test-hook-commands.sh — the layer's hook commands run in a shell. The argv and the JSON are proved in Rust
# (`argv::tests::the_layer_rides_the_recipe_and_a_plain_claude_has_none`); this proves what those tests cannot: each hook
# command `sotd agent-exec claude` hands claude, run by the shell claude runs it with, starts its script, with a comm
# folder whose name holds a space and a quote. A stand-in `claude` records its arguments; the scripts are stand-ins that
# leave a mark. The shell is whichever bash this suite runs under: macOS's /bin/bash 3.2 and Git Bash on the hosted runs
# (`scripts/tests/under-bash.sh`), bash here.
#
# HERMETIC: its own HOME and comm folder, no daemon, no real claude.
#
# Usage: agents/tests/test-hook-commands.sh     Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

WORK="$(mktemp -d "${TMPDIR:-/tmp}/hook-commands-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi
WORK="$(cd "$WORK" && pwd -P)" || exit 1
trap 'rm -rf "${WORK:?}"' EXIT
guard_fresh_home "$WORK"
[ -n "${GUARD_SOTD:-}" ] || { echo "FATAL: no built sotd (set SOT_TEST_SOTD or CARGO_TARGET_DIR)" >&2; exit 2; }
command -v jq > /dev/null 2>&1 || { echo "FATAL: jq is needed to read the settings" >&2; exit 2; }

COMM="$WORK/comm folder it's"   # a space and a quote: the quoting is what is under test
mkdir -p "$COMM/bin" "$COMM/claude-layer/.claude/skills/sot-session-start" "$HOME/.local/bin"
printf -- '---\nname: sot-session-start\n---\n' > "$COMM/claude-layer/.claude/skills/sot-session-start/SKILL.md"
MARKS="$WORK/marks"; mkdir -p "$MARKS"
for s in comm-status-working comm-status-blocked comm-status-heartbeat; do
    printf '#!/usr/bin/env bash\n: > "%s/%s"\n' "$MARKS" "$s" > "$COMM/bin/$s.sh"
    chmod +x "$COMM/bin/$s.sh"
done
ARGS="$WORK/claude.args"
printf '#!/bin/sh\nprintf "%%s\\n" "$@" > "%s"\n' "$ARGS" > "$HOME/.local/bin/claude"
chmod +x "$HOME/.local/bin/claude"

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}

case_every_hook_command_starts_its_script() {
    local settings cmds cmd name
    ( cd "$WORK" && env -i HOME="$HOME" PATH="/usr/bin:/bin" SOT_COMM_HOME="$COMM" "$GUARD_SOTD" agent-exec claude ) \
        > "$WORK/exec.out" 2>&1 || { echo "  agent-exec failed: $(cat "$WORK/exec.out")"; return 1; }
    [ -s "$ARGS" ] || { echo "  the stand-in claude was not run: $(cat "$WORK/exec.out")"; return 1; }
    settings="$(sed -n '/^--settings$/{n;p;}' "$ARGS")"
    [ -n "$settings" ] || { echo "  no --settings in: $(cat "$ARGS")"; return 1; }
    cmds="$(printf '%s' "$settings" | jq -r '.hooks[][].hooks[].command')" || { echo "  unreadable settings: $settings"; return 1; }
    [ "$(printf '%s\n' "$cmds" | grep -c .)" -eq 3 ] || { echo "  want three hook commands: $cmds"; return 1; }
    while IFS= read -r cmd; do
        bash -c "$cmd" > "$WORK/hook.out" 2>&1 || { echo "  '$cmd' failed: $(cat "$WORK/hook.out")"; return 1; }
    done <<< "$cmds"
    for name in comm-status-working comm-status-blocked comm-status-heartbeat; do
        [ -e "$MARKS/$name" ] || { echo "  $name never ran; commands: $cmds"; return 1; }
    done
}
check "each hook command in the leg's --settings starts its script from a comm folder with a space and a quote" \
    case_every_hook_command_starts_its_script

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
