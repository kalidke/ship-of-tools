#!/usr/bin/env bash
# release-before.sh -- the release an update smoke starts from: the newest published release older than a version.
#
#   release-before.sh SOTD VERSION [OWNER/NAME]
#
# Prints its tag. SOTD is any sotd of this line (its `update direction` is the one version comparison in the tree, so no
# semver is written here); VERSION is the version under test. The list is the repository's published releases
# (GITHUB_TOKEN reads it when set); SOT_SMOKE_RELEASES_JSON names a saved copy of the first page instead. Drafts are not
# releases, and neither GitHub's "latest" nor a tag named here is used: only version order picks. A release that lacks an
# asset an update smoke installs on one of the three platforms (or SHA256SUMS and COMMIT) is an error naming it, never
# a reason to take an older one. Exits 1 with the reason when no release is older.
set -euo pipefail

die() { printf 'release-before: %s\n' "$*" >&2; exit 1; }
[ $# -ge 2 ] && [ $# -le 3 ] || { echo "usage: release-before.sh SOTD VERSION [OWNER/NAME]" >&2; exit 2; }
SOTD="$1" VERSION="${2#v}" REPO="${3:-${GITHUB_REPOSITORY:-kalidke/ship-of-tools}}"
[ -x "$SOTD" ] || die "$SOTD is not an executable sotd"
command -v jq >/dev/null || die "jq is required"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-before.XXXXXX")"
trap 'rm -rf "${WORK:?}"' EXIT
LIST="$WORK/releases.json"
if [ -n "${SOT_SMOKE_RELEASES_JSON:-}" ]; then
    cp "$SOT_SMOKE_RELEASES_JSON" "$LIST"
else
    : > "$WORK/all.jsonl"
    page=1
    while [ "$page" -le 10 ]; do
        auth=()
        [ -z "${GITHUB_TOKEN:-}" ] || auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
        curl --connect-timeout 15 --speed-limit 1000 --speed-time 60 -fsSL -H 'Accept: application/vnd.github+json' \
            ${auth[@]+"${auth[@]}"} "https://api.github.com/repos/$REPO/releases?per_page=100&page=$page" > "$WORK/page.json" \
            || die "could not list the releases of $REPO"
        [ "$(jq 'length' "$WORK/page.json")" -gt 0 ] || break
        jq -c '.[]' "$WORK/page.json" >> "$WORK/all.jsonl"
        page=$((page + 1))
    done
    jq -s '.' "$WORK/all.jsonl" > "$LIST"
fi

# The comparison reads an install.json: a scratch one at the version it is asked against.
mkdir -p "$WORK/cmp"
against() {  # <version>
    printf '{\n  "schema": 1,\n  "prefix": "%s",\n  "version": "%s"\n}\n' "$WORK/cmp" "$1" > "$WORK/cmp/install.json"
}
way() { "$SOTD" update direction "$1" --prefix "$WORK/cmp"; }  # <candidate>: its direction against what `against` set

best=""
against "$VERSION"
while IFS= read -r tag; do
    [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]*)?$ ]] || continue
    [ "$(way "${tag#v}")" = older ] || continue
    if [ -z "$best" ]; then
        best="$tag"
    else
        against "${best#v}"
        [ "$(way "${tag#v}")" != newer ] || best="$tag"
        against "$VERSION"
    fi
done < <(jq -r '.[] | select(.draft == false) | .tag_name' "$LIST")
[ -n "$best" ] || die "no published release of $REPO is older than $VERSION"

have="$(jq -r --arg t "$best" '.[] | select(.tag_name == $t) | .assets[].name' "$LIST")"
ver="${best#v}"
for want in SHA256SUMS COMMIT "sot-$ver-linux-x86_64.tar.gz" "sot-$ver-macos-aarch64.tar.gz" "sot-$ver-windows-x86_64.zip"; do
    printf '%s\n' "$have" | grep -qxF "$want" || die "$best, the newest release older than $VERSION, has no asset $want"
done
printf '%s\n' "$best"
