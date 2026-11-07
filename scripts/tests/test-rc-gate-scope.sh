#!/usr/bin/env bash
# test-rc-gate-scope.sh: the gate scoped to the change (lib-gate-scope.sh, rc-gate.sh and user-manager-tests.sh). A table
# of paths and the piece each maps to, the widening cases among them (a path no piece owns, the build, the gate's own
# scripts, CI files, an empty diff); the scope of a real git diff; and the real --pipeline over stand-ins, which shows
# that a scoped run starts only the selected test jobs, every lint and cross check in every scope, a selected job the
# tree no longer has as a failure, and a result for no job the scope did not select. The last cases show a scoped run
# ends ALLDONE SCOPED and names its base, and a run with no scope is as before. Exit: failed checks.
set -u
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
base="${TMPDIR:-/tmp}"
work=$(mktemp -d "${base%/}/rc-gate-scope-XXXXXX") || exit 2
work=$(cd "$work" && pwd -P) || exit 2
trap 'rm -rf "${work:?}"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
source "$here/lib-gate-scope.sh" || exit 2
fail=0
check() { if (eval "$2"); then echo "PASS: $1"; else echo "FAIL: $1"; fail=$((fail + 1)); fi; }

# ---- the path-to-piece table, over a checkout with two crates
co=$work/co
mkdir -p "$co/rust/platform" "$co/rust/backend" "$co/rust/updater" "$co/rust/clippy-tests"
: > "$co/rust/platform/Cargo.toml"; : > "$co/rust/backend/Cargo.toml"; : > "$co/rust/updater/Cargo.toml"
table=(
    "rust/platform/src/lib.rs|rust platform"
    "rust/backend/tests/x.rs|rust backend"
    "rust/backend/CLAUDE.md|rust backend"
    "rust/Cargo.toml|whole rust/Cargo.toml"
    "rust/Cargo.lock|whole rust/Cargo.lock"
    "rust/clippy-tests/clippy.toml|whole rust/clippy-tests/clippy.toml"
    "rust/clippy.toml|whole rust/clippy.toml"
    "rust/gone/src/lib.rs|whole rust/gone/src/lib.rs"
    "rust/backend/src/comm/x.rs|rust backend;shell comm;julia root"
    "rust/backend/src/rows/x.rs|rust backend;shell comm;julia root"
    "rust/backend/src/agents/x.rs|rust backend;shell agents"
    "rust/backend/src/install/x.rs|rust backend;shell scripts;julia root"
    "rust/backend/src/update.rs|rust backend;shell scripts;julia root"
    "rust/backend/src/topology/x.rs|rust backend;shell scripts;julia root"
    "rust/backend/src/main.rs|rust backend;shell comm;shell agents;shell scripts;julia root"
    "rust/backend/src/clients_show.rs|rust backend;shell comm;shell agents;shell scripts;julia root"
    "rust/backend/src/server/x.rs|rust backend"
    "rust/updater/src/lib.rs|rust updater;shell scripts;julia root"
    "rust/platform/src/lib.rs|rust platform"
    "comm/lib/x.sh|shell comm"
    "comm/CLAUDE.md|shell comm"
    "agents/spawn|shell agents"
    "scripts/install.sh|shell scripts"
    "scripts/CLAUDE.md|shell scripts"
    "scripts/tests/installer-state.sh|shell scripts"
    "scripts/tests/test-pre-push.sh|shell scripts"
    "scripts/tests/rc-gate.sh|whole scripts/tests/rc-gate.sh"
    "scripts/tests/lib-gate-scope.sh|whole scripts/tests/lib-gate-scope.sh"
    "scripts/tests/test-rc-gate-scope.sh|whole scripts/tests/test-rc-gate-scope.sh"
    "scripts/tests/shards.py|whole scripts/tests/shards.py"
    "scripts/setup-publish-guard.sh|whole scripts/setup-publish-guard.sh"
    "scripts/hooks/pre-push|whole scripts/hooks/pre-push"
    ".github/test-shards.txt|whole .github/test-shards.txt"
    ".github/workflows/rust.yml|whole .github/workflows/rust.yml"
    ".github/CLAUDE.md|whole .github/CLAUDE.md"
    ".claude/settings.json|whole .claude/settings.json"
    "docs/package.md|docs docs"
    "README.md|docs docs"
    "core/src/Core.jl|julia core"
    "core/CLAUDE.md|docs docs"
    "julia/kernel/src/k.jl|julia julia/kernel"
    "julia/repl/test/runtests.jl|julia julia/repl"
    "julia/plugins/pdf-file/src/p.jl|julia julia/plugins/pdf-file"
    "julia/plugins/video-file/Project.toml|julia julia/plugins/video-file"
    "julia/plugins/markdown/src/m.jl|julia root"
    "src/ShipTools.jl|julia root"
    "Project.toml|julia root"
    "julia/pluto/Project.toml|whole julia/pluto/Project.toml"
    "examples/preview/x.jl|whole examples/preview/x.jl"
    "dev/anything|whole dev/anything"
    "cliff.toml|whole cliff.toml"
)
for row in "${table[@]}"; do
    p=${row%%|*} want_line=${row#*|}
    check "pieces of $p are $want_line" '[ "$(gate_scope_piece "$co" "$p" | sort | tr "\n" ";")" = "$(tr ";" "\n" <<< "$want_line" | sort | tr "\n" ";")" ]'
done

# ---- the scope of a real diff: sorted pieces, widening at the first path no piece owns, an empty diff is whole
repo=$work/repo
mkdir -p "$repo" && cp -r "$co/rust" "$repo/" && cd "$repo" || exit 2
git init -q -b main . && git add -A && git -c user.name=t -c user.email=t@t commit -q -m base -a --allow-empty
mkdir -p docs comm && echo a > docs/a.md && echo b > comm/b.sh && echo c > rust/platform/c.rs
git add -A && git -c user.name=t -c user.email=t@t commit -q -m one
check "a diff maps to its sorted pieces" '[ "$(gate_scope "$repo" HEAD~1)" = "docs docs
rust platform
shell comm" ]'
mkdir -p rust/backend/src/comm && echo r > rust/backend/src/comm/r.rs
git add -A && git -c user.name=t -c user.email=t@t commit -q -m comm
check "a Rust path a shell suite proves adds the suite's piece" '[ "$(gate_scope "$repo" HEAD~1)" = "julia root
rust backend
shell comm" ]'
echo m >> rust/Cargo.toml
git add -A && git -c user.name=t -c user.email=t@t commit -q -m two
check "one unowned path widens the scope to whole" '[ "$(gate_scope "$repo" HEAD~3)" = "whole rust/Cargo.toml" ]'
check "an empty diff is whole" '[ "$(gate_scope "$repo" HEAD)" = "whole (no path differs from the base)" ]'
check "an unknown base fails" '! gate_scope "$repo" nosuchbase > /dev/null 2>&1'
check "narrow is false unscoped and want selects everything" '(unset RCG_SCOPE_BASE; ! narrow && want rust x && want_kind shell)'
check "narrow want selects only the named piece" '(RCG_SCOPE_BASE=b RCG_SCOPE_SEL="rust platform"; narrow && want rust platform && ! want rust backend && want_kind rust && ! want_kind shell)'
check "a widened scope selects everything" '(RCG_SCOPE_BASE=b RCG_SCOPE_SEL="whole x"; ! narrow && want rust backend && want_kind julia)'
mkdir -p agents && echo x > agents/x.sh && git add -A && git -c user.name=t -c user.email=t@t commit -q -m sh
echo n > agents/n.md && git add -A && git -c user.name=t -c user.email=t@t commit -q -m md
ubr=$repo
cd "$here" || exit 2

real=$repo
# ---- the pipeline over stand-ins
gate=$here/rc-gate.sh
fixture() { # <name>: a stand-in repo with two shell groups, two crates with a test binary each, and result folders
    local n=$1
    repo=$work/$n logs=$work/L-$n
    mkdir -p "$repo/scripts/tests" "$repo/comm/tests" "$repo/agents/tests" "$repo/rust/platform" "$repo/rust/backend" \
        "$repo/.github" "$repo/julia/kernel" "$logs/rust" "$logs/steps" "$logs/home" "$work/tools" "$work/run" "$work/target/debug"
    : > "$repo/rust/platform/Cargo.toml"; : > "$repo/rust/backend/Cargo.toml"
    mkdir -p "$repo/rust/platform/src" && : > "$repo/rust/platform/src/lib.rs" # backend is binary-only, as the real one is
    : > "$repo/Project.toml"; : > "$repo/julia/kernel/Project.toml"
    cp "$here/shards.py" "$here/under-bash.sh" "$repo/scripts/tests/"
    : > "$repo/.github/test-shards.txt"
    for t in comm/tests/test-c agents/tests/test-a; do printf '#!/bin/sh\nexit 0\n' > "$repo/$t.sh"; done
    printf '#!/bin/sh\nexit 0\n' > "$repo/scripts/tests/test-defects-check.sh"
    printf '#!/bin/sh\necho "$*" >> "$(dirname "$0")/features-args.log"\nexit 0\n' > "$repo/scripts/tests/release-features-check.sh"
    for t in platform backend; do
        printf '#!/bin/sh\nexit 0\n' > "$work/tools/bin-$t"; chmod +x "$work/tools/bin-$t"
        printf 'bin_%s\t%s\t%s\n' "$t" "$work/tools/bin-$t" "$repo/rust/$t" >> "$logs/tests.tsv"
    done
    cat > "$work/tools/cargo" <<'STUB'
#!/bin/sh
echo "cargo $*" >> "$(dirname "$0")/cargo.log"
exit 0
STUB
    cp "$work/tools/cargo" "$work/tools/julia"
    chmod +x "$work/tools/cargo" "$work/tools/julia"
    rm -f "$work/tools/cargo.log"
}
pipeline() { # <name> <scope base or -> <scope lines>
    local n=$1 b=$2 sel=${3:-}
    [ "$b" != - ] || b=
    RCG_D=$work/$n RCG_L=$work/L-$n RCG_BUILD_RC=0 RCG_CARGO_DIR=$work/tools RCG_JULIA_DIR=$work/tools RCG_BASH32=${UB32:-} RCG_UB_PARSE=${UBP:-1} RCG_UB_SUITES=${UBS:-1} \
        RCG_SCOPE_BASE=$b RCG_SCOPE_SEL=$sel HOME=$work CARGO_TARGET_DIR=$work/target XDG_RUNTIME_DIR=$work/run \
        bash "$gate" --pipeline 4 > "$work/$n.out" 2>&1
}
rcof() { cat "$work/L-$1/$2.rc" 2> /dev/null; }

fixture none; pipeline none -
check "unscoped: the workspace doc tests run and pass" 'grep -q -- "--workspace --locked --doc" "$work/tools/cargo.log" && [ "$(rcof none rust/doc)" = 0 ]'
check "unscoped: both crates' test binaries run" '[ "$(rcof none rust/bin-platform)" = 0 ] && [ "$(rcof none rust/bin-backend)" = 0 ]'
check "unscoped: both shell groups run" '[ -e "$work/L-none/steps/comm-tests-test-c.rc" ] && [ -e "$work/L-none/steps/agents-tests-test-a.rc" ]'
check "unscoped: every julia job has a result" '[ -e "$work/L-none/steps/julia-root.rc" ] && [ -e "$work/L-none/steps/julia-julia-kernel.rc" ] && [ -e "$work/L-none/steps/julia-core.rc" ]'

fixture sc1; pipeline sc1 b $'rust platform\nshell comm'
check "scoped: the touched crate's test binary runs" '[ "$(rcof sc1 rust/bin-platform)" = 0 ]'
check "scoped: an untouched crate has no result file" '[ ! -e "$work/L-sc1/rust/bin-backend.rc" ]'
check "scoped: the touched shell group runs, an untouched one has no result" '[ -e "$work/L-sc1/steps/comm-tests-test-c.rc" ] && [ ! -e "$work/L-sc1/steps/agents-tests-test-a.rc" ]'
check "scoped: no julia job without a julia piece" '! ls "$work"/L-sc1/steps/julia-*.rc > /dev/null 2>&1'
check "scoped: all three lints still run" '[ "$(rcof sc1 steps/lint-lib-bins)$(rcof sc1 steps/lint-all-targets)$(rcof sc1 steps/lint-windows-tests)" = 000 ]'
check "scoped: both cross checks still run" '[ "$(rcof sc1 steps/win-check)" = 0 ] && [ "$(rcof sc1 steps/darwin-check)" = 0 ]'
check "scoped: the doc tests run for the touched crate alone" 'grep -q "^cargo test --manifest-path $work/sc1/rust/platform/Cargo.toml --locked --doc" "$work/tools/cargo.log" && ! grep -q "rust/backend/Cargo.toml --locked --doc" "$work/tools/cargo.log" && ! grep -q -- "--workspace --locked --doc" "$work/tools/cargo.log"'

fixture sc8; pipeline sc8 b $'rust platform\nshell scripts'
check "scoped: an installer piece gets the install smoke job, with its result and log" '[ -e "$work/L-sc8/steps/install-smoke.rc" ] && [ -e "$work/L-sc8/install-smoke.log" ]'
check "scoped: a piece other than the installer's has no install smoke" '[ ! -e "$work/L-sc1/steps/install-smoke.rc" ] && [ ! -e "$work/L-sc1/install-smoke.log" ]'
check "unscoped: the install smoke runs" '[ -e "$work/L-none/steps/install-smoke.rc" ]'

# the install smoke job's body, over a stand-in smoke that records what it was given and fails
fixture sm
mkdir -p "$work/sm/deploy"; : > "$work/sm/deploy/sotd.service"; printf '#!/bin/sh\nexit 0\n' > "$work/sm/scripts/sot-apply.sh"
printf '#!/bin/sh\necho "sotd 1.2.3 (abc)"\n' > "$work/target/debug/sotd"; cp "$work/target/debug/sotd" "$work/target/debug/sot"
cp "$work/target/debug/sotd" "$work/target/debug/sot-capsule"; chmod +x "$work/target/debug"/*
cat > "$work/sm/scripts/tests/install-smoke.sh" <<'STUB'
#!/bin/bash
{ echo "args $*"; echo "depot ${JULIA_DEPOT_PATH-unset}"; echo "tmp $TMPDIR"; echo "rewrite $GIT_CONFIG_COUNT $GIT_CONFIG_KEY_0=$GIT_CONFIG_VALUE_0"
  tar -tzf "$2"/sot-1.2.3-linux-x86_64.tar.gz; } > "$(dirname "$0")/../../smoke.rec"
echo "install-smoke: FAIL: stand-in"; exit 7
STUB
JULIA_DEPOT_PATH=/depot-from-env pipeline sm - ""
check "install smoke job: the pack holds the built binaries, the unit and the apply" 'grep -q "^args fresh-local .*/pack $work/sm$" "$work/sm/smoke.rec" && grep -q "sot-1.2.3-linux-x86_64/sotd$" "$work/sm/smoke.rec" && grep -q "/sotd.service$" "$work/sm/smoke.rec" && grep -q "/sot-apply$" "$work/sm/smoke.rec"'
check "install smoke job: it gets the gate's depot, a TMPDIR of its own and the url rewrite to the checkout" 'grep -qx "depot /depot-from-env" "$work/sm/smoke.rec" && grep -q "^tmp $work/run/rc-gate-smoke-.*/tmp$" "$work/sm/smoke.rec" && grep -qx "rewrite 1 url.$work/sm.insteadOf=https://github.com/kalidke/ship-of-tools" "$work/sm/smoke.rec"'
check "install smoke job: a failing smoke is the step's rc, named in the summary, and its folder is gone" '[ "$(rcof sm steps/install-smoke)" = 7 ] && grep -qx "install-smoke rc=7 " "$work/L-sm/summary.txt" && grep -q "FAIL: stand-in" "$work/L-sm/install-smoke.log" && ! ls -d "$work"/run/rc-gate-smoke-* > /dev/null 2>&1'

fixture sc5; pipeline sc5 b $'rust backend'
check "scoped: a binary-only crate is asked no doc test and has no doc result" '[ "$(rcof sc5 rust/bin-backend)" = 0 ] && ! grep -q -- "--doc" "$work/tools/cargo.log" && [ ! -e "$work/L-sc5/rust/doc.rc" ]'
fixture sc6; pipeline sc6 b $'rust backend\nrust platform'
check "scoped: of two crates only the one with a library target is asked doc tests" 'grep -q "platform/Cargo.toml --locked --doc" "$work/tools/cargo.log" && ! grep -q "backend/Cargo.toml --locked --doc" "$work/tools/cargo.log" && [ "$(rcof sc6 rust/doc)" = 0 ]'

fixture sc7; mkdir -p "$work/sc7/rust/libonly" && printf '[package]\nname = "x"\n\n[ lib ]\npath = "x.rs"\n' > "$work/sc7/rust/libonly/Cargo.toml"
pipeline sc7 b $'rust libonly'
check "scoped: a crate whose library is only a [lib] table is asked doc tests" 'grep -q "libonly/Cargo.toml --locked --doc" "$work/tools/cargo.log"'
fixture sc2; pipeline sc2 b $'julia julia/kernel\ndocs docs'
check "scoped julia: only the selected package has a result" '[ "$(rcof sc2 steps/julia-julia-kernel)" = 0 ] && [ ! -e "$work/L-sc2/steps/julia-root.rc" ] && [ ! -e "$work/L-sc2/steps/julia-core.rc" ]'
check "scoped julia: no crate test and no doc test" '! ls "$work"/L-sc2/rust/*.rc > /dev/null 2>&1 && ! grep -q -- "--doc" "$work/tools/cargo.log"'
check "docs only: the lints still run" '[ "$(rcof sc2 steps/lint-all-targets)" = 0 ]'
args_ok() { local n; for n in "$@"; do grep -qx -- "--prove $work/$n/rust" "$work/$n/scripts/tests/features-args.log" || return 1; done; }
check "the release features check runs unscoped, scoped to a crate and scoped to docs alone" '[ "$(rcof none steps/release-features-check)" = 0 ] && [ "$(rcof sc1 steps/release-features-check)" = 0 ] && [ "$(rcof sc2 steps/release-features-check)" = 0 ]'
check "the release features check is run with --prove over the checkout's rust folder every time" 'args_ok none sc1 sc2'
check "the defects check runs unscoped, scoped to a crate and scoped to docs alone" '[ "$(rcof none steps/scripts-tests-test-defects-check)" = 0 ] && [ "$(rcof sc1 steps/scripts-tests-test-defects-check)" = 0 ] && [ "$(rcof sc2 steps/scripts-tests-test-defects-check)" = 0 ]'

# ---- under-bash: the scans as one job, each suite as its own, only for what changed (UB32 names a bash, UBP and UBS the flags
# the main flow sets from gate_ub_decide)
ubjobs() { ls "$work/L-$1/steps" | grep -c '^under-bash.*\.rc$'; }
check "gate_scope_ub: a markdown file under agents/ sets the scans and no suite" '(gate_scope_ub "$ubr" HEAD~1; [ "$RCG_UB_PARSE$RCG_UB_SUITES" = 10 ])'
check "gate_scope_ub: a script under agents/ sets both" '(gate_scope_ub "$ubr" HEAD~2; [ "$RCG_UB_PARSE$RCG_UB_SUITES" = 11 ])'
check "gate_scope_ub: an empty diff sets neither" '(gate_scope_ub "$ubr" HEAD; [ "$RCG_UB_PARSE$RCG_UB_SUITES" = 00 ])'
check "under-bash.sh --list keeps the old order: comm, agents, scripts" '[ "$(bash "$here/under-bash.sh" --list | cut -d/ -f1 | uniq | tr "\n" " ")" = "comm agents scripts " ]'
decide() { # <base or -> <scope lines> <line>: gate_ub_decide's flags and whether it skipped
    ( [ "$1" = - ] && unset RCG_SCOPE_BASE || { RCG_SCOPE_BASE=$1; RCG_SCOPE_SEL=$2; }
      gate_ub_decide "$ubr" "${3:-}"; echo "$RCG_UB_PARSE$RCG_UB_SUITES${SKIPPED:+ skipped}" )
}
check "decide: a whole run with no line runs the scans and every suite" '[ "$(decide -)" = 11 ]'
check "decide: a whole run given a line equal to the tree skips both" '[ "$(decide - "" HEAD)" = "00 skipped" ]'
check "decide: a whole run given a line that differs runs both" '[ "$(decide - "" HEAD~2)" = 11 ]'
check "decide: a scoped run with a markdown change alone runs the scans only" '[ "$(decide HEAD~1 "shell agents")" = 10 ]'
check "decide: a scoped run with a script change runs both" '[ "$(decide HEAD~2 "shell agents")" = 11 ]'
check "decide: a scoped run with nothing under the three folders skips both" '[ "$(decide HEAD "rust backend")" = "00 skipped" ]'
fixture ub1; UB32=/bin/bash UBP=0 UBS=0 pipeline ub1 b $'rust backend\nshell comm\njulia root'
check "under-bash: a scope from rows alone emits no under-bash job" '[ "$(ubjobs ub1)" = 0 ]'
fixture ub2; UB32=/bin/bash UBS=0 pipeline ub2 b $'shell agents'
check "under-bash: a markdown change alone emits the scans job only" '[ "$(ubjobs ub2)" = 1 ] && [ -e "$work/L-ub2/steps/under-bash-parse.rc" ]'
fixture ub3; UB32=/bin/bash pipeline ub3 b $'shell agents'
check "under-bash: a script under agents emits the scans and one job per agents suite, each with its own result" '[ "$(ubjobs ub3)" = 2 ] && [ -e "$work/L-ub3/steps/under-bash-parse.rc" ] && [ -e "$work/L-ub3/steps/under-bash-agents-tests-test-a.rc" ] && [ ! -e "$work/L-ub3/steps/under-bash-comm-tests-test-c.rc" ]'
fixture ub4; UB32=/bin/bash pipeline ub4 -
check "under-bash: a whole run emits the scans and one job per suite of all three groups" '[ "$(ubjobs ub4)" = $(( $(bash "$work/ub4/scripts/tests/under-bash.sh" --list | wc -l) + 1 )) ] && [ -e "$work/L-ub4/steps/under-bash-comm-tests-test-c.rc" ] && [ -e "$work/L-ub4/steps/under-bash-agents-tests-test-a.rc" ] && [ -e "$work/L-ub4/steps/under-bash-scripts-tests-installer-state.rc" ]'

fixture sc3; pipeline sc3 b $'whole rust/Cargo.toml'
check "widened: everything runs as unscoped" '[ "$(rcof sc3 rust/bin-backend)" = 0 ] && [ -e "$work/L-sc3/steps/agents-tests-test-a.rc" ] && [ -e "$work/L-sc3/steps/julia-root.rc" ]'

check "widened: the install smoke runs" '[ -e "$work/L-sc3/steps/install-smoke.rc" ]'

fixture sc4; rm "$work/sc4/julia/kernel/Project.toml"; rm "$work/sc4/agents/tests/test-a.sh"
pipeline sc4 b $'julia julia/kernel\nshell agents'
check "scoped: a julia package the tree no longer has is a failure" '[ "$(rcof sc4 steps/julia-julia-kernel)" = 101 ]'
check "scoped: a shell group with no suite is a failure" '[ "$(rcof sc4 steps/scope-shell-agents)" = 101 ]'

# ---- the verdict, the summary lines and the refusals the main flows call, driven as they are called
check "a whole run that passed ends ALLDONE" '[ "$(unset RCG_SCOPE_BASE; gate_verdict 0)" = ALLDONE ]'
check "a scoped run that passed ends ALLDONE SCOPED, never ALLDONE" '[ "$(RCG_SCOPE_BASE=b gate_verdict 0)" = "ALLDONE SCOPED" ]'
check "a failed run ends ALLDONE FAILED scoped or not" '[ "$(RCG_SCOPE_BASE=b gate_verdict 1)" = "ALLDONE FAILED" ] && [ "$(unset RCG_SCOPE_BASE; gate_verdict 1)" = "ALLDONE FAILED" ]'
check "a scoped summary names its base and pieces" '[ "$(RCG_SCOPE_BASE=abc RCG_SCOPE_SEL="rust x
docs docs" gate_scope_summary)" = "scoped base abc
scope rust x
scope docs docs" ]'
check "a whole run's summary has no scope line" '[ -z "$(unset RCG_SCOPE_BASE; gate_scope_summary)" ]'
check "RCG_SCOPE resolves to the base commit and its pieces" '(cd "$real"; RCG_SCOPE=HEAD~2 gate_scope_resolve "$real" t && [ "$RCG_SCOPE_BASE" = "$(git rev-parse HEAD~2)" ] && [ -n "$RCG_SCOPE_SEL" ])'
check "no RCG_SCOPE leaves the run whole" '(unset RCG_SCOPE; RCG_SCOPE_BASE=stale; gate_scope_resolve "$repo" t && [ -z "$RCG_SCOPE_BASE" ])'
check "RCG_SCOPE with RCG_FULL is refused" '(RCG_SCOPE=HEAD~1 RCG_FULL=1 gate_scope_resolve "$real" t 2> /dev/null; [ $? -eq 2 ])'
check "an RCG_SCOPE that is no commit is refused" '(RCG_SCOPE=nosuch gate_scope_resolve "$real" t 2> /dev/null; [ $? -eq 2 ])'
# the entries themselves: both refuse before any container check or build, so the refusals run through the real scripts
entry() { # <script> <RCG_FULL value>: the real entry with RCG_SCOPE=<$3>; its status and standard error
    RCG_SCOPE=$3 RCG_FULL=$2 CARGO_TARGET_DIR=$work/entry-target bash "$here/$1" "$real" "$work/entry-logs" 2>&1 > /dev/null
}
for e in rc-gate.sh user-manager-tests.sh; do
    out=$(entry $e 1 HEAD~1); r=$?
    check "$e refuses RCG_SCOPE with RCG_FULL" '[ "$r" -eq 2 ] && grep -q "RCG_SCOPE and RCG_FULL together" <<< "$out"'
    out=$(entry $e "" nosuch); r=$?
    check "$e refuses an RCG_SCOPE that is no commit" '[ "$r" -eq 2 ] && grep -q "is no commit" <<< "$out"'
done

echo "failed: $fail"
exit "$fail"
