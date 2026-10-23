#!/usr/bin/env bash
# test-skill-templates.sh — a lint, not a proof of behavior. Claude Code substitutes `$<digit>` and `$ARGUMENTS` into a
# skill's text before the model reads it, so a skill file carries no `$` followed by a digit (`$0`, `$1`, `${1}`). Every
# SKILL.md of the layer (agents/claude, comm/adapters/claude) and of the checkout's own skills (.claude/skills) is read;
# a file grep cannot read fails the lint, and the suite proves the lint can fail, on a scratch skill that carries `$1`.
# Nothing here runs a skill's script or Claude Code.
#
# HERMETIC: reads this checkout and writes only under a temp folder of its own.
#
# Usage: agents/tests/test-skill-templates.sh     Exit: 0 if every row passes, 1 otherwise.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-skill-templates-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
trap 'rm -rf "${WORK:?}"' EXIT
PASS=0
FAIL=0
check() {
    local name="$1"; shift
    if "$@"; then echo "PASS: $name"; PASS=$((PASS + 1)); else echo "FAIL: $name"; FAIL=$((FAIL + 1)); fi
}

# template_hits ROOT — every `$<digit>` line of each SKILL.md under ROOT's three skill folders, one file at a time (a
# path with a space is one file); 0 only when there are skills, grep read every one, and none carries one.
template_hits() {
    local root="$1" file out status seen=0 bad=0
    while IFS= read -r file; do
        seen=$((seen + 1))
        out="$(grep -n -E '\$\{?[0-9]' "$file")"; status=$?
        case "$status" in
            0) printf '%s:%s\n' "$file" "$out"; bad=1 ;;
            1) ;;
            *) echo "grep could not read $file"; bad=1 ;;
        esac
    done < <(find "$root/agents/claude" "$root/comm/adapters/claude" "$root/.claude/skills" -name SKILL.md 2>/dev/null)
    [ "$seen" -gt 0 ] || { echo "no skills found under $root"; return 1; }
    return "$bad"
}

check "lint: no skill file carries a dollar sign and a digit" template_hits "$ROOT"

mkdir -p "$WORK/bad dir/.claude/skills/x"
printf -- '---\nname: x\n---\nrun $1 now\n' > "$WORK/bad dir/.claude/skills/x/SKILL.md"
if template_hits "$WORK/bad dir" > /dev/null; then
    echo "FAIL: the lint passes a skill that carries \$1"; FAIL=$((FAIL + 1))
else
    echo "PASS: the lint fails a skill that carries \$1, under a folder with a space in its name"; PASS=$((PASS + 1))
fi

echo "PASS: $PASS  FAIL: $FAIL"
exit $((FAIL > 0))
