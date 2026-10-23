#!/usr/bin/env bash
# test-skill-templates.sh — a skill body is a template: Claude Code substitutes `$<digit>` and `$ARGUMENTS` into the text
# before the model reads it, so a skill file carries no `$` followed by a digit (`$0`, `$1`, `${1}`), and a script a skill
# ships finds its own folder by BASH_SOURCE, never `$0`, which a substituted body has already spoiled. Every skill file of
# the layer (agents/claude, comm/adapters/claude) and of the checkout's own skills (.claude/skills) is read; the suite
# also proves the check can fail, on a scratch skill that carries one.
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

# template_hits ROOT — every `$<digit>` line in a SKILL.md under ROOT's three skill folders; 0 only when there are skills
# and none carries one.
template_hits() {
    local root="$1" files hits
    files="$(find "$root/agents/claude" "$root/comm/adapters/claude" "$root/.claude/skills" -name SKILL.md 2>/dev/null)"
    [ -n "$files" ] || { echo "no skills found under $root"; return 1; }
    # shellcheck disable=SC2086
    hits="$(grep -n -E '\$\{?[0-9]' $files)" || return 0
    printf '%s\n' "$hits"
    return 1
}

# The launcher script of /sot-setup finds its folder the way a sourced or substituted script can.
setup_finds_its_folder_by_source() {
    grep -q 'BASH_SOURCE\[0\]' "$ROOT/agents/claude/sot-setup/SKILL.md" \
        && ! grep -n -E 'dirname[[:space:]]+"?\$\{?0' "$ROOT/agents/claude/sot-setup/SKILL.md"
}

check "no skill file carries a dollar sign and a digit" template_hits "$ROOT"
check "the sot-setup skill's launcher script finds its folder by BASH_SOURCE" setup_finds_its_folder_by_source

mkdir -p "$WORK/bad/.claude/skills/x"
printf -- '---\nname: x\n---\nrun $1 now\n' > "$WORK/bad/.claude/skills/x/SKILL.md"
if template_hits "$WORK/bad" > /dev/null; then
    echo "FAIL: the check passes a skill that carries \$1"; FAIL=$((FAIL + 1))
else
    echo "PASS: the check fails a skill that carries \$1"; PASS=$((PASS + 1))
fi

echo "PASS: $PASS  FAIL: $FAIL"
exit $((FAIL > 0))
