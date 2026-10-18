#!/usr/bin/env bash
# test-release-before.sh -- release-before.sh picks the newest published release older than a version, by the real sotd's
# `update direction`, from a fixture list (SOT_SMOKE_RELEASES_JSON): drafts and non-version tags are skipped, the first
# rc of a new minor takes the previous final, a version with nothing older fails, and a release missing an asset is an
# error naming it, never a reason to take an older one. Needs the sotd of this tree (SOT_TEST_SOTD): the comparison is
# the one place in the tree that orders versions, so a stub would test nothing.
#
# Run: SOT_TEST_SOTD=rust/target/debug/sotd scripts/tests/test-release-before.sh

set -euo pipefail

SCRIPT="$(cd "$(dirname "$0")" && pwd)/release-before.sh"
[ -n "${SOT_TEST_SOTD:-}" ] && [ -x "$SOT_TEST_SOTD" ] || { echo "FAIL: SOT_TEST_SOTD must name an executable sotd of this tree" >&2; exit 1; }
command -v jq >/dev/null || { echo "FAIL: jq is required" >&2; exit 1; }
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK:?}"' EXIT
fails=0

check() {  # <description> <expected> <actual>
    if [ "$2" = "$3" ]; then
        printf '    ok   %s\n' "$1"
    else
        printf '    FAIL %s\n      expected: %s\n      actual:   %s\n' "$1" "$2" "$3"
        fails=$((fails + 1))
    fi
}

# release <tag> <draft true|false> [asset-to-leave-out]: one entry of the releases list, with the five assets an update smoke installs.
release() {
    local tag="$1" draft="$2" skip="${3:-}" v="${1#v}" a
    printf '{"tag_name": "%s", "draft": %s, "assets": [' "$tag" "$draft"
    local sep=""
    for a in SHA256SUMS COMMIT "sot-$v-linux-x86_64.tar.gz" "sot-$v-macos-aarch64.tar.gz" "sot-$v-windows-x86_64.zip"; do
        [ "$a" = "$skip" ] && continue
        printf '%s{"name": "%s"}' "$sep" "$a"
        sep=", "
    done
    printf ']}'
}
# list <entry>...: a releases list, newest first as GitHub gives it.
list() {
    local sep=""
    printf '['
    for e in "$@"; do printf '%s%s' "$sep" "$e"; sep=", "; done
    printf ']\n'
}
# pick <list file> <version>: the tag printed, or the failure's message. The repository is named here, not read from the
# environment, so a fork's GITHUB_REPOSITORY does not change the message.
pick() {
    SOT_SMOKE_RELEASES_JSON="$1" bash "$SCRIPT" "$SOT_TEST_SOTD" "$2" example/ship-of-tools 2>&1 || true
}

list "$(release v0.6.7-rc1 true)" "$(release v0.6.6 false)" "$(release v0.6.6-rc9.14 false)" "$(release v0.6.6-rc9.13 false)" \
    "$(release v0.6.6-rc9 false)" "$(release v0.6.5 false)" "$(release v0.6.5-rc1 false)" "$(release v0.5.10 false)" \
    '{"tag_name": "nightly", "draft": false, "assets": []}' > "$WORK/full.json"

echo "  the pick by version order"
check "a dev build of the line takes the release before its rc"     "v0.6.6-rc9.13" "$(pick "$WORK/full.json" 0.6.6-rc9.14-dev+8461efd6f)"
check "an rc takes the rc before it"                                  "v0.6.6-rc9.13" "$(pick "$WORK/full.json" 0.6.6-rc9.14)"
check "the final takes the last rc"                                   "v0.6.6-rc9.14" "$(pick "$WORK/full.json" 0.6.6)"
check "the first rc of a new minor takes the previous final, its own draft skipped" "v0.6.6" "$(pick "$WORK/full.json" 0.6.7-rc1)"
check "a dev build of the next version takes the previous final"      "v0.6.6" "$(pick "$WORK/full.json" 0.6.7-DEV+abc123456)"
check "a leading v on the version is the same version"                "v0.6.6-rc9.14" "$(pick "$WORK/full.json" v0.6.6)"
check "a version with nothing older fails and says so" \
    "release-before: no published release of example/ship-of-tools is older than 0.5.10" "$(pick "$WORK/full.json" 0.5.10)"

echo "  a release that lacks an asset"
list "$(release v0.6.6-rc9.14 false sot-0.6.6-rc9.14-windows-x86_64.zip)" "$(release v0.6.6-rc9.13 false)" > "$WORK/missing.json"
check "the newest older release is the one judged: its missing asset is named, the release before it is not taken" \
    "release-before: v0.6.6-rc9.14, the newest release older than 0.6.6, has no asset sot-0.6.6-rc9.14-windows-x86_64.zip" \
    "$(pick "$WORK/missing.json" 0.6.6)"
check "a release newer than the version is not judged" "v0.6.6-rc9.13" "$(pick "$WORK/missing.json" 0.6.6-rc9.14)"

if [ "$fails" -gt 0 ]; then
    echo "FAILED: $fails check(s)"
    exit 1
fi
echo "all checks passed"
