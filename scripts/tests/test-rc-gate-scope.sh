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
mkdir -p "$co/rust/platform" "$co/rust/backend" "$co/rust/updater" "$co/rust/frontend" "$co/rust/clippy-tests"
: > "$co/rust/frontend/Cargo.toml"; : > "$co/rust/platform/Cargo.toml"; : > "$co/rust/backend/Cargo.toml"; : > "$co/rust/updater/Cargo.toml"
table=(
    "rust/platform/src/lib.rs|rust platform;tree rust;gate scope-suite"
    "rust/backend/tests/x.rs|rust backend;tree rust;gate scope-suite"
    "rust/backend/CLAUDE.md|rust backend"
    "rust/backend/Cargo.toml|rust backend;tree rust;shell comm;shell agents;shell scripts;julia root;gate scope-suite"
    "rust/frontend/build.rs|rust frontend;tree rust;gate scope-suite"
    "rust/frontend/src/ui/x.rs|rust frontend;tree rust;gate scope-suite"
    "rust/frontend/CLAUDE.md|rust frontend"
    "rust/Cargo.toml|whole rust/Cargo.toml"
    "rust/Cargo.lock|whole rust/Cargo.lock"
    "rust/clippy-tests/clippy.toml|whole rust/clippy-tests/clippy.toml"
    "rust/clippy.toml|whole rust/clippy.toml"
    "rust/gone/src/lib.rs|whole rust/gone/src/lib.rs"
    "rust/backend/src/comm/x.rs|rust backend;shell comm;julia root;tree rust;gate scope-suite"
    "rust/backend/src/rows/x.rs|rust backend;shell comm;julia root;tree rust;gate scope-suite"
    "rust/backend/src/agents/x.rs|rust backend;shell agents;tree rust;gate scope-suite"
    "rust/backend/src/install/x.rs|rust backend;shell scripts;julia root;tree rust;gate scope-suite"
    "rust/backend/src/update.rs|rust backend;shell scripts;julia root;tree rust;gate scope-suite"
    "rust/backend/src/topology/x.rs|rust backend;shell scripts;julia root;tree rust;gate scope-suite"
    "rust/backend/src/main.rs|rust backend;shell comm;shell agents;shell scripts;julia root;tree rust;gate scope-suite"
    "rust/backend/src/clients_show.rs|rust backend;shell comm;shell agents;shell scripts;julia root;tree rust;gate scope-suite"
    "rust/backend/src/server/x.rs|rust backend;tree rust;gate scope-suite"
    "rust/updater/src/lib.rs|rust updater;shell scripts;julia root;tree rust;gate scope-suite"
    "rust/platform/src/lib.rs|rust platform;tree rust;gate scope-suite"
    "comm/lib/x.sh|shell comm;tree comm-lib;tree stage-bin"
    "comm/CLAUDE.md|shell comm"
    "agents/claude/bin/sot-fe|shell agents;julia root"
    "agents/claude/sot-install/SKILL.md|shell agents;julia root"
    "agents/codex/bin/ccx|shell agents;julia root;tree ccx"
    "comm/adapters/codex/skills/sot-comm/SKILL.md|shell comm;julia root"
    "comm/legacy-skills.txt|shell comm;julia root"
    "comm/launcher-hashes.txt|shell comm;julia root"
    "agents/spawn|shell agents;tree stage-bin"
    "scripts/install.sh|shell scripts"
    "scripts/CLAUDE.md|shell scripts"
    "scripts/tests/installer-state.sh|shell scripts;gate scope-suite"
    "scripts/tests/test-pre-push.sh|shell scripts;gate scope-suite"
    "scripts/tests/test-x.sh|shell scripts;gate scope-suite"
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
    "julia/repl/test/runtests.jl|julia julia/repl;tree julia-repl"
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
gate scope-suite
rust platform
shell comm
tree rust" ]'
mkdir -p rust/backend/src/comm && echo r > rust/backend/src/comm/r.rs
git add -A && git -c user.name=t -c user.email=t@t commit -q -m comm
check "a Rust path a shell suite proves adds the suite's piece" '[ "$(gate_scope "$repo" HEAD~1)" = "gate scope-suite
julia root
rust backend
shell comm
tree rust" ]'
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
treetargets() { # <name>: every target of gate_tree_tests as a stand-in binary that lists the filters asked of it and logs its arguments
    local n=$1 t
    for t in $(gate_tree_tests | awk '{print $3}' | sort -u); do
        cat > "$work/tools/bin-$t" <<STUB
#!/bin/sh
case "\$*" in *--ignored*) cat "\$0.ignored" 2> /dev/null; exit 0 ;; esac
case "\$1" in --list) cat "\$0.list"; exit 0 ;; esac
echo "\$*" >> "$logs/tree-$t.args"
case "\$1" in --exact) printf 'running 1 test\ntest %s ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n' "\$2" ;; esac
exit 0
STUB
        chmod +x "$work/tools/bin-$t"
        gate_tree_tests | awk -v t="$t" '$3 == t { for (i = 4; i <= NF; i++) print "mod::" $i "x: test" }' > "$work/tools/bin-$t.list"
        printf '%s\t%s\t%s\n' "$t" "$work/tools/bin-$t" "$repo/rust/platform" >> "$work/L-$n/tests.tsv"
    done
}

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
    # a stand-in for this very suite: the gate's own job for it must not start the real one again; it records where it ran and its environment
    printf '#!/bin/sh\n{ echo "ran $0"; env; } > "$(dirname "$0")/scope-suite.rec"\nexit 0\n' > "$repo/scripts/tests/test-rc-gate-scope.sh"
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
    treetargets "$n"
}
pipeline() { # <name> <scope base or -> <scope lines>
    local n=$1 b=$2 sel=${3:-}
    [ "$b" != - ] || b=
    RCG_D=$work/$n RCG_L=$work/L-$n RCG_BUILD_RC=0 RCG_CARGO_DIR=$work/tools RCG_JULIA_DIR=$work/tools RCG_BASH32=${UB32:-} RCG_UB_PARSE=${UBP:-1} RCG_UB_SUITES=${UBS:-1} \
        RCG_SCOPE_BASE=$b RCG_SCOPE_SEL=$sel HOME=$work CARGO_TARGET_DIR=$work/target XDG_RUNTIME_DIR=$work/run \
        bash "$gate" --pipeline ${CAP:-4} > "$work/$n.out" 2>&1
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
# ---- the tests that read files outside their own crate: a scoped run whose paths yield a `tree` piece runs that piece's tests
# whatever crates it selected (gate_tree_tests)
treefix() { fixture "$1"; }
treefix tr1; pipeline tr1 b $'rust backend\ntree rust'
check "tree: a scoped run with a Rust path runs each Rust-reading test under a crate it did not select" '[ "$(rcof tr1 rust/bin-sot_platform__mod__test_scan__x)" = 0 ] && [ "$(rcof tr1 rust/bin-isolation_guards)" = 0 ] && [ "$(rcof tr1 rust/bin-dependents)" = 0 ]'
check "tree: the platform scan runs by its filter, the capsule guards whole, and no other piece's test runs" 'grep -q "^--exact mod::test_scan::x " "$work/L-tr1/tree-sot_platform.args" && grep -qx "" "$work/L-tr1/tree-isolation_guards.args" && [ ! -e "$work/L-tr1/tree-unit_pin.args" ] && [ ! -e "$work/L-tr1/tree-sotd.args" ]'
treefix tr2; pipeline tr2 b $'docs docs\nshell comm'
check "tree: a scope with no tree piece runs none of them" '! ls "$work"/L-tr2/rust/*.rc > /dev/null 2>&1 && ! ls "$work"/L-tr2/tree-*.args > /dev/null 2>&1'
treefix tr3; pipeline tr3 b $'rust platform\ntree rust'
check "tree: a selected crate's own run is not doubled by the tree job" '[ ! -e "$work/L-tr3/rust/bin-sot_platform__mod__test_scan__x.rc" ] && [ "$(rcof tr3 rust/bin-platform)" = 0 ]'
treefix tr4; rm -f "$work/L-tr4/tests.tsv"; pipeline tr4 b $'rust backend\ntree rust'
check "tree: a test target the build no longer has is a failure" '[ "$(rcof tr4 steps/tree-dependents)" = 101 ] && grep -qx "tree-missing dependents" "$work/L-tr4/summary.txt"'
check "unscoped: the tree jobs add nothing beside the ordinary binaries" '[ ! -e "$work/L-none/rust/bin-sot_platform__mod__test_scan__x.rc" ]'
treefix tr8; : > "$work/tools/bin-sot_platform.list"; pipeline tr8 b $'rust backend\ntree rust'
check "tree: a filter that selects no listed test is a failure, and its job never runs" '[ "$(rcof tr8 steps/tree-sot_platform-1)" = 101 ] && grep -q "^tree-missing sot_platform: no test is selected by test_scan::" "$work/L-tr8/summary.txt" && [ ! -e "$work/L-tr8/tree-sot_platform.args" ]'
treefix tr11; : > "$work/tools/bin-sot_platform.list"; pipeline tr11 -
check "tree: a whole run fails a row that selects no test, naming the row" '[ "$(rcof tr11 steps/tree-sot_platform-1)" = 101 ] && grep -q "^tree-missing sot_platform: no test is selected by test_scan::" "$work/L-tr11/summary.txt"'
treefix tr12; : > "$work/tools/bin-sot_platform.list"; pipeline tr12 b $'shell comm'
check "tree: a scoped run whose paths yield no tree piece still fails a row that selects no test" '[ "$(rcof tr12 steps/tree-sot_platform-1)" = 101 ]'
treefix tr13; printf 'gate-x test unit_pin\n' > "$work/tr13/.github/test-shards.txt"; pipeline tr13 b $'tree docs-install'
check "tree: a whole-binary row a gate-* shard runs whole is skipped and says so" 'grep -qx "tree-skipped unit_pin: a gate-\* shard runs it whole" "$work/L-tr13/summary.txt" && [ ! -e "$work/L-tr13/rust/bin-unit_pin.rc" ]'
treefix tr5; pipeline tr5 b $'docs docs\ntree docs-install\ntree docs-keybindings'
check "tree: a read docs file selects its tests alone, whole binaries and filtered ones" '[ "$(rcof tr5 rust/bin-unit_pin)" = 0 ] && grep -q "^--exact mod::ui::input::keybindings::tests::x " "$work/L-tr5/tree-sot.args" && [ ! -e "$work/L-tr5/tree-sot_chat.args" ] && [ ! -e "$work/L-tr5/tree-dependents.args" ]'
treefix tr6; pipeline tr6 b $'shell comm\ntree stage-bin\ntree comm-lib'
check "tree: two pieces of one binary's readers run it once" '[ "$(rcof tr6 rust/bin-comm_cli)" = 0 ] && [ "$(rcof tr6 rust/bin-shell_dial)" = 0 ] && grep -q "^--exact mod::client_wire_tests::x " "$work/L-tr6/tree-sot_protocol.args" && [ ! -e "$work/L-tr6/tree-selection.args" ]'
treefix tr7; pipeline tr7 b $'julia julia/repl\ntree julia-repl'
check "tree: a row's tests run each by its exact name" 'grep -q "^--exact mod::sidecars::repl::x " "$work/L-tr7/tree-sotd.args"'
treefix tr9; printf 'gate-wgl bin sotd wgl\n' > "$work/tr9/.github/test-shards.txt"; echo "mod::sidecars::repl::wgl: test" >> "$work/tools/bin-sotd.list"; pipeline tr9 b $'rust backend\ntree julia-repl'
check "tree: a test only the release gate runs is left out, as the ordinary run leaves it" 'grep -q "^--exact mod::sidecars::repl::x " "$work/L-tr9/tree-sotd.args" && ! grep -q wgl "$work/L-tr9/tree-sotd.args"'
treefix tr10; echo "mod::sidecars::repl::ign: test" | tee -a "$work/tools/bin-sotd.list" > "$work/tools/bin-sotd.ignored"; pipeline tr10 b $'rust backend\ntree julia-repl'
check "tree: an ignored test is left out and the others run" 'grep -q "^--exact mod::sidecars::repl::x " "$work/L-tr10/tree-sotd.args" && ! grep -q "::ign " "$work/L-tr10/tree-sotd.args"'

# the files outside rust/ that a Rust test reads select the piece that runs it (a representative path of each piece)
treepath() { gate_scope_piece "$co" "$1" | grep '^tree ' | sort | tr '\n' ';'; }
tree_cases=(
    "rust/platform/src/x.rs|tree rust;"
    "docs/src/ref/keybindings.md|tree docs-keybindings;"
    "logo-wordmark-dark.png|tree logo;"
    "logo-dark.png|"
    "logo.png|"
    "logo.ico|"
    "docs/src/ref/config.md|tree docs-config;"
    "docs/src/start/install.md|tree docs-install;"
    "docs/ENROLLING-A-HOST.md|tree docs-install;"
    "docs/INSTALL-AGENT.md|tree docs-install;"
    "docs/src/guide/troubleshooting.md|tree docs-install;"
    "docs/src/ref/other.md|"
    "comm/lib/comm-lib.sh|tree comm-lib;tree stage-bin;"
    "comm/lib/CLAUDE.md|"
    "comm/registry/x|tree stage-bin;"
    "comm/work_state/hooks/x|tree stage-bin;"
    "comm/mail/x|tree stage-bin;"
    "comm/bin-folders.txt|tree stage-bin;"
    "comm/tests/stage-bin.sh|tree stage-bin;"
    "comm/adapters/codex/hooks.json|tree codex-hooks;"
    "comm/tests/test-x.sh|"
    "agents/sot-fe/x|tree stage-bin;"
    "agents/spawn/x|tree stage-bin;"
    "agents/worktree/x|tree stage-bin;"
    "agents/sot-selection-hook.sh|tree selection-hook;tree stage-bin;"
    "agents/codex/bin/ccx|tree ccx;"
    "agents/tests/test-x.sh|"
    "julia/repl/src/x.jl|tree julia-repl;"
    "julia/kernel/src/x.jl|"
    "julia/kernel/Project.toml|"
    "scripts/sot-install-layout.ps1|"
    "scripts/sot-local-daemon.ps1|tree local-daemon;"
    "scripts/sot-apply.sh|"
    "scripts/install.sh|"
    "deploy/sotd.service|"
)
for row in "${tree_cases[@]}"; do
    p=${row%%|*} want_line=${row#*|}
    check "tree pieces of $p are '$want_line'" '[ "$(treepath "$p")" = "$want_line" ]'
done
check "every tree piece the list names is yielded by a path of the cases above" 'for pc in $(gate_tree_tests | awk "{print \$1}" | sort -u); do case $(printf "%s\n" "${tree_cases[@]}") in *"tree $pc;"*) ;; *) echo "no case yields tree $pc" >&2; exit 1 ;; esac; done'

# every Rust file that reads outside its own crate is in the list. Four forms are searched: a walk of the workspace
# (rust_sources, production_sources, a `cargo metadata` call); an include_str! or include_bytes! whose path, resolved from
# its file, leaves the file's own crate (another crate, or outside rust/); and a file that names CARGO_MANIFEST_DIR or the
# daemon-lifetime helper repo_file and holds a `../` or a `.parent()`; and a `"../..."` literal as the argument of a file
# call (read_to_string, File::open, Path::new, ...). A reader that finds its file another way (a walk through a helper of
# another name, a lookup through resource_dir) is not found, and its writer adds it to the list.
tree_listed() { gate_tree_tests | awk '{print $2}'; }
tree_root=$(cd "$here/../.." && pwd)
check "the search needs python3, and a missing one fails the suite" 'command -v python3 > /dev/null'
tree_walk=$(cd "$tree_root" && grep -rlE 'rust_sources\(|production_sources\(|\["metadata",' --include=*.rs rust)
tree_inc=$(cd "$tree_root" && python3 -I - <<'PY'
import os, re
for d, _, names in os.walk('rust'):
    crate = os.sep.join(d.split(os.sep)[:2]) + os.sep
    for n in names:
        if not n.endswith('.rs'):
            continue
        f = os.path.join(d, n)
        text = open(f, encoding='utf-8').read()
        for rel in re.findall(r'include_(?:str|bytes)!\(\s*"([^"]+)"', text):
            if not (os.path.normpath(os.path.join(d, rel)) + os.sep).startswith(crate):
                print(f)
                break
PY
) || tree_inc=SEARCH-FAILED
tree_up=$(cd "$tree_root" && grep -rlE 'CARGO_MANIFEST_DIR|repo_file\(' --include=*.rs rust | xargs grep -lE '\.\./|\.parent\(\)')
# a path literal relative to the crate folder (cargo and the gate run tests there), as the argument of a file call
tree_rel=$(cd "$tree_root" && grep -rlE '(read_to_string|read_dir|File::open|fs::read|Path::new|PathBuf::from|canonicalize)\([[:space:]]*&?"\.\./' --include=*.rs rust)
tree_out=$(printf '%s\n%s\n%s\n%s\n' "$tree_walk" "$tree_inc" "$tree_up" "$tree_rel" | grep -v '^$' | LC_ALL=C sort -u)
# read outside the crate but not by a test this gate can scope, each with its reason
IFS= read -r -d '' tree_exempt <<'EXEMPT'
rust/platform/src/test_scan.rs the walker's own definition
rust/backend/tests/daemon_boot.rs scans the backend's own tests alone
rust/platform/src/host/paths.rs the resource lookup of a running daemon, not a test
rust/frontend/build.rs the window's icon at build time, and the workspace build runs in every scope
rust/frontend/src/ui/render/surface.rs compiles the logo files in, which the workspace build covers in every scope; the decoding test is surface_tests.rs, listed
rust/frontend/src/ui/init/mod.rs compiles logo.png in at build time, as above; no test decodes it
rust/frontend/src/ui/result_tests.rs decodes logo.png, but the module is built only with the feature test-window-progress, which the gate never sets
rust/frontend/src/ui/preview_return_tests.rs uses logo.png for a parked picture in the same native fixtures, built only with test-window-progress
rust/frontend/src/ui/preview/replies_notice_tests.rs compiles logo.png in as a page's picture for one fixture, which only the native fixtures run, and those are built only with test-window-progress
rust/frontend/src/net/transport/golden_tests.rs reads its own crate's testdata
rust/frontend/src/ui/preview/markdown/tests.rs reads its own crate's testdata
rust/backend/tests/ancestors.rs its tests are Windows-only; the gate runs on Linux, and the hosted Windows run covers it
rust/updater/src/pending_tests.rs the one test that reads scripts/sot-install-layout.ps1 is Windows-only, as above
rust/backend/src/install/unix_tests.rs reads the checkout's scripts/lib and deploy, both paths that widen the run
rust/backend/tests/contained_job.rs scripts/tests/in-container.sh, a gate script, so its change widens the run
rust/backend/src/install/publish_tests.rs the publish guard's scripts, which widen the run
rust/backend/tests/daemon_lifetime/done.rs julia/pluto, which widens the run, and a release-gate test
EXEMPT
tree_exempt=${tree_exempt%$'\n'}
# listed files the search cannot find (they read through an item another file defines), each with its reason
IFS= read -r -d '' tree_byhand <<'HAND'
rust/frontend/src/ui/render/surface_tests.rs decodes LOGO_WORDMARK_PNG, which surface.rs compiles in from logo-wordmark-dark.png
HAND
tree_byhand=${tree_byhand%$'\n'}
check "the tree-reading list holds every Rust file that walks the workspace or reads outside its crate (the exemptions each name their reason)" '
    [ "$tree_inc" != SEARCH-FAILED ] || { echo "the include search failed" >&2; exit 1; }
    missing=$(printf "%s\n" "$tree_out" | while read -r f; do
        tree_listed | grep -qxF "$f" || printf "%s\n" "$tree_exempt" | grep -q "^$f " || echo "$f"; done)
    [ -z "$missing" ] || { echo "$missing" >&2; false; }'
check "every file the list names is found by the search or named as read by hand" '
    [ -n "$tree_out" ] || exit 1
    for f in $(tree_listed); do
        printf "%s\n" "$tree_out" | grep -qxF "$f" || printf "%s\n" "$tree_byhand" | grep -q "^$f " || { echo "$f" >&2; exit 1; }; done'
fixture sc2; pipeline sc2 b $'julia julia/kernel\ndocs docs'
check "scoped julia: only the selected package has a result" '[ "$(rcof sc2 steps/julia-julia-kernel)" = 0 ] && [ ! -e "$work/L-sc2/steps/julia-root.rc" ] && [ ! -e "$work/L-sc2/steps/julia-core.rc" ]'
check "scoped julia: no crate test and no doc test" '! ls "$work"/L-sc2/rust/*.rc > /dev/null 2>&1 && ! grep -q -- "--doc" "$work/tools/cargo.log"'
check "docs only: the lints still run" '[ "$(rcof sc2 steps/lint-all-targets)" = 0 ]'
args_ok() { local n; for n in "$@"; do grep -qx -- "--prove $work/$n/rust" "$work/$n/scripts/tests/features-args.log" || return 1; done; }
check "the release features check runs unscoped, scoped to a crate and scoped to docs alone" '[ "$(rcof none steps/release-features-check)" = 0 ] && [ "$(rcof sc1 steps/release-features-check)" = 0 ] && [ "$(rcof sc2 steps/release-features-check)" = 0 ]'
check "the release features check is run with --prove over the checkout's rust folder every time" 'args_ok none sc1 sc2'
check "the defects check runs unscoped, scoped to a crate and scoped to docs alone" '[ "$(rcof none steps/scripts-tests-test-defects-check)" = 0 ] && [ "$(rcof sc1 steps/scripts-tests-test-defects-check)" = 0 ] && [ "$(rcof sc2 steps/scripts-tests-test-defects-check)" = 0 ]'
# this suite is a job of the gate's own: every whole run, and a scoped one that yields `gate scope-suite` (a Rust source or crate
# manifest, a file under scripts/tests/); never for docs or Julia alone. The job in a fixture is a stand-in, so it never starts itself again
check "the scope suite's job runs unscoped" '[ "$(rcof none steps/scripts-tests-test-rc-gate-scope)" = 0 ]'
fixture sg1; pipeline sg1 b $'gate scope-suite\nrust platform'
check "the scope suite's job runs in a scope that yields gate scope-suite" '[ "$(rcof sg1 steps/scripts-tests-test-rc-gate-scope)" = 0 ]'
check "the scope suite's job is not run for a crate piece, a docs piece or a Julia piece alone" '[ ! -e "$work/L-sc1/steps/scripts-tests-test-rc-gate-scope.rc" ] && [ ! -e "$work/L-sc2/steps/scripts-tests-test-rc-gate-scope.rc" ] && [ ! -e "$work/sc1/scripts/tests/scope-suite.rec" ] && [ ! -e "$work/sc2/scripts/tests/scope-suite.rec" ]'
fixture sg2; GATE_TOOLS_DIR=/sg RCG_TOOLS=/sg RCG_NODE_DIR=/sg RCG_SCOPE=HEAD RCG_FULL_MARK=1 JULIA_DEPOT_PATH=/sg CARGO_HOME=/sg pipeline sg2 b $'gate scope-suite'
check "the scope suite's job runs the checkout's suite and gets none of the run's RCG_, GATE_, CARGO_ or JULIA_ variables" 'grep -qx "ran $work/sg2/scripts/tests/test-rc-gate-scope.sh" "$work/sg2/scripts/tests/scope-suite.rec" && ! grep -q "^\(RCG_\|GATE_\|CARGO_\|JULIA_\)" "$work/sg2/scripts/tests/scope-suite.rec" && grep -q "^HOME=$work/L-sg2/home$" "$work/sg2/scripts/tests/scope-suite.rec"'
fixture sg3; printf '#!/bin/sh\necho "FAIL: stand-in"\nexit 1\n' > "$work/sg3/scripts/tests/test-rc-gate-scope.sh"; pipeline sg3 -
check "a failing scope suite is the run's failure: its rc, its summary line and its own log" '[ "$(rcof sg3 steps/scripts-tests-test-rc-gate-scope)" = 1 ] && grep -qx "scripts-tests-test-rc-gate-scope rc=1 " "$work/L-sg3/summary.txt" && grep -q "FAIL: stand-in" "$work/L-sg3/scripts-tests-test-rc-gate-scope.log"'

# ---- under-bash: the scans as one job, each suite as its own, only for what changed (UB32 names a bash, UBP and UBS the flags
# the main flow sets from gate_ub_decide)
ubjobs() { ls "$work/L-$1/steps" | grep -c '^under-bash.*\.rc$'; }
check "gate_scope_ub: a markdown file under agents/ sets the scans and no suite" '(gate_scope_ub "$ubr" HEAD~1; [ "$RCG_UB_PARSE$RCG_UB_SUITES" = 10 ])'
check "gate_scope_ub: a script under agents/ sets both" '(gate_scope_ub "$ubr" HEAD~2; [ "$RCG_UB_PARSE$RCG_UB_SUITES" = 11 ])'
check "gate_scope_ub: an empty diff sets neither" '(gate_scope_ub "$ubr" HEAD; [ "$RCG_UB_PARSE$RCG_UB_SUITES" = 00 ])'
check "under-bash.sh --list keeps the old order: comm, agents, scripts" '[ "$(bash "$here/under-bash.sh" --list | cut -d/ -f1 | uniq | tr "\n" " ")" = "comm agents scripts " ]'
mkdir -p "$ubr/scripts/tests"
for v in 3.2.57 3.2.48 5.2.0; do printf '#!/bin/sh\necho %s\n' "$v" > "$work/fake$v"; chmod +x "$work/fake$v"; done
stub() { # ok | bad | empty | five | old: the bash32.sh the main flow's decision builds with; each call is logged
    case $1 in
        ok) out="echo $work/fake3.2.57" ;; bad) out="exit 1" ;; empty) out=: ;; five) out="echo $work/fake5.2.0" ;;
        old) out="echo $work/fake3.2.48" ;;
    esac
    printf '#!/bin/sh\necho x >> "%s"\n%s\n' "$work/b32calls" "$out" > "$ubr/scripts/tests/bash32.sh"
}
decide() { # <base or -> <scope lines> <line>: the main flow's own decision lines (rc-gate.sh, from LINE= to the export of
    # RCG_BASH32) over the stub; the flags, whether it skipped, whether RCG_BASH32 was set, the builds made
    : > "$work/b32calls"
    ( if [ "$1" = - ]; then unset RCG_SCOPE_BASE; else RCG_SCOPE_BASE=$1 RCG_SCOPE_SEL=$2; fi
      D=$ubr; set -- a b c "${3:-}"
      eval "$(sed -n '/^LINE=/,/^export RCG_BASH32$/p' "$here/rc-gate.sh")"
      echo "$RCG_UB_PARSE$RCG_UB_SUITES${SKIPPED:+ skipped}${RCG_BASH32:+ bash} builds=$(wc -l < "$work/b32calls" | tr -d ' ')" )
}
stub ok
check "decide: a whole run with no line runs the scans and every suite, with one bash 3.2" '[ "$(decide -)" = "11 bash builds=1" ]'
check "decide: a whole run given a line equal to the tree skips both and builds none" '[ "$(decide - "" HEAD)" = "00 skipped builds=0" ]'
check "decide: a whole run given a line that differs runs both" '[ "$(decide - "" HEAD~2)" = "11 bash builds=1" ]'
check "decide: a scoped run with a markdown change alone runs the scans only" '[ "$(decide HEAD~1 "shell agents")" = "10 bash builds=1" ]'
check "decide: a scoped run with a script change runs both" '[ "$(decide HEAD~2 "shell agents")" = "11 bash builds=1" ]'
check "decide: a scoped run with nothing under the three folders skips both and builds none" '[ "$(decide HEAD "rust backend")" = "00 skipped builds=0" ]'
for s in bad empty five old; do
    stub $s
    check "decide: a $s bash 3.2 ends the run (2) naming it, whenever a job is selected, given a line or not" '
        fails() { out=$("$@" 2>&1); [ $? -eq 2 ] && grep -q "no bash 3.2.57" <<< "$out"; }
        fails decide - "" && fails decide - "" HEAD~2 && fails decide HEAD~1 "shell agents"'
done
stub ok
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

check "widened: the install smoke and the scope suite run" '[ -e "$work/L-sc3/steps/install-smoke.rc" ] && [ "$(rcof sc3 steps/scripts-tests-test-rc-gate-scope)" = 0 ]'

fixture sc4; rm "$work/sc4/julia/kernel/Project.toml"; rm "$work/sc4/agents/tests/test-a.sh"
pipeline sc4 b $'julia julia/kernel\nshell agents'
check "scoped: a julia package the tree no longer has is a failure" '[ "$(rcof sc4 steps/julia-julia-kernel)" = 101 ]'
check "scoped: a shell group with no suite is a failure" '[ "$(rcof sc4 steps/scope-shell-agents)" = 101 ]'

# ---- longest first: the order jobs start in, one at a time (CAP=1), from the times file the last run kept
times=$work/target/rc-gate-times.tsv
ordfixture() { # <name> [extra]: test binaries that log their start (the third has a build hash in its name, plus <extra> more)
    fixture "$1"
    local t x i
    for t in bin-platform bin-backend; do
        printf '#!/bin/sh\necho %s >> "%s"\nexit 0\n' "$t" "$work/order-$1" > "$work/tools/$t"
    done
    x=$work/tools/bin-gamma-0123456789abcdef
    printf '#!/bin/sh\necho gamma >> "%s"\nexit 0\n' "$work/order-$1" > "$x"; chmod +x "$x"
    printf 'bin_gamma\t%s\t%s\n' "$x" "$repo/rust/platform" >> "$work/L-$1/tests.tsv"
    for ((i = 1; i <= ${2:-0}; i++)); do
        x=$work/tools/$(printf 'bin-k%03d' "$i")
        printf '#!/bin/sh\necho k%03d >> "%s"\nexit 0\n' "$i" "$work/order-$1" > "$x"; chmod +x "$x"
        printf 'k%03d\t%s\t%s\n' "$i" "$x" "$repo/rust/platform" >> "$work/L-$1/tests.tsv"
    done
}
starts() { tr '\n' ' ' < "$work/order-$1"; }
rm -f "$times"
ordfixture o1 100; CAP=1 pipeline o1 -
check "longest first: with no times file 103 jobs keep the producer's order, past the 10th and the 100th" '[ "$(starts o1)" = "bin-platform bin-backend gamma $(printf "k%03d " $(seq 1 100))" ]'
printf 'bin:bin_backend\t50\nbin:bin_platform\t10\nbin:bin_gamma\t1\nbin:gone\t999\ngarbage\nbin:x\tabc\nbin:y\t5\textra\n' > "$times"
ordfixture o2; CAP=1 pipeline o2 -
check "longest first: B named longer than A starts before A; the key names the target, not the hashed file" '[ "$(starts o2)" = "bin-backend bin-platform gamma " ]'
check "longest first: a job the file names that the run lacks, and a malformed line, add no job and no error" '[ "$(wc -l < "$work/order-o2")" -eq 3 ] && [ "$(ls "$work/L-o2/rust" | grep -cE "^bin-(platform|backend|gamma-[0-9a-f]+)\.rc$")" -eq 3 ] && ! grep -q "rc-gate:" "$work/o2.out"'
printf 'bin:bin_backend\t50\nbin:bin_platform\t10\n' > "$times"
ordfixture o3; CAP=1 pipeline o3 -
check "longest first: a job the file does not name starts before every named one" '[ "$(starts o3)" = "gamma bin-backend bin-platform " ]'
check "longest first: every job starts once and writes its result once" '[ -z "$(sort "$work/order-o3" | uniq -d)" ] && [ "$(wc -l < "$work/order-o3")" -eq 3 ] && [ "$(rcof o3 rust/bin-gamma-0123456789abcdef)" = 0 ]'
printf 'bin:bin_backend\t99999999999\nbin:bin_platform\t10\nbin:bin_gamma\tabc\n' > "$times"
ordfixture o5; CAP=1 pipeline o5 -
check "longest first: a value that is not under ten digits, on a job the run has, leaves it unmeasured" '[ "$(starts o5)" = "bin-backend gamma bin-platform " ]'
rm -f "$times"
# keys are unique per job: two test binaries whose file names differ only in the hash, two suites with one basename
ordfixture o6; printf '#!/bin/sh\nexit 0\n' > "$work/o6/agents/tests/test-c.sh"
x=$work/tools/bin-gamma-fedcba9876543210; printf '#!/bin/sh\necho gamma2 >> "%s"\nexit 0\n' "$work/order-o6" > "$x"; chmod +x "$x"
printf 'bin_gamma2\t%s\t%s\n' "$x" "$work/o6/rust/platform" >> "$work/L-o6/tests.tsv"
CAP=1 pipeline o6 -
check "keys: every job of a whole run has a key of its own, none shared by a hash-stripped name or a basename" '[ "$(cat "$work"/L-o6/times/* | wc -l)" -ge 10 ] && [ -z "$(cut -f1 "$work"/L-o6/times/* | sort | uniq -d)" ] && grep -q "^bin:bin_gamma2	" "$work"/L-o6/times/* && grep -q "^shell:comm/tests/test-c.sh	" "$work"/L-o6/times/* && grep -q "^shell:agents/tests/test-c.sh	" "$work"/L-o6/times/* && ! cut -f1 "$work"/L-o6/times/* | grep -q ":$" && grep -q "^julia	" "$work"/L-o6/times/* && grep -q "^install-smoke	" "$work"/L-o6/times/* && grep -q "^cargo-chain	" "$work"/L-o6/times/*'
# the dispatcher records each job's seconds under its key, a failed job's included; the run's end merges them
fixture tm; printf '#!/bin/sh\nsleep 2\nexit 3\n' > "$work/tools/bin-fail-0123456789abcdef"; chmod +x "$work/tools/bin-fail-0123456789abcdef"
printf 'bin_fail\t%s/tools/bin-fail-0123456789abcdef\t%s\n' "$work" "$work/tm" >> "$work/L-tm/tests.tsv"
RCG_D=$work/tm RCG_L=$work/L-tm RCG_CARGO_DIR=$work/tools RCG_JULIA_DIR=$work/tools HOME=$work CARGO_TARGET_DIR=$work/target XDG_RUNTIME_DIR=$work/run \
    bash "$gate" --job "$(printf 'bin\tbin-fail-0123456789abcdef\t%s\t%s' "$work/tools/bin-fail-0123456789abcdef" "$work/tm")"
check "a job's seconds are recorded under its key, after it ran, a failed job's too" '[ "$(rcof tm rust/bin-fail-0123456789abcdef)" = 3 ] && grep -qP "^bin:bin_fail\t[2-9]$" "$work"/L-tm/times/*'
mergefn=$(sed -n '/^merge_times() {/,/^}/p' "$gate")
check "the run's end writes this run's jobs over the file and keeps a job that did not run" '(
    eval "$mergefn"
    L=$work/L-tm; RCG_TIMES=$work/merged.tsv
    printf "bin:bin_fail\t99\nbin:kept\t7\nbad line\n" > "$RCG_TIMES"
    merge_times
    [ "$(cut -f1 "$RCG_TIMES" | tr "\n" " ")" = "bin:bin_fail bin:kept " ] && grep -qP "^bin:kept\t7$" "$RCG_TIMES" && ! grep -q "99" "$RCG_TIMES")'
check "the first run on a target, with no times file, writes this run's jobs" '(
    eval "$mergefn"
    L=$work/L-tm; RCG_TIMES=$work/first.tsv; rm -f "$RCG_TIMES"
    merge_times
    grep -qP "^bin:bin_fail\t[2-9]$" "$RCG_TIMES")'

# a split binary's per-test jobs key on the target and the test, each its own. comm_wake is also a target of the
# tree-reading list, and a target has one row: this stand-in takes the place of that list's own and lists its tests too
fixture o7
x=$work/tools/comm_wake-0123456789abcdef
printf '#!/bin/sh\ncase "$*" in *--ignored*) ;; *--list*) echo "a_test: test"; echo "b_test: test"; cat "%s" ;; esac\nexit 0\n' "$work/tools/bin-comm_wake.list" > "$x"; chmod +x "$x"
grep -v $'^comm_wake\t' "$work/L-o7/tests.tsv" > "$work/L-o7/tests.tsv.one"; mv "$work/L-o7/tests.tsv.one" "$work/L-o7/tests.tsv"
printf 'comm_wake\t%s\t%s\n' "$x" "$work/o7/rust/platform" >> "$work/L-o7/tests.tsv"
CAP=1 pipeline o7 -
check "keys: a split binary's jobs are keyed by target and test, distinct, with no hash" '[ "$(cut -f1 "$work"/L-o7/times/* | grep -c "^one:comm_wake:[ab]_test$")" -eq 2 ] && [ -z "$(cut -f1 "$work"/L-o7/times/* | sort | uniq -d)" ] && ! cut -f1 "$work"/L-o7/times/* | grep -q "^one:.*[0-9a-f]\{16\}"'
# a lookup never makes an entry: two unmeasured jobs of one key stay unmeasured beside a measured one
orderfn=$(sed -n '/^timekey() {/,/^}/p;/^order_listing() {/,/^}/p' "$gate")
check "order_listing: reading a job's time makes no entry, so an unmeasured key stays unmeasured" '(
    eval "$orderfn"
    RCG_TIMES=$work/ol.tsv; printf "lint:b\t5\n" > "$RCG_TIMES"
    [ "$(order_listing <<< "$(printf "lint\tA\nlint\tA\nlint\tb")" | cut -f2 | tr "\n" " ")" = "A A b " ])'
check "merge_times: a value of ten digits or more, or a bad line, is not kept; nine digits is" '(
    eval "$mergefn"
    L=$work/L-tm; RCG_TIMES=$work/dig.tsv; printf "bin:big\t12345678901\nbin:ok\t123456789\n" > "$RCG_TIMES"
    merge_times
    [ "$(cut -f1 "$RCG_TIMES" | tr "\n" " ")" = "bin:bin_fail bin:ok " ])'

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
