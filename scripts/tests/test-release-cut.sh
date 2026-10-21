#!/usr/bin/env bash
# test-release-cut.sh -- release.sh, the cut, in throwaway repos: the branch guard (a tag is cut from main and from no
# other branch), the refusals before the stamp (a dirty tree, a HEAD origin lacks, an order the comparator refuses), the
# restore after a failure past the stamp, the dry run, and a whole cut into a scratch origin: the release commit holds
# only the stamped files, the annotated tag sits on it, the next -dev stamp is a commit of its own, and the branch and
# the tag reach origin together or not at all. The cut runs no tests and reads no hosted run: stubs of cargo and gh
# record every call, and neither `cargo test` nor gh may be among them. The stub cargo answers the version comparison
# (the updater's `compare_versions` has its own tests in rust/updater) and the lock refresh. Last, the workflows: their
# one release write is release.yml's draft, on a tag ref, and none pushes a tag.
#
# Run: scripts/tests/test-release-cut.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/release-cut.XXXXXX")"
trap 'rm -rf "${WORK:?}"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
fails=0

check() {  # <description> <expected> <actual>
    if [ "$2" = "$3" ]; then
        printf '    ok   %s\n' "$1"
    else
        printf '    FAIL %s\n      expected: %s\n      actual:   %s\n' "$1" "$2" "$3"
        fails=$((fails + 1))
    fi
}

REPO="$WORK/repo"
mkdir -p "$REPO/scripts"
cp "$ROOT/scripts/release.sh" "$REPO/scripts/release.sh"
(
    cd "$REPO"
    git init -q
    git add scripts/release.sh
    git -c user.name=test -c user.email=test@example.invalid -c commit.gpgsign=false commit -q -m "fixture: release.sh"
)

# first_refusal <branch>: the first `preflight:` line release.sh prints for a cut from <branch>, empty when there is none.
first_refusal() {
    (cd "$REPO" && git checkout -q -B "$1" && bash scripts/release.sh 0.6.6 2>&1) | grep -m1 '^preflight:' || true
}

echo "  release.sh branch guard"
check "a cut from main passes the guard: its first refusal is the missing origin" \
    "preflight: cannot read origin's tags — the highest tag on the 0.6.6 line is unknown" "$(first_refusal main)"
check "a cut from fixes/0.6.6 is refused by the guard" \
    "preflight: on 'fixes/0.6.6' — every tag is cut from main" "$(first_refusal fixes/0.6.6)"
check "a cut from rc/0.6.6 is refused by the guard" \
    "preflight: on 'rc/0.6.6' — every tag is cut from main" "$(first_refusal rc/0.6.6)"

# A fixture repo with an origin and a stamp of 0.6.6-dev, and stubs of cargo and gh on PATH that record every call.
STUBS="$WORK/stubs"
CALLS="$WORK/calls"
mkdir -p "$STUBS"
: > "$CALLS"
cat > "$STUBS/cargo" <<STUB
#!/bin/sh
echo "cargo \$*" >> "$CALLS"
case "\$1" in
    run) echo "\${STUB_VERCMP_HIGHEST:-}"; exit "\${STUB_VERCMP_RC:-0}" ;;
    update) exit "\${STUB_CARGO_UPDATE_RC:-0}" ;;
esac
exit 0
STUB
cat > "$STUBS/gh" <<STUB
#!/bin/sh
echo "gh \$*" >> "$CALLS"
exit 1
STUB
chmod +x "$STUBS/cargo" "$STUBS/gh"

ORIGIN="$WORK/origin.git"
CUT="$WORK/cut"
git init -q --bare "$ORIGIN"
mkdir -p "$CUT/scripts" "$CUT/rust"
cp "$ROOT/scripts/release.sh" "$CUT/scripts/release.sh"
printf '[workspace.package]\nversion = "0.6.6-dev"\n' > "$CUT/rust/Cargo.toml"
printf 'version = 3\n' > "$CUT/rust/Cargo.lock"
printf 'name = "Fixture"\nversion = "0.6.6-dev"\n' > "$CUT/Project.toml"
(
    cd "$CUT"
    git init -q
    git symbolic-ref HEAD refs/heads/main
    git config user.name test
    git config user.email test@example.invalid
    git config commit.gpgsign false
    git config tag.gpgSign false
    git remote add origin "$ORIGIN"
    git add .
    git commit -q -m "fixture"
    git push -q origin main
)

# cut_out [VAR=value...] -- <release.sh args>: the cut's output and exit status, run with the stubs first on PATH.
cut_out() {
    local envs=()
    while [ "$1" != -- ]; do envs+=("$1"); shift; done
    shift
    (cd "$CUT" && env PATH="$STUBS:$PATH" ${envs[@]+"${envs[@]}"} bash scripts/release.sh "$@" 2>&1; echo "exit $?")
}
first() { printf '%s\n' "$1" | grep -m1 "^$2" || true; }
tree_state() { (cd "$CUT" && git status --porcelain | tr '\n' ' '); }
stamp_of() { sed -n 's/^version = "\(.*\)"$/\1/p'; }

echo "  release.sh refusals before the stamp"
touch "$CUT/stray"
out=$(cut_out -- 0.6.6)
check "a dirty tree is refused" "preflight: working tree not clean (see git status)" "$(first "$out" preflight:)"
rm -f "$CUT/stray"
out=$(cut_out STUB_VERCMP_RC=1 STUB_VERCMP_HIGHEST=v0.6.6-rc9 -- 0.6.6)
check "a cut the comparator puts below its line is refused" \
    "preflight: v0.6.6 does not sort above v0.6.6-rc9, the highest tag on the 0.6.6 line" "$(first "$out" preflight:)"
check "  and nothing was stamped" "" "$(tree_state)"
(cd "$CUT" && git commit -q --allow-empty -m "not on origin")
out=$(cut_out -- 0.6.6)
check "a HEAD origin lacks is refused" "preflight: HEAD != origin/main — pull/push first" \
    "$(printf '%s\n' "$out" | grep '^preflight:' | tail -n1)"
check "  and nothing was stamped" "" "$(tree_state)"
(cd "$CUT" && git reset -q --hard origin/main)

echo "  release.sh restore and dry run"
out=$(cut_out STUB_CARGO_UPDATE_RC=1 -- 0.6.6)
check "a failure after the stamp ends the cut with a failing status" "exit 1" "$(printf '%s\n' "$out" | tail -n1)"
check "  and says the restore ran" "release: stopped after the stamp; the stamped files are restored" "$(first "$out" release:)"
check "  and left the tree clean" "" "$(tree_state)"
check "  with the stamp back at 0.6.6-dev" "0.6.6-dev" "$(stamp_of < "$CUT/rust/Cargo.toml")"
out=$(cut_out STUB_VERCMP_HIGHEST=v0.6.6-rc9 -- 0.6.6 --dry-run)
check "a dry run ends with exit 0" "exit 0" "$(printf '%s\n' "$out" | tail -n1)"
check "  printing each check's evidence" \
    "preflight: main at $(git -C "$CUT" rev-parse --short=9 HEAD) equals origin/main; the tree is clean" "$(first "$out" "preflight: main")"
check "  the order's among them" \
    "preflight: v0.6.6 sorts above v0.6.6-rc9 (the highest of 0 on the 0.6.6 line) and above the stamp v0.6.6-dev" \
    "$(first "$out" "preflight: v0.6.6 sorts")"
check "  and the stamp after the tag" "== after the tag the line is stamped 0.6.7-dev" "$(first "$out" "== after")"
check "  and restores the stamp" "" "$(tree_state)"
check "  and tags nothing" "" "$(git -C "$CUT" tag --list)"
# A release commit the repository refuses (a hook here; a signing error alike) is a failure before the commit: the
# stamp is restored, nothing is tagged, and a second cut finds a clean tree.
printf '#!/bin/sh\nexit 1\n' > "$CUT/.git/hooks/pre-commit"; chmod +x "$CUT/.git/hooks/pre-commit"
out=$(cut_out STUB_VERCMP_HIGHEST=v0.6.6-rc9 -- 0.6.6 --yes)
rm -f "$CUT/.git/hooks/pre-commit"
check "a refused release commit ends the cut with a failing status" "exit 1" "$(printf '%s\n' "$out" | tail -n1)"
check "  and says the restore ran" "release: stopped after the stamp; the stamped files are restored" "$(first "$out" release:)"
check "  and left the tree clean" "" "$(tree_state)"
check "  and tags nothing" "" "$(git -C "$CUT" tag --list)"

echo "  release.sh cut into a scratch origin"
before=$(git -C "$CUT" rev-parse HEAD)
out=$(cut_out -- 0.6.6 --yes)
check "the cut ends with exit 0" "exit 0" "$(printf '%s\n' "$out" | tail -n1)"
rel=$(git -C "$ORIGIN" rev-parse -q --verify 'refs/tags/v0.6.6^{commit}' || true)
check "origin holds the tag" "release: v0.6.6" "$(git -C "$ORIGIN" log -1 --format=%s "${rel:-HEAD}")"
check "  as an annotated tag" "tag" "$(git -C "$ORIGIN" cat-file -t refs/tags/v0.6.6 2> /dev/null || true)"
check "  on a commit whose parent is the HEAD the cut was given" "$before" "$(git -C "$ORIGIN" rev-parse "${rel:-HEAD}^" 2> /dev/null || true)"
check "  holding only the stamped files" "Project.toml rust/Cargo.toml" \
    "$(git -C "$ORIGIN" diff --name-only "$before" "${rel:-HEAD}" | tr '\n' ' ' | sed 's/ $//')"
check "  stamped 0.6.6" "0.6.6" "$(git -C "$ORIGIN" show v0.6.6:rust/Cargo.toml | stamp_of)"
check "origin's main is the next stamp's commit, the tag's child" \
    "version: 0.6.7-dev opens the line after v0.6.6 [skip ci] / $rel" \
    "$(git -C "$ORIGIN" log -1 --format='%s / %P' main)"
check "  stamped 0.6.7-dev" "0.6.7-dev" "$(git -C "$ORIGIN" show main:rust/Cargo.toml | stamp_of)"
check "the cut ran no tests" "" "$(grep '^cargo test' "$CALLS" || true)"
check "the cut read no hosted run" "" "$(grep '^gh ' "$CALLS" || true)"

echo "  release.sh push is atomic"
# Origin refuses main (another box's merge would do the same): with --atomic the tag must not land alone either.
printf '#!/bin/sh\n[ "$1" = refs/heads/main ] && exit 1\nexit 0\n' > "$ORIGIN/hooks/update"
chmod +x "$ORIGIN/hooks/update"
main_before=$(git -C "$ORIGIN" rev-parse main)
out=$(cut_out -- 0.6.7 --yes)
check "a push origin refuses ends the cut with a failing status" "exit 1" "$(printf '%s\n' "$out" | tail -n1)"
check "  and origin has no tag" "" "$(git -C "$ORIGIN" tag --list v0.6.7)"
check "  and origin's main did not move" "$main_before" "$(git -C "$ORIGIN" rev-parse main)"

echo "  no workflow tags or releases on its own"
# Every release action but the draft is a person's command. The workflows' one write of a release is release.yml's
# `gh release create --draft`, in a job that runs only on a tag ref; no workflow pushes a tag or uses a release action.
WF="$ROOT/.github/workflows"
code() { grep -vE '^[[:space:]]*#' "$1"; }  # <workflow>: its lines but comments
writes() {  # each release write a workflow's code holds, as `<file>: <command>`
    local f
    for f in "$WF"/*.yml; do
        code "$f" | grep -oE 'gh release (create|edit|delete|upload|delete-asset)' | sed "s|^|$(basename "$f"): |" || true
    done
}
check "the one release write in the workflows is release.yml's create" "release.yml: gh release create" "$(writes)"
check "  which creates a draft" '--draft\' "$(code "$WF/release.yml" | grep -A1 'gh release create' | tail -n1 | tr -d ' ')"
check "  in a job that runs only on a tag ref" "    if: startsWith(github.ref, 'refs/tags/')" \
    "$(sed -n '/^  draft:$/,/^    steps:$/p' "$WF/release.yml" | grep '^    if:')"
check "no workflow pushes a tag or runs a release action" "" \
    "$(for f in "$WF"/*.yml; do code "$f" | grep -E 'git (tag|push)|action-gh-release|create-release|upload-release-asset' || true; done)"

if [ "$fails" -gt 0 ]; then
    echo "FAILED: $fails check(s)"
    exit 1
fi
echo "all checks passed"
