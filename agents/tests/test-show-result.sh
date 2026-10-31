#!/usr/bin/env bash
# test-show-result.sh — hermetic suite for which `sot-fe` verb `show-result` asks for. An image or any non-page badges
# through `preview`; an .html page opens nothing: under the calling row's project folder (SOT_WORKSPACE_ROOT) it says how
# to offer the page to the person's Queue (`Page: <absolute path>` on the closing marker line), and elsewhere it refuses.
# A stand-in sot-fe in a temporary HOME records its arguments; nothing reaches a daemon or the live comm home.
#
# Usage: agents/tests/test-show-result.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

SHOW="$(cd "$(dirname "${BASH_SOURCE[0]}")/../claude/bin" && pwd)/show-result"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/show-result-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi
trap 'rm -rf "${WORK:?}"' EXIT

FAKE_HOME="$WORK/home"
mkdir -p "$FAKE_HOME/.sot-comm/bin" "$WORK/row/dev/output" "$WORK/elsewhere"
ROW="$(cd "$WORK/row" && pwd -P)"
ELSEWHERE="$(cd "$WORK/elsewhere" && pwd -P)"
ARGS="$WORK/args"
# The stand-in: `--help` names --direct unless STUB_OLD is set; any other call records its arguments, one per line.
cat > "$FAKE_HOME/.sot-comm/bin/sot-fe" <<'STUB'
#!/usr/bin/env bash
if [ "${1:-}" = --help ]; then
    echo "sot-fe docs <workspace> <abs-path> [--fe <host>]"
    [ -n "${STUB_OLD:-}" ] || echo "  --direct  --direct-all"
    exit 0
fi
printf '%s\n' "$@" > "$STUB_ARGS"
STUB
chmod +x "$FAKE_HOME/.sot-comm/bin/sot-fe"

PASS=0
FAIL=0
check() {
    if "$2"; then echo "PASS: $1"; PASS=$((PASS + 1)); else echo "FAIL: $1"; FAIL=$((FAIL + 1)); fi
}

# run PATH [ENV=VALUE...] — show-result from inside the row's folder, as a row's session runs it; its output in $OUT
# and its exit code in $RC.
OUT="$WORK/out"
run() {
    local path="$1"; shift
    rm -f "$ARGS"
    (cd "$ROW" && env HOME="$FAKE_HOME" SOT_WORKSPACE=row-slug SOT_WORKSPACE_ROOT="$ROW" STUB_ARGS="$ARGS" "$@" \
        bash "$SHOW" "$path") >"$OUT" 2>&1
    RC=$?
}
got() { tr '\n' ' ' < "$ARGS" 2>/dev/null | sed 's/ $//'; }
expect() {
    local want="$1"
    [ "$(got)" = "$want" ] || { echo "  sot-fe got: '$(got)'"; echo "  wanted:     '$want'"; return 1; }
}

case_an_image_badges_through_preview() {
    run fig.png && expect "preview row-slug fig.png"
}
# A page under the row's folder opens nothing: show-result names the line that offers it to the Queue, by its absolute
# path, and exits 0 (the maintainer, 2026-10-31: pages went to the browser, not to the Queue).
case_a_page_in_the_row_is_offered_and_opens_nothing() {
    : > "$ROW/dev/output/decision.html"
    for path in dev/output/decision.html "$ROW/dev/output/decision.html"; do
        run "$path"
        [ "$RC" -eq 0 ] || { echo "  exit $RC: $(cat "$OUT")"; return 1; }
        [ ! -e "$ARGS" ] || { echo "  sot-fe was run: '$(got)'"; return 1; }
        grep -qF "Page: $ROW/dev/output/decision.html" "$OUT" || { echo "  no offer line: $(cat "$OUT")"; return 1; }
    done
}
# A page outside the row's folder cannot be served from the Queue: refused, exit 2, nothing run.
case_a_page_outside_the_row_is_refused() {
    : > "$ELSEWHERE/report.html"
    run "$ELSEWHERE/report.html"
    [ "$RC" -eq 2 ] || { echo "  expected exit 2, got $RC: $(cat "$OUT")"; return 1; }
    [ ! -e "$ARGS" ] || { echo "  sot-fe was run: '$(got)'"; return 1; }
    grep -qF "project folder" "$OUT" || { echo "  no reason: $(cat "$OUT")"; return 1; }
}

check "an image badges through preview, unchanged" case_an_image_badges_through_preview
check "a page under the row's folder is offered to the Queue and opens nothing" case_a_page_in_the_row_is_offered_and_opens_nothing
check "a page outside the row's folder is refused and opens nothing" case_a_page_outside_the_row_is_refused

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
