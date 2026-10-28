#!/usr/bin/env bash
# test-show-result.sh — hermetic suite for which `sot-fe` verb `show-result` asks for. An image or any non-page badges
# through `preview`; an .html page opens through `docs` by its absolute path, with `--direct` only when it lies under
# the calling row's project folder (SOT_WORKSPACE_ROOT) and the installed sot-fe knows the flag. A stand-in sot-fe in a
# temporary HOME records its arguments; nothing reaches a daemon or the live comm home.
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

# run PATH [ENV=VALUE...] — show-result from inside the row's folder, as a row's session runs it.
run() {
    local path="$1"; shift
    rm -f "$ARGS"
    (cd "$ROW" && env HOME="$FAKE_HOME" SOT_WORKSPACE=row-slug SOT_WORKSPACE_ROOT="$ROW" STUB_ARGS="$ARGS" "$@" \
        bash "$SHOW" "$path") >/dev/null 2>&1
}
got() { tr '\n' ' ' < "$ARGS" 2>/dev/null | sed 's/ $//'; }
expect() {
    local want="$1"
    [ "$(got)" = "$want" ] || { echo "  sot-fe got: '$(got)'"; echo "  wanted:     '$want'"; return 1; }
}

case_an_image_badges_through_preview() {
    run fig.png && expect "preview row-slug fig.png"
}
case_a_page_in_the_row_opens_direct() {
    : > "$ROW/dev/output/decision.html"
    run dev/output/decision.html && expect "docs row-slug $ROW/dev/output/decision.html --direct" || return 1
    run "$ROW/dev/output/decision.html" && expect "docs row-slug $ROW/dev/output/decision.html --direct"
}
case_a_page_outside_the_row_opens_plain() {
    : > "$ELSEWHERE/report.html"
    run "$ELSEWHERE/report.html" && expect "docs row-slug $ELSEWHERE/report.html"
}
case_an_older_sot_fe_opens_it_plain() {
    : > "$ROW/page.htm"
    run page.htm STUB_OLD=1 && expect "docs row-slug $ROW/page.htm"
}

check "an image badges through preview, unchanged" case_an_image_badges_through_preview
check "a page under the row's folder opens with docs --direct, by its absolute path" case_a_page_in_the_row_opens_direct
check "a page outside the row's folder opens with docs, no scope" case_a_page_outside_the_row_opens_plain
check "a sot-fe that does not know --direct opens the page plain" case_an_older_sot_fe_opens_it_plain

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
