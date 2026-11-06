#!/usr/bin/env bash
# test-defects-check.sh: defects-check.sh (every item id a landing record names is an item of docs/defects.md). Over
# fixture checkouts: a record naming an unknown id fails and the output names the id (the red case); a known open id and a
# known closed id pass; capitals that are not ids (P1, ALLDONE, RCG_SCOPE, SHA-256, UTF-8) never hit; ids written before the
# landing section are not records; a duplicate id and an open item with no mark fail; a missing section is refused (2).
# Last, the real tree passes. Exit: failed checks.
set -u
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
base="${TMPDIR:-/tmp}"
work=$(mktemp -d "${base%/}/defects-check-XXXXXX") || exit 2
work=$(cd "$work" && pwd -P) || exit 2
trap 'rm -rf "${work:?}"' EXIT
fail=0
check() { if eval "$2"; then echo "PASS: $1"; else echo "FAIL: $1"; fail=$((fail + 1)); fi; }

LIST_OK='# Known defects

## rows

- **ROW-ID-MINT-OPEN** [note] an open item.
- **WIN-IDLE-STATUS-50MS** [session] an id with a digit part.

## Closed

- **ROW-ID-MINT** (rows) closed by abc1234.
'
fixture() { # <name> <passes tail after the landing heading> [<defects text>]: a checkout under $work/<name>
    local d=$work/$1
    mkdir -p "$d/docs/history"
    printf '%s' "${3-$LIST_OK}" > "$d/docs/defects.md"
    printf '# Passes\n\nEarlier text names OLD-UNKNOWN-ID, which is no record.\n\n## Landing records since 2026-11-06\n\n%s\n' "$2" > "$d/docs/history/passes.md"
}
run() { bash "$here/defects-check.sh" --root "$work/$1" 2>&1; }

fixture unknown '- P1, 2026-11-06: fixes NO-SUCH-ITEM and nothing else.'
out=$(run unknown); r=$?
check "a record naming an unknown id fails" '[ "$r" -eq 1 ]'
check "the failure names the id" 'grep -q "NO-SUCH-ITEM" <<< "$out"'
check "an id before the landing section is no record" '! grep -q "OLD-UNKNOWN-ID" <<< "$out"'

fixture known '- A fix, 2026-11-06: closes WIN-IDLE-STATUS-50MS and ROW-ID-MINT, and moves ROW-ID-MINT-OPEN.'
out=$(run known); r=$?
check "known open and closed ids pass" '[ "$r" -eq 0 ] && [ -z "$out" ]'

fixture false '- P1, B1 and P14 landed. ALLDONE SCOPED; RCG_SCOPE=HEAD, SHA-256 of UTF-8 text, CARGO_TARGET_DIR, 0.7.1-DEV, and the ranges P1-P4, B1-B5, R1-R3 and D2-D4.'
out=$(run false); r=$?
check "capitals that are not ids never hit" '[ "$r" -eq 0 ] && [ -z "$out" ]'

fixture later '- First record.

## A later section

- Second record names LATER-UNKNOWN-ID.'
out=$(run later); r=$?
check "a section after the landing heading is still a record" '[ "$r" -eq 1 ] && grep -q "LATER-UNKNOWN-ID" <<< "$out"'

fixture lower '- A record naming Non-ID-words, ROW-ID-MINTs and xROW-ID-MINT.'
out=$(run lower); r=$?
check "a token inside lower-case text or with a suffix letter is not the id" '[ "$r" -eq 0 ] && [ -z "$out" ]'

fixture dup '- Nothing.' "$LIST_OK
- **ROW-ID-MINT-OPEN** [note] again.
"
out=$(run dup); r=$?
check "an id that starts two items fails naming it" '[ "$r" -eq 1 ] && grep -q "ROW-ID-MINT-OPEN: starts two items" <<< "$out"'

fixture nomark '- Nothing.' "## rows

- **ROW-MARKLESS-ITEM** an open item with no mark.

## Closed

- **ROW-CLOSED-ITEM** (rows) closed by abc1234.
"
out=$(run nomark); r=$?
check "an open item with no mark fails naming it" '[ "$r" -eq 1 ] && grep -q "ROW-MARKLESS-ITEM: an open item with no mark" <<< "$out"'
check "a closed item needs no mark" '! grep -q "ROW-CLOSED-ITEM" <<< "$out"'

# ---- a tracked CLAUDE.md that cites the list names an item there
cite() { # <name> <CLAUDE.md text>: a passing checkout with one contract
    fixture "$1" '- Nothing.'
    printf '%s\n' "$2" > "$work/$1/CLAUDE.md"
}
cite citeok 'A limit (docs/defects.md: ROW-ID-MINT-OPEN); another (docs/defects.md item WIN-IDLE-STATUS-50MS, a note); and docs/defects.md in prose.'
out=$(run citeok); r=$?
check "citations naming known items pass, and a plain mention of the file is no citation" '[ "$r" -eq 0 ] && [ -z "$out" ]'
cite citebare 'A limit (docs/defects.md).'
out=$(run citebare); r=$?
check "a citation with no id fails naming the file" '[ "$r" -eq 1 ] && grep -q "CLAUDE.md: cites docs/defects.md with no item id" <<< "$out"'
cite citeunknown 'A limit (docs/defects.md item NO-SUCH-ITEM).'
out=$(run citeunknown); r=$?
check "a citation of an id that is no item fails naming the id" '[ "$r" -eq 1 ] && grep -q "cites NO-SUCH-ITEM, which is no item" <<< "$out"'
cite citemany 'A limit (docs/defects.md: ROW-ID-MINT-OPEN and NO-SUCH-TWO, ROW-ID-MINT).'
out=$(run citemany); r=$?
check "every id of a joined citation is checked" '[ "$r" -eq 1 ] && grep -q "NO-SUCH-TWO" <<< "$out" && ! grep -q "cites ROW-ID-MINT" <<< "$out"'
fixture rangehead '- Nothing.' "$LIST_OK
- **P1-P4** [note] a head that is no id.
"
out=$(run rangehead); r=$?
check "an item head that is no id by the rule fails naming it" '[ "$r" -eq 1 ] && grep -q "P1-P4: an item head that is no id" <<< "$out"'

mkdir -p "$work/nosection/docs/history"
printf '%s' "$LIST_OK" > "$work/nosection/docs/defects.md"
printf '# Passes\n\nNo landing section.\n' > "$work/nosection/docs/history/passes.md"
out=$(run nosection); r=$?
check "a passes file with no landing section is refused" '[ "$r" -eq 2 ]'
bash "$here/defects-check.sh" --root "$work/absent" > /dev/null 2>&1; r=$?
check "a missing checkout is refused" '[ "$r" -eq 2 ]'
bash "$here/defects-check.sh" --bogus > /dev/null 2>&1; r=$?
check "a bad argument is refused" '[ "$r" -eq 2 ]'

out=$(bash "$here/defects-check.sh" 2>&1); r=$?
check "the real tree passes" '[ "$r" -eq 0 ]'
[ "$r" -eq 0 ] || echo "$out"

echo "failed: $fail"
exit "$fail"
