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
# platform artifacts and publishes the GitHub Release (the julia bundle is
# retired — installs clone the repo at the tag; ADR 0030 amendment).
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
# Two places a tag is legitimately cut from (owner ruling 2026-09-24):
# CANDIDATES from the release line's own branch (`fixes/<version>` or
# `rc/<version>`), which is where work accumulates from one tag until the next;
# and the FINAL release from main, which the candidate branch merges into once
# the line is ready. So main only ever advances to states that shipped, and a
# checkout following it is never mid-candidate. Nothing else is accepted,
# because a tag from an arbitrary branch is a release whose history nobody can
# find. Whichever it is, `$branch` is then the one thing the CI gate reads and
# the push pushes — below here the script never names a branch again.
branch=$(git rev-parse --abbrev-ref HEAD)
case "$branch" in
    main | fixes/* | rc/*) ;;
    *) echo "preflight: on '$branch' — a tag is cut from main (final) or from a release line's candidate branch (fixes/* or rc/*)" >&2; exit 1 ;;
esac
if [[ $ALLOW_DIRTY -eq 0 && -n "$(git status --porcelain)" ]]; then
    echo "preflight: working tree not clean (see git status; --allow-dirty to override)" >&2; exit 1
fi
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && { echo "preflight: tag $TAG already exists" >&2; exit 1; }
# Explicit refspec: `git fetch origin <branch>` only updates the tracking ref
# opportunistically, and the comparison below has to read a ref that is
# certainly fresh. A branch that isn't on origin yet fails here by design —
# CI cannot have run on what was never pushed.
git fetch -q origin "refs/heads/$branch:refs/remotes/origin/$branch" 2>/dev/null \
    || { echo "preflight: $branch is not on origin — push it and let CI run there first" >&2; exit 1; }
if [[ "$(git rev-parse HEAD)" != "$(git rev-parse "origin/$branch")" ]]; then
    echo "preflight: HEAD != origin/$branch — pull/push first" >&2; exit 1
fi
# CI is the TAG gate, not the per-PR gate (owner ruling 2026-09-08): the
# Rust and CI workflows are path-filtered (so a docs-only HEAD may have no
# Rust run of its own), so the LATEST run of each ON THE BRANCH BEING CUT must
# be green and must sit at HEAD or an ancestor of it. Anything else
# (failed, cancelled, still running, a run on a commit not in HEAD's history)
# refuses the cut; rerun a flaky leg (`gh run rerun <id> --failed`), never
# skip it. gh is required — a release is never cut blind. A candidate branch
# only has runs if the workflows trigger on it, or if someone dispatched them
# there (`gh workflow run <wf> --ref <branch>`); an empty list REFUSES the cut
# rather than waving it through, which is the whole reason this reads the
# branch instead of main.
command -v gh >/dev/null 2>&1 || { echo "preflight: gh not found — CI on $branch cannot be verified" >&2; exit 1; }
for wf in rust.yml CI.yml; do
    # The version-stamp commit's run is skipped by the workflows' own `if`
    # (2026-09-18), so read past skipped runs to the latest real verdict.
    run="$(gh run list --branch "$branch" --workflow "$wf" --limit 5 --json headSha,status,conclusion,databaseId \
        --jq '[.[] | select(.conclusion != "skipped")][0] | "\(.headSha) \(.status) \(.conclusion) \(.databaseId)"' 2>/dev/null || true)"
    [[ -n "$run" && "$run" != "null null null null" ]] || { echo "preflight: no $wf run on $branch — push and let CI finish, or: gh workflow run $wf --ref $branch" >&2; exit 1; }
    read -r run_sha run_status run_concl run_id <<<"$run"
    if [[ "$run_concl" != "success" ]]; then
        echo "preflight: latest $wf run on $branch (id $run_id, ${run_sha:0:8}) is $run_status/$run_concl — rerun it (gh run rerun $run_id --failed) and cut when green" >&2; exit 1
    fi
    git merge-base --is-ancestor "$run_sha" HEAD \
        || { echo "preflight: latest green $wf run is at ${run_sha:0:8}, which is not in HEAD's history" >&2; exit 1; }
done

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
    # SOT_TEST_REQUIRE_USER_MANAGER=1 (ADR 0043 decision 32, Codex
    # SHOULD-FIX): a release is always cut on the backend host, which has
    # a real `systemd --user` manager -- so the real survival proof
    # (capsule_supervisor_survives_a_real_user_service_stop) must actually
    # RUN here, panicking rather than silently SKIPPED:-ing forever the
    # way a bare hosted CI runner (no user manager) is allowed to.
    (cd rust && SOT_TEST_REQUIRE_USER_MANAGER=1 cargo test --workspace --locked)
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
git push origin "$branch" "$TAG"

echo "== $TAG pushed — release workflow: https://github.com/kalidke/ship-of-tools/actions/workflows/release.yml"
