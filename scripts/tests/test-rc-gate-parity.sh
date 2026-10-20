#!/usr/bin/env bash
# test-rc-gate-parity.sh: rc-gate.sh runs what hosted CI runs on Linux. Its three clippy jobs (rust.yml's lib-bins line,
# the all-targets line under rust/clippy-tests, and that line for x86_64-pc-windows-gnu) go red on a stand-in workspace
# holding a test-children violation, an un-allowed std::fs::copy, and a violation only cfg(windows) test code holds; and
# its under-bash job goes red on a script that parses only under bash 4. The jobs run as rc-gate runs them (--job) over
# stand-in trees with the real cargo and a real bash 3.2.57 (scripts/tests/bash32.sh, built once and cached). The
# pipeline is run over a stub cargo to show the gate emits every job with each clippy line's arguments, and that a line
# equal to the tree skips the bash 3.2 job. Needs cargo with clippy; the windows-gnu cases print SKIP where that target's
# standard library is not installed. Exit: failed checks.
set -u
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
gate=$here/rc-gate.sh
root=$(cd "$here/../.." && pwd)
base="${TMPDIR:-/tmp}"
work=$(mktemp -d "${base%/}/rc-gate-parity-XXXXXX") || exit 2
trap 'rm -rf "${work:?}"' EXIT
fail=0
check() { if eval "$2"; then echo "PASS: $1"; else echo "FAIL: $1"; fail=$((fail + 1)); fi; }
cargo=$(command -v cargo || echo "$HOME/.cargo/bin/cargo")
[ -x "$cargo" ] || { echo "test-rc-gate-parity.sh: no cargo" >&2; exit 2; }
# a hosted runner has no /scratch: its bash 3.2 is built under RUNNER_TEMP, the folder rust.yml's bash32-linux job uses
bash32=$(bash "$here/bash32.sh" ${RUNNER_TEMP:+"$RUNNER_TEMP/bash-3.2.57"}) || exit 2
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null CARGO_TARGET_DIR=$work/target
sysroot=$("$(dirname "$cargo")/rustc" --print sysroot 2> /dev/null)
[ -d "$sysroot/lib/rustlib/x86_64-pc-windows-gnu" ] && haswin=1 || haswin=0

# the stand-in workspace's crates: one library and one integration test each, the features rust.yml's lines name
crate() { # <name> <features...>
  local n=$1; shift
  mkdir -p "$repo/rust/$n/src" "$repo/rust/$n/tests"
  { printf '[package]\nname = "%s"\nversion = "0.0.0"\nedition = "2021"\n\n[features]\n' "$n"
    for f in "$@"; do printf '%s = []\n' "$f"; done; } > "$repo/rust/$n/Cargo.toml"
  echo 'pub fn f() {}' > "$repo/rust/$n/src/lib.rs"
  echo '#[test]
fn t() {}' > "$repo/rust/$n/tests/t.rs"
}
# new_repo <name> -> sets repo: a workspace with the real clippy configs and a lockfile
new_repo() {
  repo=$work/$1
  mkdir -p "$repo/rust/clippy-tests" "$repo/comm/tests" "$repo/agents/tests" "$repo/scripts/tests"
  cp "$root/rust/clippy.toml" "$repo/rust/clippy.toml"
  cp "$root/rust/clippy-tests/clippy.toml" "$repo/rust/clippy-tests/clippy.toml"
  printf '[workspace]\nresolver = "2"\nmembers = ["sot-log", "sot-backend", "sot-frontend", "vt100-ctt"]\n' > "$repo/rust/Cargo.toml"
  crate sot-log; crate sot-backend daemon-lifetime-faults; crate sot-frontend test-pane-timing test-window-close; crate vt100-ctt
  (cd "$repo/rust" && "$cargo" generate-lockfile --offline > /dev/null 2>&1) || { echo "no lockfile" >&2; exit 2; }
  mkdir -p "$work/L-$1/home" "$work/L-$1/steps" "$work/L-$1/rust" "$work/julia-bin"
}
# job <repo-name> <job line>: one rc-gate job over the case's repo; its result file is steps/<name>.rc
job() {
  RCG_D=$work/$1 RCG_L=$work/L-$1 RCG_CARGO_DIR=$(dirname "$cargo") RCG_JULIA_DIR=$work/julia-bin RCG_BASH32=$bash32 \
    bash "$gate" --job "$2" > /dev/null 2>&1
}
rcof() { cat "$work/L-$1/steps/$2.rc" 2> /dev/null; }
# redby <repo> <step>: the step failed, and its log names a disallowed method (cargo exits 101 on a clippy error)
redby() {
  local rc; rc=$(rcof "$1" "$2")
  [ -n "$rc" ] && [ "$rc" != 0 ] && [ "$rc" != unrun ] && grep -q "disallowed method" "$work/L-$1/$2.log"
}
lints() { job "$1" $'lint\tlib-bins'; job "$1" $'lint\tall-targets'; [ "$haswin" -eq 0 ] || job "$1" $'lint\twindows-tests'; }
winis() { # <repo> <0|red>: the windows line's result, or a skip
  if [ "$haswin" -eq 0 ]; then echo "SKIP: windows-gnu standard library not installed ($1 windows-tests)"; return 0; fi
  if [ "$2" = red ]; then redby "$1" lint-windows-tests; else [ "$(rcof "$1" lint-windows-tests)" = 0 ]; fi
}

# a clean workspace: every line green
new_repo clean
lints clean
check "clean: lib-bins green" '[ "$(rcof clean lint-lib-bins)" = 0 ]'
check "clean: all-targets green" '[ "$(rcof clean lint-all-targets)" = 0 ]'
check "clean: windows-tests green" 'winis clean 0'

# a test that runs a child through Command::output: the test-children rule, only in rust/clippy-tests
new_repo child
cat > "$repo/rust/sot-log/tests/t.rs" <<'EOF'
#[test]
fn t() {
    let _ = std::process::Command::new("true").output();
}
EOF
lints child
check "test child: lib-bins stays green (production config)" '[ "$(rcof child lint-lib-bins)" = 0 ]'
check "test child: all-targets goes red" 'redby child lint-all-targets'
check "test child: windows-tests goes red" 'winis child red'

# std::fs::copy in a test, then with the sanctioned allow
new_repo copy
cat > "$repo/rust/sot-log/tests/t.rs" <<'EOF'
#[test]
fn t() {
    let _ = std::fs::copy("a", "b");
}
EOF
lints copy
check "fs::copy: all-targets goes red" 'redby copy lint-all-targets'
new_repo copy-allowed
cat > "$repo/rust/sot-log/tests/t.rs" <<'EOF'
#[test]
fn t() {
    #[allow(clippy::disallowed_methods, reason = "a data copy")]
    let _ = std::fs::copy("a", "b");
}
EOF
lints copy-allowed
check "fs::copy with an allow: all-targets green" '[ "$(rcof copy-allowed lint-all-targets)" = 0 ]'

# a violation only Windows test code holds: the host's all-targets line cannot see it
new_repo winonly
cat > "$repo/rust/sot-backend/tests/t.rs" <<'EOF'
#[cfg(windows)]
#[test]
fn t() {
    let _ = std::process::Command::new("true").output();
}
EOF
lints winonly
check "windows-only test code: all-targets stays green" '[ "$(rcof winonly lint-all-targets)" = 0 ]'
check "windows-only test code: windows-tests goes red" 'winis winonly red'

# under-bash: a git checkout with the suites under-bash.sh lists, as stubs that pass
new_ub() {
  new_repo "$1"
  cp "$here/under-bash.sh" "$here/sigpipe-scan.awk" "$repo/scripts/tests/"
  for s in installer-state installer-wrapper installer-apply installer-shared-home test-topology-plan test-sigpipe-scan; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$repo/scripts/tests/$s.sh"
  done
  printf '#!/usr/bin/env bash\nexit 0\n' > "$repo/comm/tests/test-ok.sh"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$repo/agents/tests/test-ok.sh"
}
gitall() { (cd "$repo" && git init -q . && git add -A && git -c user.name=t -c user.email=t@t commit -q -m t); }
new_ub ubgreen
gitall
job ubgreen under-bash
check "under-bash: a script that runs under bash 3.2 is green" '[ "$(rcof ubgreen under-bash)" = 0 ]'
new_ub ubred
# `;;&` ends a case clause only in bash 4 and later: under 3.2 the script does not parse
printf '#!/usr/bin/env bash\ncase x in\n  x) echo a ;;&\n  *) echo b ;;\nesac\n' > "$repo/comm/four-only.sh"
gitall
job ubred under-bash
check "under-bash: a script that parses only under bash 4 goes red" '[ "$(rcof ubred under-bash)" = 1 ] && grep -q "FAIL parse comm/four-only.sh" "$work/L-ubred/under-bash.log"'

# the pipeline over a stub cargo: every clippy line is emitted with its arguments, and the bash 3.2 job runs or is skipped
pipe() { # <repo-name> [bash] -> runs the real --pipeline over a stub cargo that logs each clippy line to its folder
  local stub=$work/stub-$1
  mkdir -p "$stub"
  cat > "$stub/cargo" <<'STUB'
#!/bin/sh
[ "$1" = clippy ] && printf '%s conf=%s\n' "$*" "${CLIPPY_CONF_DIR:-}" >> "$(dirname "$0")/log"
exit 0
STUB
  chmod +x "$stub/cargo"
  new_ub "$1"
  : > "$work/L-$1/tests.tsv"
  gitall
  RCG_D=$repo RCG_L=$work/L-$1 RCG_BUILD_RC=0 RCG_CARGO_DIR=$stub RCG_JULIA_DIR=$stub RCG_BASH32=${2:-} \
    bash "$gate" --pipeline 1 > /dev/null 2>&1
}
pipe pipe "$bash32"
check "pipeline: lib-bins line runs without the test config" 'grep -qx "clippy --workspace --exclude vt100-ctt --lib --bins --locked -- -A clippy::all -D clippy::disallowed_methods conf=" "$work/stub-pipe/log"'
check "pipeline: all-targets line runs under rust/clippy-tests" 'grep -q "clippy --workspace --exclude vt100-ctt --all-targets --features .*daemon-lifetime-faults.* conf=$work/pipe/rust/clippy-tests$" "$work/stub-pipe/log"'
check "pipeline: windows line runs over sot-log and sot-backend under rust/clippy-tests" 'grep -q "clippy -p sot-log -p sot-backend --target x86_64-pc-windows-gnu --all-targets .* conf=$work/pipe/rust/clippy-tests$" "$work/stub-pipe/log"'
check "pipeline: every lint has a result of its own" '[ "$(rcof pipe lint-lib-bins)$(rcof pipe lint-all-targets)$(rcof pipe lint-windows-tests)" = 000 ]'
check "pipeline: the bash 3.2 job has a result" '[ -n "$(rcof pipe under-bash)" ] && [ "$(rcof pipe under-bash)" != unrun ]'
pipe none ""
check "pipeline: without a bash 3.2 the job is not emitted" '[ -z "$(rcof none under-bash)" ]'

echo "failed: $fail"
exit "$fail"
