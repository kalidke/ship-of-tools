#!/usr/bin/env bash
# release.sh — cut a Ship of Tools release: the release path's cut (scripts/CLAUDE.md, "Release path").
#
#   scripts/release.sh <X.Y.Z[-pre]> [--dry-run] [--yes]
#
# Stamps the single-source product version everywhere, refreshes Cargo.lock, commits `release: vX.Y.Z`, tags `vX.Y.Z`,
# stamps the line's next -dev version in a commit of its own, and pushes the branch and the tag in one atomic push,
# after the maintainer's word: the prompt, or --yes when the word was given. The tag push starts
# .github/workflows/release.yml, which builds, smokes and installs the artifacts and drafts the release; publishing the
# draft is the maintainer's own act (the release skill).
#
# It runs no tests and reads no hosted run: the commit it tags passed the local gate (rc-gate.sh and
# user-manager-tests.sh) before the word, and a re-run here would only repeat it. What it checks protects an invariant
# of the tag itself, and each check prints what it found: the cut is from main, HEAD equals origin/main, the tree is
# clean, the tag sorts above every tag of its line and above the line's stamp, and the stamp after it sorts above it.
#
# --dry-run stamps, shows the diff, and restores — nothing committed.
# Run it from anywhere inside the repo; it re-roots itself.

set -euo pipefail

cd "$(dirname "$0")/.."

VERSION="" DRY_RUN=0 YES=0
for a in "$@"; do
    case "$a" in
        --dry-run) DRY_RUN=1 ;;
        --yes) YES=1 ;;
        -*) echo "unknown flag: $a" >&2; exit 2 ;;
        *) VERSION="$a" ;;
    esac
done

[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] \
    || { echo "usage: scripts/release.sh <X.Y.Z[-pre]> [--dry-run] [--yes]" >&2; exit 2; }
TAG="v$VERSION"

# ---- preflight -------------------------------------------------------------
# Every tag, candidate or final, is cut from main. Main is the -dev line: work merges to main, a cut is tagged from it,
# and the stamp after the tag moves main to the next version (below). A tag from any other branch is a release whose
# history nobody can find. `$branch` is then the one thing the push pushes — below here the script never names a
# branch again.
branch=$(git rev-parse --abbrev-ref HEAD)
case "$branch" in
    main) ;;
    *) echo "preflight: on '$branch' — every tag is cut from main" >&2; exit 1 ;;
esac
# A clean tree: the tag holds HEAD's committed tree plus the stamp, and the restore after a failure (`git restore` of
# the stamped files) would otherwise discard an uncommitted edit to one of them.
if [[ -n "$(git status --porcelain)" ]]; then
    echo "preflight: working tree not clean (see git status)" >&2; exit 1
fi
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && { echo "preflight: tag $TAG already exists" >&2; exit 1; }
# The line's ceiling, enforced by the tool rather than written down. Semver §11 compares two pre-release identifiers
# numerically only when BOTH parse as integers, and `rc9`/`rc10` are each ONE alphanumeric identifier — so they compare
# as text and `v0.6.6-rc10` sorts BELOW `v0.6.6-rc9`. So a tag must sort strictly above every tag already on its line,
# computed with the updater's own `compare_versions` (rust/updater/src/semver.rs) — the comparison every installed box
# uses to decide what "newer" means. Never a second semver implementation in shell; a second one is what hid this
# inversion. This runs before the stamp, because a mid-run failure leaves 14 files stamped.
core="${VERSION%%-*}"
# Literal globs, not a parse: the line is its final tag plus its pre-releases.
# Local tags AND origin's, because another box may have cut one minutes ago.
remote_tags=$(git ls-remote --tags origin "v$core" "v$core-*") \
    || { echo "preflight: cannot read origin's tags — the highest tag on the $core line is unknown" >&2; exit 1; }
line_tags=()
# `[^^]*` drops the `refs/tags/<tag>^{}` peel lines; a tag name cannot hold `^`. (No comment inside the substitution:
# bash 3.2 reads an apostrophe in one as an open quote.)
while read -r t; do [[ -n "$t" ]] && line_tags+=("$t"); done < <(
    { git tag --list "v$core" "v$core-*"
      printf '%s\n' "$remote_tags" | sed -n 's|^.*refs/tags/\([^^]*\)$|\1|p'
    } | sort -u
)
order_rc=0
highest=$( (cd rust && cargo run -q -p sot-updater --example vercmp -- \
    "$TAG" ${line_tags[@]+"${line_tags[@]}"}) ) || order_rc=$?
case $order_rc in
    0) ;;
    1) echo "preflight: $TAG does not sort above $highest, the highest tag on the $core line" >&2
       if [[ "$highest" == *-* ]]; then
           echo "preflight: more is not later — identifiers compare as text unless both parse as integers; appending one always sorts above: $highest.1" >&2
       else
           echo "preflight: $highest is a final release, so the $core line is closed — cut the next line" >&2
       fi
       exit 1 ;;
    *) echo "preflight: the version-order check did not run (cargo output above) — a release is never cut blind" >&2; exit 1 ;;
esac
# A cut replaces the line's current stamp (HEAD's workspace version, normally X.Y.Z-pre-dev) and must sort above it,
# or the line goes backwards: 0.6.7-alpha.1 sorts below 0.6.7-dev. Same comparator as above.
stamp=$(awk '/^\[workspace\.package\]/ { b = 1; next } /^\[/ { b = 0 }
    b && /^version[ \t]*=/ { split($0, q, "\""); print q[2]; exit }' rust/Cargo.toml)
[[ -n "$stamp" ]] || { echo "preflight: cannot read the line's stamp from rust/Cargo.toml" >&2; exit 1; }
stamp_rc=0
(cd rust && cargo run -q -p sot-updater --example vercmp -- "$TAG" "v$stamp") >/dev/null || stamp_rc=$?
case $stamp_rc in
    0) ;;
    1) echo "preflight: $TAG does not sort above v$stamp, the line's current stamp — cut a version above it (normally the stamp without -dev)" >&2; exit 1 ;;
    *) echo "preflight: the stamp order check did not run (cargo output above) — a release is never cut blind" >&2; exit 1 ;;
esac
# The line's stamp after a cut (decision 0033): X.Y.Z -> X.Y.(Z+1)-dev; a pre-release ending in a number n -> n+1,
# then -dev; any other pre-release -> .1, then -dev (rc9 -> rc9.1-dev, which sorts above rc9).
next_dev() {
    local v=$1 core pre last
    case "$v" in
        *-*) core=${v%%-*} pre=${v#*-}; last=${pre##*.}
             if [[ $last =~ ^[0-9]+$ ]]; then echo "$core-${pre%"$last"}$((last + 1))-dev"; else echo "$v.1-dev"; fi ;;
        *)   echo "${v%.*}.$(( ${v##*.} + 1 ))-dev" ;;
    esac
}
# The stamp after the tag must sort above the tag too, or the line would not move forward.
NEXT=$(next_dev "$VERSION")
next_rc=0
(cd rust && cargo run -q -p sot-updater --example vercmp -- "v$NEXT" "$TAG") >/dev/null || next_rc=$?
case $next_rc in
    0) ;;
    1) echo "preflight: the stamp after this cut, v$NEXT, does not sort above $TAG" >&2; exit 1 ;;
    *) echo "preflight: the next-stamp order check did not run (cargo output above) — a release is never cut blind" >&2; exit 1 ;;
esac
# Explicit refspec: `git fetch origin <branch>` only updates the tracking ref opportunistically, and the comparison
# below has to read a ref that is certainly fresh. HEAD must be origin's main exactly: a commit origin lacks is one no
# gate on another box can have seen, and a stale HEAD would push a tag beside a main that moved on.
git fetch -q origin "refs/heads/$branch:refs/remotes/origin/$branch" 2>/dev/null \
    || { echo "preflight: $branch is not on origin — push it first" >&2; exit 1; }
head=$(git rev-parse HEAD)
if [[ "$head" != "$(git rev-parse "origin/$branch")" ]]; then
    echo "preflight: HEAD != origin/$branch — pull/push first" >&2; exit 1
fi
echo "preflight: $branch at ${head:0:9} equals origin/$branch; the tree is clean"
echo "preflight: $TAG sorts above ${highest:-no tag} (the highest of ${#line_tags[@]} on the $core line) and above the stamp v$stamp"
echo "preflight: the stamp after it, v$NEXT, sorts above $TAG"

# ---- stamp -----------------------------------------------------------------
# One version into every file that carries it: called with the release version,
# and again after the tag with the line's next -dev version. It appends to
# STAMPED and reads JULIA_TOMLS, which the caller sets.
stamp_version() {
    local ver=$1 t
    STAMPED+=(rust/Cargo.toml rust/Cargo.lock)

    awk -v ver="$ver" '
        /^\[workspace\.package\]/ { inblk = 1 }
        /^\[/ && $0 !~ /^\[workspace\.package\]/ { inblk = 0 }
        inblk && /^version[ \t]*=/ { sub(/"[^"]*"/, "\"" ver "\"") }
        { print }
    ' rust/Cargo.toml > rust/Cargo.toml.tmp && mv rust/Cargo.toml.tmp rust/Cargo.toml

    for t in "${JULIA_TOMLS[@]}"; do
        grep -q '^version = ' "$t" || continue
        sed -E "s/^version = \"[^\"]*\"/version = \"$ver\"/" "$t" > "$t.tmp" && mv "$t.tmp" "$t"
        STAMPED+=("$t")
    done

    (cd rust && cargo update --workspace -q)   # re-sync member versions into the lock
}

STAMPED=()
# Until the commit, any failure puts the stamped files back as they were.
restore_stamp() {
    [[ ${#STAMPED[@]} -eq 0 ]] || git restore --staged --worktree -- "${STAMPED[@]}"
}
trap 'rc=$?; if [[ $rc -ne 0 ]]; then restore_stamp; echo "release: stopped after the stamp; the stamped files are restored" >&2; fi' EXIT
trap 'exit 130' INT TERM
shopt -s nullglob
JULIA_TOMLS=(Project.toml core/Project.toml julia/kernel/Project.toml
             julia/repl/Project.toml julia/pluto/Project.toml
             julia/plugins/*/Project.toml examples/plugins/*/Project.toml)
stamp_version "$VERSION"

echo "== stamped $VERSION into:"
git diff --stat -- "${STAMPED[@]}"

if [[ $DRY_RUN -eq 1 ]]; then
    restore_stamp
    echo "== after the tag the line is stamped $NEXT"
    echo "== dry run: restored, nothing committed"
    exit 0
fi

# ---- commit, tag, push -----------------------------------------------------
if [[ $YES -eq 0 ]]; then
    read -r -p "Push release $TAG and the $NEXT dev stamp to origin (the tag starts the release workflow, which drafts the release)? [y/N] " ans
    [[ "$ans" == [yY]* ]] || { trap - EXIT; restore_stamp; echo "aborted, restored"; exit 1; }
fi
git add -- "${STAMPED[@]}"
git commit -m "release: $TAG"
trap - EXIT   # the stamp is committed: a later failure is a half-cut to undo by the skill's steps
git tag -a "$TAG" -m "Ship of Tools $TAG"
STAMPED=()
stamp_version "$NEXT"
git add -- "${STAMPED[@]}"
git commit -m "version: $NEXT opens the line after $TAG [skip ci]"
# --atomic: a branch push origin rejects (a lane merged meanwhile) must not publish the tag alone. A push that failed
# is re-run with this same command, nothing else is undone.
git push --atomic origin "$branch" "$TAG"

echo "== $TAG pushed — release workflow: https://github.com/kalidke/ship-of-tools/actions/workflows/release.yml"
