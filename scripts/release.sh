#!/usr/bin/env bash
# release.sh — cut a Ship of Tools release (ADR 0030 §3, Phase B).
#
#   scripts/release.sh <X.Y.Z[-pre]> [--dry-run] [--yes] [--skip-tests] [--allow-dirty]
#
# Stamps the single-source product version everywhere, refreshes Cargo.lock,
# optionally regenerates CHANGELOG.md (when git-cliff is installed — the
# release workflow generates the release notes regardless), runs the test
# suite, then commits `release: vX.Y.Z`, tags `vX.Y.Z`, and pushes. The
# tag push triggers .github/workflows/release.yml, which builds the
# platform artifacts + julia bundle and publishes the GitHub Release.
#
# --dry-run stamps, shows the diff, and restores — nothing committed.
# Run it from anywhere inside the repo; it re-roots itself.

set -euo pipefail

cd "$(dirname "$0")/.."

VERSION="" DRY_RUN=0 YES=0 SKIP_TESTS=0 ALLOW_DIRTY=0
for a in "$@"; do
    case "$a" in
        --dry-run) DRY_RUN=1 ;;
        --yes) YES=1 ;;
        --skip-tests) SKIP_TESTS=1 ;;
        --allow-dirty) ALLOW_DIRTY=1 ;;
        -*) echo "unknown flag: $a" >&2; exit 2 ;;
        *) VERSION="$a" ;;
    esac
done

[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] \
    || { echo "usage: scripts/release.sh <X.Y.Z[-pre]> [--dry-run] [--yes] [--skip-tests] [--allow-dirty]" >&2; exit 2; }
TAG="v$VERSION"

# ---- preflight -------------------------------------------------------------
branch=$(git rev-parse --abbrev-ref HEAD)
[[ "$branch" == "main" ]] || { echo "preflight: on '$branch', releases cut from main" >&2; exit 1; }
if [[ $ALLOW_DIRTY -eq 0 && -n "$(git status --porcelain)" ]]; then
    echo "preflight: working tree not clean (see git status; --allow-dirty to override)" >&2; exit 1
fi
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && { echo "preflight: tag $TAG already exists" >&2; exit 1; }
git fetch -q origin main
if [[ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]]; then
    echo "preflight: HEAD != origin/main — pull/push first" >&2; exit 1
fi

# ---- stamp -----------------------------------------------------------------
STAMPED=(rust/Cargo.toml rust/Cargo.lock)

awk -v ver="$VERSION" '
    /^\[workspace\.package\]/ { inblk = 1 }
    /^\[/ && $0 !~ /^\[workspace\.package\]/ { inblk = 0 }
    inblk && /^version[ \t]*=/ { sub(/"[^"]*"/, "\"" ver "\"") }
    { print }
' rust/Cargo.toml > rust/Cargo.toml.tmp && mv rust/Cargo.toml.tmp rust/Cargo.toml

shopt -s nullglob
JULIA_TOMLS=(Project.toml core/Project.toml julia/kernel/Project.toml
             julia/repl/Project.toml julia/pluto/Project.toml
             julia/plugins/*/Project.toml examples/plugins/*/Project.toml)
for t in "${JULIA_TOMLS[@]}"; do
    grep -q '^version = ' "$t" || continue
    sed -i -E "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" "$t"
    STAMPED+=("$t")
done

(cd rust && cargo update --workspace -q)   # re-sync member versions into the lock

NEW_FILES=()
if command -v git-cliff >/dev/null 2>&1; then
    git ls-files --error-unmatch CHANGELOG.md >/dev/null 2>&1 \
        && STAMPED+=(CHANGELOG.md) || NEW_FILES+=(CHANGELOG.md)
    git-cliff --tag "$TAG" -o CHANGELOG.md
else
    echo "note: git-cliff not installed — skipping CHANGELOG.md (release notes are generated in CI)"
fi

echo "== stamped $VERSION into:"
git diff --stat -- "${STAMPED[@]}"

if [[ $DRY_RUN -eq 1 ]]; then
    git restore -- "${STAMPED[@]}"
    [[ ${#NEW_FILES[@]} -gt 0 ]] && rm -f -- "${NEW_FILES[@]}"
    echo "== dry run: restored, nothing committed"
    exit 0
fi

# ---- test ------------------------------------------------------------------
if [[ $SKIP_TESTS -eq 0 ]]; then
    (cd rust && cargo test --workspace --locked)
fi

# ---- commit, tag, push -----------------------------------------------------
if [[ $YES -eq 0 ]]; then
    read -r -p "Push release $TAG to origin (commit + tag -> CI builds + publishes)? [y/N] " ans
    [[ "$ans" == [yY]* ]] || { git restore -- "${STAMPED[@]}"; echo "aborted, restored"; exit 1; }
fi

# ${arr[@]+...} (not ${arr[@]:-}) — an empty array must expand to NOTHING;
# the :- form yields one empty string, which git add rejects as a pathspec.
git add -- "${STAMPED[@]}" ${NEW_FILES[@]+"${NEW_FILES[@]}"}
git commit -m "release: $TAG"
git tag -a "$TAG" -m "Ship of Tools $TAG"
git push origin main "$TAG"

echo "== $TAG pushed — release workflow: https://github.com/kalidke/ship-of-tools/actions/workflows/release.yml"
