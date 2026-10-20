#!/usr/bin/env bash
# release.sh — cut a Ship of Tools release (ADR 0030 §3, Phase B).
#
#   scripts/release.sh <X.Y.Z[-pre]> [--dry-run] [--yes] [--skip-tests] [--allow-dirty]
#
# Stamps the single-source product version everywhere, refreshes Cargo.lock,
# optionally regenerates CHANGELOG.md (when git-cliff is installed — the
# release workflow generates the release notes regardless), runs the test
# suite, then commits `release: vX.Y.Z`, tags `vX.Y.Z`, stamps the line's next
# -dev version in a commit of its own, and pushes both. The tag push triggers
# .github/workflows/release.yml, which builds the
# platform artifacts and creates the GitHub Release as a draft (the julia bundle is
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
# Every tag, candidate or final, is cut from main. The maintainer, 2026-10-16:
# "so just like our julia packages, once we tag we move main to next version as
# dev." So main is the -dev line: work merges to main, a cut is tagged from it,
# and the stamp after the tag moves main to the next version (below). Nothing
# else is accepted, because a tag from an arbitrary branch is a release whose
# history nobody can find. `$branch` is then the one thing the CI gate reads and
# the push pushes — below here the script never names a branch again.
branch=$(git rev-parse --abbrev-ref HEAD)
case "$branch" in
    main) ;;
    *) echo "preflight: on '$branch' — every tag is cut from main" >&2; exit 1 ;;
esac
if [[ $ALLOW_DIRTY -eq 0 && -n "$(git status --porcelain)" ]]; then
    echo "preflight: working tree not clean (see git status; --allow-dirty to override)" >&2; exit 1
fi
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && { echo "preflight: tag $TAG already exists" >&2; exit 1; }
# The line's ceiling, enforced by the tool rather than written down. Semver §11
# compares two pre-release identifiers numerically only when BOTH parse as
# integers, and `rc9`/`rc10` are each ONE alphanumeric identifier — so they
# compare as text and `v0.6.6-rc10` sorts BELOW `v0.6.6-rc9`. Two sessions have
# already restated that as prose ("the line stops at rcN") and drawn the wrong
# conclusion from it, so the rule lives here: a tag must sort strictly above
# every tag already on its line, computed with the updater's own
# `compare_versions` (rust/updater/src/semver.rs) — the comparison every
# installed box uses to decide what "newer" means. Never a second semver
# implementation in shell; a second one is what hid this inversion.
# This runs before the stamp, because a mid-run failure leaves 14 files stamped.
core="${VERSION%%-*}"
# Literal globs, not a parse: the line is its final tag plus its pre-releases.
# Local tags AND origin's, because another box may have cut one minutes ago.
remote_tags=$(git ls-remote --tags origin "v$core" "v$core-*") \
    || { echo "preflight: cannot read origin's tags — the highest tag on the $core line is unknown" >&2; exit 1; }
line_tags=()
while read -r t; do [[ -n "$t" ]] && line_tags+=("$t"); done < <(
    { git tag --list "v$core" "v$core-*"
      # `[^^]*` drops the `refs/tags/<tag>^{}` peel lines; a tag name can't hold `^`.
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
# Explicit refspec: `git fetch origin <branch>` only updates the tracking ref
# opportunistically, and the comparison below has to read a ref that is
# certainly fresh. A branch that isn't on origin yet fails here by design —
# CI cannot have run on what was never pushed.
git fetch -q origin "refs/heads/$branch:refs/remotes/origin/$branch" 2>/dev/null \
    || { echo "preflight: $branch is not on origin — push it and let CI run there first" >&2; exit 1; }
if [[ "$(git rev-parse HEAD)" != "$(git rev-parse "origin/$branch")" ]]; then
    echo "preflight: HEAD != origin/$branch — pull/push first" >&2; exit 1
fi
# CI is the TAG gate, not the per-PR gate: the
# LATEST run of each ON THE BRANCH BEING CUT must be green and must sit at
# HEAD. Anything else (failed, cancelled, still running, a run on any other
# commit) refuses the cut; rerun a flaky leg (`gh run rerun <id> --failed`),
# never skip it. gh is required — a release is never cut blind. Dispatch both
# workflows there first (`gh workflow run <wf> --ref main`): push runs are
# path-filtered, so HEAD may have none of its own, and the cut's runs are
# dispatched ones. An empty list REFUSES the cut rather than waving it through.
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
    [[ "$run_sha" == "$(git rev-parse HEAD)" ]] \
        || { echo "preflight: latest green $wf run is at ${run_sha:0:8}, not at HEAD — dispatch it there: gh workflow run $wf --ref $branch" >&2; exit 1; }
done

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
shopt -s nullglob
JULIA_TOMLS=(Project.toml core/Project.toml julia/kernel/Project.toml
             julia/repl/Project.toml julia/pluto/Project.toml
             julia/plugins/*/Project.toml examples/plugins/*/Project.toml)
stamp_version "$VERSION"

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
    [[ ${#NEW_FILES[@]} -gt 0 ]] && rm -f -- "${NEW_FILES[@]:?}"
    echo "== after the tag the line is stamped $NEXT"
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
    #
    # This box's own SOT_ env (a live session's SOT_SOCKET, SOT_COMM_HOME,
    # SOT_RELAY_ENDPOINT, ...) must never reach a test daemon -- the
    # harness-isolation lane's whole point is a daemon that only ever
    # discovers a registry/socket a test itself minted. Unset every
    # ambient SOT_-prefixed var first (SOT_TEST_REQUIRE_USER_MANAGER=1
    # below is a test knob, not host state, so it's set fresh after);
    # DBUS_SESSION_BUS_ADDRESS/XDG_RUNTIME_DIR are left alone -- the real
    # user-manager survival test needs a reachable `systemd --user` -- and
    # so are HOME/PATH, both still needed by git and gh.
    (
        cd rust
        for v in $(compgen -v SOT_); do unset "$v"; done
        SOT_TEST_REQUIRE_USER_MANAGER=1 cargo test --workspace --locked
    )
fi

# ---- commit, tag, push -----------------------------------------------------
if [[ $YES -eq 0 ]]; then
    read -r -p "Push release $TAG and the $NEXT dev stamp to origin (commit + tag -> CI builds and creates the draft release)? [y/N] " ans
    [[ "$ans" == [yY]* ]] || { git restore -- "${STAMPED[@]}"; echo "aborted, restored"; exit 1; }
fi

# ${arr[@]+...} (not ${arr[@]:-}) — an empty array must expand to NOTHING;
# the :- form yields one empty string, which git add rejects as a pathspec.
git add -- "${STAMPED[@]}" ${NEW_FILES[@]+"${NEW_FILES[@]}"}
git commit -m "release: $TAG"
git tag -a "$TAG" -m "Ship of Tools $TAG"
STAMPED=()
stamp_version "$NEXT"
git add -- "${STAMPED[@]}"
git commit -m "version: $NEXT opens the line after $TAG [skip ci]"
# --atomic: a branch push origin rejects (a lane merged meanwhile) must not publish the tag alone. A push that failed
# is re-run with this same command, nothing else is undone.
git push --atomic origin "$branch" "$TAG"

echo "== $TAG pushed — release workflow: https://github.com/kalidke/ship-of-tools/actions/workflows/release.yml"
