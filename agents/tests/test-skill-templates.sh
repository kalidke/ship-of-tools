#!/usr/bin/env bash
# test-skill-templates.sh — a lint, not a proof of behavior. Claude Code substitutes `$<digit>` and `$ARGUMENTS` into a
# skill's text before the model reads it, so a skill file carries no `$` followed by a digit (`$0`, `$1`, `${1}`). Every
# SKILL.md of the layer (agents/claude, comm/adapters/claude) and of the checkout's own skills (.claude/skills) is read;
# a file grep cannot read fails the lint, and the suite proves the lint can fail, on a scratch skill that carries `$1`.
# It also lints every skill's frontmatter (name is its folder, one-line description within 1024 characters, in the four
# skill trees including the Codex one) and that the install page names every skill and its uninstall line every Codex
# skill; each has a scratch mutant that must fail.
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

# frontmatter_bad ROOT — every SKILL.md of the four skill trees (the three above and comm/adapters/codex/skills) names its
# own folder in `name:` and has a one-line `description:` (not a `>` or `|` block, which the line read cannot see) of at
# most 1024 characters (Codex's own loader limit; the repo states none). 0 only when there are skills and none is wrong.
frontmatter_bad() {
    local root="$1" file n name desc seen=0 bad=0
    while IFS= read -r file; do
        seen=$((seen + 1))
        n="$(basename "$(dirname "$file")")"
        name="$(sed -n '2,/^---$/{s/^name: *//p;}' "$file" | head -1)"
        desc="$(sed -n '2,/^---$/{s/^description: *//p;}' "$file" | head -1)"
        [ "$name" = "$n" ] || { echo "$file: name '$name' is not its folder '$n'"; bad=1; }
        case "$desc" in '>'*|'|'*) echo "$file: description is a block, not one line"; bad=1 ;; esac
        [ -n "$desc" ] && [ "${#desc}" -le 1024 ] || { echo "$file: description missing or over 1024 characters"; bad=1; }
    done < <(find "$root/agents/claude" "$root/comm/adapters/claude" "$root/comm/adapters/codex/skills" "$root/.claude/skills" -name SKILL.md 2>/dev/null)
    [ "$seen" -gt 0 ] || { echo "no skills found under $root"; return 1; }
    return "$bad"
}
check "lint: every skill's name is its folder and its description is one line within 1024 characters" frontmatter_bad "$ROOT"
mkdir -p "$WORK/fm/agents/claude/y" "$WORK/fm2/agents/claude/z"
printf -- '---\nname: z\ndescription: >\n  a block\n---\nbody\n' > "$WORK/fm2/agents/claude/z/SKILL.md"
if frontmatter_bad "$WORK/fm2" > /dev/null; then
    echo "FAIL: the frontmatter lint passes a description written as a block"; FAIL=$((FAIL + 1))
else
    echo "PASS: the frontmatter lint fails a description written as a block"; PASS=$((PASS + 1))
fi
printf -- '---\nname: other\n---\nbody\n' > "$WORK/fm/agents/claude/y/SKILL.md"
if frontmatter_bad "$WORK/fm" > /dev/null; then
    echo "FAIL: the frontmatter lint passes a skill whose name is not its folder"; FAIL=$((FAIL + 1))
else
    echo "PASS: the frontmatter lint fails a skill whose name is not its folder and has no description"; PASS=$((PASS + 1))
fi

# page_misses ROOT PAGE — the install page names every skill folder (in backticks), and its uninstall line removes every
# Codex skill. 0 only when none is missing and there are skills.
page_misses() {
    local root="$1" page="$2" d n seen=0 bad=0 rm_line
    rm_line="$(grep -F 'rm -rf ~/.codex/skills/' "$page")"
    for d in "$root"/agents/claude/*/ "$root"/comm/adapters/claude/*/ "$root"/comm/adapters/codex/skills/*/; do
        [ -f "$d/SKILL.md" ] || continue
        seen=$((seen + 1)); n="$(basename "$d")"
        grep -qF "\`$n\`" "$page" || { echo "$page does not name skill $n"; bad=1; }
    done
    for d in "$root"/comm/adapters/codex/skills/*/; do
        n="$(basename "$d")"
        case "$rm_line" in *"$n"*) ;; *) echo "the uninstall line leaves ~/.codex/skills/$n"; bad=1 ;; esac
    done
    local list="$root/agents/codex/shared-skills.txt" expected line actual section
    expected="$(LC_ALL=C sort "$list")" && [ -n "$expected" ] || return 1
    for section in '^[|] .*sot-row@sot-local' '^- .*sot-row@sot-local'; do
        line="$(grep -E "$section" "$page")" || return 1
        # Both records put only the skill names after the final "skills are ".
        actual="$(printf '%s\n' "${line##*skills are }" | grep -oE '`[a-z][a-z0-9-]*`' | tr -d '`' | LC_ALL=C sort)"
        [ "$actual" = "$expected" ] || { echo "row plugin skills in $section differ from $list"; bad=1; }
    done
    [ "$seen" -gt 0 ] || return 1
    return "$bad"
}
check "lint: the install page names every skill and its uninstall line removes every Codex skill" page_misses "$ROOT" "$ROOT/docs/src/start/install.md"
sed 's/,sot-chat-pair}/}/' "$ROOT/docs/src/start/install.md" > "$WORK/page-no-codex.md"
sed 's/`sot-chat-post`/`x`/g' "$ROOT/docs/src/start/install.md" > "$WORK/page-no-name.md"
sed '/^[|] .*sot-row@sot-local/s/`worktree`/`other`/' "$ROOT/docs/src/start/install.md" > "$WORK/page-row.md"
sed '/^- .*sot-row@sot-local/s/`worktree`/`other`/' "$ROOT/docs/src/start/install.md" > "$WORK/page-uninstall.md"
for m in page-no-codex page-no-name page-row page-uninstall; do
    if page_misses "$ROOT" "$WORK/$m.md" > /dev/null; then
        echo "FAIL: the page lint passes a page that misses a skill ($m)"; FAIL=$((FAIL + 1))
    else
        echo "PASS: the page lint fails a page that misses a skill ($m)"; PASS=$((PASS + 1))
    fi
done

mkdir -p "$WORK/bad dir/.claude/skills/x"
printf -- '---\nname: x\n---\nrun $1 now\n' > "$WORK/bad dir/.claude/skills/x/SKILL.md"
if template_hits "$WORK/bad dir" > /dev/null; then
    echo "FAIL: the lint passes a skill that carries \$1"; FAIL=$((FAIL + 1))
else
    echo "PASS: the lint fails a skill that carries \$1, under a folder with a space in its name"; PASS=$((PASS + 1))
fi

echo "PASS: $PASS  FAIL: $FAIL"
exit $((FAIL > 0))
