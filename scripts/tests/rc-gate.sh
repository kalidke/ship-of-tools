#!/usr/bin/env bash
# rc-gate.sh <checkout> <logdir> [cap [line]]
#
# The rc gate as concurrent jobs under one cap (default 10). Linux only. It
# covers what the old full gate covered: the Rust workspace tests (the four
# slow binaries split per test), the doc tests, the windows-gnu and darwin
# cross checks, the three clippy lines of rust.yml (lib-bins, all-targets with the test-children config, and the
# all-targets line for x86_64-pc-windows-gnu over sot-log and sot-backend, which finds Windows-only test code from Linux),
# every Julia suite, and the shell suites; and, when the tree differs from `line` (a commit-ish; always when none is given)
# under scripts/, comm/ or agents/, scripts/tests/under-bash.sh under a bash 3.2.57 that scripts/tests/bash32.sh builds
# once and keeps under /scratch/<user>, as rust.yml's bash32-linux job runs it; and, as rust.yml's Linux job runs it, the daemon lifetime
# harness, built in CARGO_TARGET_DIR/daemon-lifetime, since its features (the capsule's phase barriers, the daemon's
# faults) would replace the `sotd` and `sot-capsule` the other jobs run.
#
# Runs as the main process of its own test container: scripts/tests/in-container.sh UNIT -- bash rc-gate.sh ...
# (exit 2 otherwise). Reads HOME, PATH (only for `command -v cargo`, `julia` and `node`),
# XDG_RUNTIME_DIR (the container's private folder) and CARGO_TARGET_DIR (required); forwards JULIA_DEPOT_PATH (to the
# Julia suites and the Rust tests, whose WGL page test adds WGLMakie from it), SSH_AUTH_SOCK and
# CARGO_PROFILE_DEV_DEBUG when set. The gate's julia is the Julia binary `julia` starts, asked once under the runner's
# HOME and named on summary.txt's third line: the Julia suites and the lifetime harness run it, and the Rust jobs get its
# folder on their PATH, as rust.yml's test jobs get setup-julia's, so a test daemon runs it (`support::julia_binary`).
# CARGO_TARGET_DIR is the gate's alone while it runs: its jobs run the binaries in it (the
# tests.tsv paths, `sotd`, `sot-capsule`), and summary.txt's first line names one head and tree; a build of another
# checkout into it would replace them mid-run. It belongs to one checkout for good: the first run names that checkout in
# <target>/rc-gate-checkout, and a run for any other checkout is refused (exit 2) before anything is built, since cargo
# keys a workspace crate by its path inside the workspace and trusts file times, so a second tree built into one target
# can run the first tree's binaries. Every job gets the container's private XDG_RUNTIME_DIR and no
# DBUS_SESSION_BUS_ADDRESS, and a HOME of its own, <logdir>/home, with XDG_CONFIG_HOME, XDG_STATE_HOME and CLAUDE_CONFIG_DIR
# under it and CARGO_HOME and RUSTUP_HOME named as the runner has them, so a test that reaches the runner's real home
# fails here as it would on a hosted runner, and cannot touch the fleet's live files, so no test scope reaches the user's systemd manager. Needs jq. Exit 2 on bad args;
# otherwise 0 (130 if interrupted) and the verdict is in <logdir>/summary.txt: it ends in ALLDONE, or ALLDONE FAILED
# when any job's rc is nonzero, the job runner failed, a job never ran, the run was interrupted, or a test left a
# process behind (any process left in the container). Every job's result file starts as `unrun`, so a job that never
# ran cannot pass; each job's command has a 1200 s end (the lifetime harness 3600 s; the builds none). The container's
# end kills what is left.
# ALLDONE means every rc line is 0; the rc lines say which job failed. A container stopped from outside SIGKILLs
# rc-gate, so summary.txt gets no ALLDONE line; a summary without one is a failed run.
# A shell suite's result and log are named by its path in the checkout with / as - (comm-tests-test-temp-files), so
# two suites with one basename keep their own verdicts.
# A selected Rust job's result is zero only when its exact body completed successfully. A missing,
# ignored-only or mismatched body produces a failed result even if the executable exited zero.
# Ignored names from split binaries are reported as skipped and are not ordinary jobs. The checked job
# results alone decide ALLDONE or ALLDONE FAILED, and the summary sums no test counts: a test that re-runs its binary
# prints the child's own libtest summary into the job's log, so a sum of summary lines miscounts.

SELF=$(readlink -f "$0")
source "$(dirname "$SELF")/lib-test-body.sh" || exit 2
TO=(timeout -k 10 1200)
# The clippy lines compile every crate again as clippy sees it, and under-bash.sh runs every hermetic shell suite once more.
LNT=(timeout -k 10 2400)
# The lifetime harness runs every case one at a time: 963 s inside a whole gate and 1035 s alone, on a shared test
# host. So it has an end of its own, inside the 75 min rust.yml gives its job.
LTO=(timeout -k 10 3600)
SPLIT=(capsule_workspaces comm_wake lane_bridge fe_client)
JULIA_PKGS=(core julia/kernel julia/repl julia/plugins/pdf-file julia/plugins/video-file julia/sotlog)

envs() {
  D=$RCG_D L=$RCG_L
  local hm=$L/home
  # one home for every job, the runner's real one named only for the toolchains
  HE=(HOME="$hm" XDG_CONFIG_HOME="$hm/.config" XDG_STATE_HOME="$hm/.local/state" CLAUDE_CONFIG_DIR="$hm/.claude")
  CE=(env -i "${HE[@]}" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" PATH="$RCG_JULIA_DIR:${RCG_NODE_DIR:+$RCG_NODE_DIR:}$RCG_CARGO_DIR:/usr/bin:/bin" TMPDIR=/tmp CARGO_TARGET_DIR="$CARGO_TARGET_DIR"
    XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR"
    ${CARGO_PROFILE_DEV_DEBUG:+CARGO_PROFILE_DEV_DEBUG="$CARGO_PROFILE_DEV_DEBUG"}
    ${JULIA_DEPOT_PATH:+JULIA_DEPOT_PATH="$JULIA_DEPOT_PATH"})
  # SOT_TEST_SOTD: the sotd the workspace build just made, so installer-apply.sh's real-sotd cases run in the gate.
  SE=(env -i "${HE[@]}" PATH="${RCG_NODE_DIR:+$RCG_NODE_DIR:}/usr/bin:/bin" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" SOT_TEST_SOTD="$CARGO_TARGET_DIR/debug/sotd" ${SSH_AUTH_SOCK:+SSH_AUTH_SOCK="$SSH_AUTH_SOCK"})
  JE=(env -i "${HE[@]}" PATH="$RCG_JULIA_DIR:/usr/bin:/bin" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" ${JULIA_DEPOT_PATH:+JULIA_DEPOT_PATH="$JULIA_DEPOT_PATH"})
}

st() { echo "$1 start $(date +%H:%M:%S) load $(cut -d' ' -f1-3 /proc/loadavg) cargo=$(pgrep -c cargo)" >> "$L/summary.txt"; }
rc() { echo "$1 rc=$2 ${3:-}" >> "$L/summary.txt"; }
# a step's verdict: its result file (swept at the end) and its summary line
fin() { echo "$2" > "$L/steps/$1.rc"; rc "$1" "$2"; }
# shell_key <path>: a shell suite's result and log name, its path in the checkout with / as -
shell_key() {
  local k=${1#"$D/"}
  k=${k%.sh}
  echo "${k//\//-}"
}

# logged OUT ERR CMD...: CMD with its standard output and error in a folder of its own in the container's private folder
# (XDG_RUNTIME_DIR, on this host's own disk), appended to OUT and ERR (ERR may be OUT: one stream, in order) when CMD
# ends; the status is CMD's. Every job runs its command so. The logs folder is wherever the caller reads it, often the
# network home, where each close of a written file waits for the file server to commit it (NFS flushes on every close),
# and a child closes the standard streams it inherited as it sets up its own, so a test's bound on that setup would time
# the file server. A job's log so holds only its header line until its command ends.
logged() {
  local out=$1 err=$2 o r
  shift 2
  o=$(mktemp -d "$XDG_RUNTIME_DIR/rc-gate-job-XXXXXX") || return 2
  if [ "$err" = "$out" ]; then
    "$@" > "$o/out" 2>&1
  else
    "$@" > "$o/out" 2> "$o/err"
  fi
  r=$?
  cat "$o/out" >> "$out"
  [ "$err" = "$out" ] || cat "$o/err" >> "$err"
  rm -rf "$o"
  return "$r"
}

job_julia() {
  local p s
  st julia
  logged "$L/julia-root.log" "$L/julia-root.log" "${TO[@]}" "${JE[@]}" julia --project="$D" -e 'using Pkg; Pkg.test()'
  fin julia-root $?
  for p in "${JULIA_PKGS[@]}"; do
    s=julia-${p//\//-}
    logged "$L/$s.log" "$L/$s.log" "${TO[@]}" "${JE[@]}" julia --project="$D/$p" -e 'using Pkg; Pkg.instantiate(); Pkg.test()'
    fin "$s" $?
  done
}

job_cargo_chain() {
  local M="$D/rust/Cargo.toml" r
  st rust-doc
  logged "$L/rust/doc.log" "$L/rust/doc.log" \
    "${TO[@]}" "${CE[@]}" cargo test --manifest-path "$M" --workspace --locked --doc --no-fail-fast
  r=$?
  echo "$r" > "$L/rust/doc.rc"
  rc rust-doc "$r"
  st win-check
  logged "$L/win-check.log" "$L/win-check.log" "${TO[@]}" "${CE[@]}" CC_x86_64_pc_windows_gnu=gcc AR_x86_64_pc_windows_gnu=ar \
    cargo check --manifest-path "$M" --target x86_64-pc-windows-gnu -p sot-log -p sot-backend -p sot-frontend --locked &&
  logged "$L/win-check.log" "$L/win-check.log" "${TO[@]}" "${CE[@]}" CC_x86_64_pc_windows_gnu=gcc AR_x86_64_pc_windows_gnu=ar \
    cargo check --manifest-path "$M" --target x86_64-pc-windows-gnu -p sot-backend -p sot-log --all-targets --locked
  fin win-check $?
  st darwin-check
  logged "$L/darwin-check.log" "$L/darwin-check.log" \
    "${TO[@]}" "${CE[@]}" cargo check --manifest-path "$M" --target aarch64-apple-darwin -p sot-log -p sot-backend --all-targets --locked
  fin darwin-check $?
}

# job_lint <line>: one of rust.yml's clippy lines, run from rust/ as the workflow runs it. The all-targets lines read
# rust/clippy-tests (CLIPPY_CONF_DIR), the test-children rule; the windows line finds cfg(windows) test code from Linux (the
# frontend's tree-sitter needs a cross C compiler, so it is left to the hosted leg).
job_lint() {
  local feats=sot-frontend/test-pane-timing,sot-frontend/test-window-close,sot-backend/daemon-lifetime-faults
  local deny=(-A clippy::all -D clippy::disallowed_methods)
  local conf=CLIPPY_CONF_DIR=$D/rust/clippy-tests cmd=()
  st "lint-$1"
  cd "$D/rust" || { fin "lint-$1" 2; return; }
  case $1 in
    lib-bins)
      cmd=("${LNT[@]}" "${CE[@]}" cargo clippy --workspace --exclude vt100-ctt --lib --bins --locked -- -A clippy::all -D clippy::disallowed_methods) ;;
    all-targets)
      cmd=("${LNT[@]}" "${CE[@]}" "$conf" cargo clippy --workspace --exclude vt100-ctt --all-targets --features "$feats" --locked -- "${deny[@]}") ;;
    windows-tests)
      cmd=("${LNT[@]}" "${CE[@]}" "$conf" CC_x86_64_pc_windows_gnu=gcc AR_x86_64_pc_windows_gnu=ar \
        cargo clippy -p sot-log -p sot-backend --target x86_64-pc-windows-gnu --all-targets \
        --features sot-backend/daemon-lifetime-faults --locked -- "${deny[@]}") ;;
    *) echo "rc-gate: no lint line $1" > "$L/lint-$1.log"; fin "lint-$1" 2; return ;;
  esac
  logged "$L/lint-$1.log" "$L/lint-$1.log" "${cmd[@]}"
  fin "lint-$1" $?
}

# job_under_bash: scripts/tests/under-bash.sh under the bash 3.2.57 the main flow names in RCG_BASH32
job_under_bash() {
  st under-bash
  cd "$D" || { fin under-bash 2; return; }
  logged "$L/under-bash.log" "$L/under-bash.log" "${LNT[@]}" "${SE[@]}" bash scripts/tests/under-bash.sh "$RCG_BASH32"
  fin under-bash $?
}

job_shell() {
  local k
  k=$(shell_key "$1")
  logged "$L/$k.log" "$L/$k.log" "${TO[@]}" "${SE[@]}" bash "$@"
  fin "$k" $?
}

# job_lifetime: the daemon lifetime harness as rust.yml's Linux job runs it: Pluto's environment prepared in the gate's
# depot with one notebook run (fixtures/warm_pluto.jl) and handed to the cases, then every case one at a time, the Julia
# ones on the gate's julia
job_lifetime() {
  local log=$L/rust/daemon_lifetime-faults.log pkg=$D/rust/backend warm=$L/lifetime-warm r
  echo "     Running ($RCG_LIFETIME) load $(cut -d' ' -f1 /proc/loadavg)" > "$log"
  mkdir -p "$warm" && cp "$pkg/tests/daemon_lifetime/fixtures/notebook.jl" "$warm/" &&
    (cd "$D" && logged "$L/lifetime-warm.log" "$L/lifetime-warm.log" "${TO[@]}" "${JE[@]}" julia --startup-file=no \
      --project=julia/pluto rust/backend/tests/daemon_lifetime/fixtures/warm_pluto.jl "$warm/notebook.jl")
  r=$?
  if [ "$r" -eq 0 ]; then
    (cd "$pkg" && logged "$log" "$L/rust/daemon_lifetime-faults.stderr" "${LTO[@]}" "${CE[@]}" CARGO_MANIFEST_DIR="$pkg" \
      SOT_JULIA_BIN="$RCG_JULIA_DIR/julia" SOT_L2_PLUTO_MANIFEST="$D/julia/pluto/Manifest.toml" \
      SOT_LIFETIME_KEEP_DIR="$L/lifetime-keep" "$RCG_LIFETIME" --include-ignored --test-threads=1)
    r=$?
  fi
  echo "$r" > "$L/rust/daemon_lifetime-faults.rc"
}

# job_test <key> <exe> <pkgdir> [test]
job_test() {
  local key=$1 exe=$2 pkg=$3 t=${4:-} log r
  log=$L/rust/$key.log
  echo "     Running ($exe) load $(cut -d' ' -f1 /proc/loadavg)" > "$log"
  (cd "$pkg" && logged "$log" "$L/rust/$key.stderr" "${TO[@]}" "${CE[@]}" CARGO_MANIFEST_DIR="$pkg" "$exe" \
    ${t:+--exact "$t" --format pretty --color never --show-output --test-threads=1})
  r=$?
  if [ -n "$t" ]; then
    if test_body_check "$t" "$r" "$log"; then r=0; else r=$?; fi
  fi
  echo "$r" > "$L/rust/$key.rc"
}

# one exactly-one-row lookup in tests.tsv; sets exe and pkg
row_for() {
  local r
  r=$(awk -F'\t' -v n="$1" '$1==n' "$L/tests.tsv")
  [ -n "$r" ] && [ "$(wc -l <<< "$r")" -eq 1 ] || return 1
  IFS=$'\t' read -r _ exe pkg <<< "$r"
}

# emit <key> <line> [resultfile]: once per key; the result starts as unrun. A result file emitted under two keys is
# refused (exit 2) before any job runs: the second job would overwrite the first one's verdict.
declare -A EMITTED RESULT
emit() {
  [ -n "${EMITTED[$1]:-}" ] && return
  EMITTED[$1]=1
  if [ -n "${3:-}" ]; then
    [ -n "${RESULT[$3]:-}" ] && { echo "rc-gate: result $3 emitted twice" >&2; exit 2; }
    RESULT[$3]=1
    echo unrun > "$3"
  fi
  printf '%s\n' "$2"
}
emit_bin() {
  local key; key=$(basename "$1")
  emit "bin:$1" "$(printf 'bin\t%s\t%s\t%s' "$key" "$1" "$2")" "$L/rust/$key.rc"
}
emit_one() {
  local key; key=$(basename "$1")__${3//:/_}
  emit "one:$1:$3" "$(printf 'one\t%s\t%s\t%s\t%s' "$key" "$1" "$2" "$3")" "$L/rust/$key.rc"
}
emit_shell() {
  local name; name=$(basename "$1" .sh)
  emit "shell:$1" "$(printf 'shell\t%s' "$name"; printf '\t%s' "$@")" "$L/steps/$(shell_key "$1").rc"
}

producer() {
  local k tgt t n exe pkg f s found out lrc ignored irc prc
  local -A LISTED SPLIT_EXE SPLIT_PKG
  local SHELL_ALL=()
  local SLOW_FIRST=(
    lane_bridge/outage::a_blackhole_is_unreachable_and_retried
    lane_bridge/outage::a_daemon_outage_past_the_window_keeps_retrying
    lane_bridge/outage::a_terminal_row_is_terminal_after_the_window
    fe_client/supervisor_word::unresponsive_supervisor_expires_the_health_window
    test-spawn-capsule-workspace
    capsule_workspaces/spawn::capsule_supervisor_spawn_survives_fence_contention_without_marking_terminal
    supervisor
    test-status-floor
  )
  # shell suites: the SLOW_FIRST ones are emitted early, the rest at step 5
  for f in "$D"/comm/tests/test-*.sh "$D"/agents/tests/test-*.sh; do
    SHELL_ALL+=("$f")
  done
  SHELL_ALL+=("$D/scripts/tests/installer-state.sh" "$D/scripts/tests/installer-wrapper.sh" "$D/scripts/tests/installer-apply.sh" "$D/scripts/tests/installer-shared-home.sh" "$D/scripts/tests/test-topology-plan.sh" "$D/scripts/tests/test-test-body.sh" "$D/scripts/tests/test-bash32.sh" "$D/scripts/tests/test-release-cut.sh")

  [ -n "${RCG_LIFETIME:-}" ] && emit lifetime lifetime
  for s in julia-root "${JULIA_PKGS[@]/#/julia-}"; do echo unrun > "$L/steps/${s//\//-}.rc"; done
  emit julia julia
  echo unrun > "$L/rust/doc.rc"
  echo unrun > "$L/steps/win-check.rc"
  echo unrun > "$L/steps/darwin-check.rc"
  emit cargo-chain cargo-chain
  for s in lib-bins all-targets windows-tests; do emit "lint:$s" "$(printf 'lint\t%s' "$s")" "$L/steps/lint-$s.rc"; done
  [ -n "${RCG_BASH32:-}" ] && emit under-bash under-bash "$L/steps/under-bash.rc"
  [ "$BUILD_RC" -eq 0 ] && st rust-workspace

  # list the four slow binaries; a split binary runs only per test
  if [ "$BUILD_RC" -eq 0 ]; then
    for n in "${SPLIT[@]}"; do
      if ! row_for "$n"; then
        echo "split-missing $n" >> "$L/summary.txt"; fin "split-$n" 101; continue
      fi
      EMITTED["bin:$exe"]=1
      (cd "$pkg" && "${TO[@]}" "${CE[@]}" CARGO_MANIFEST_DIR="$pkg" "$exe" --list --format terse) \
        > "$L/$n.all.list" 2> "$L/$n.list.log"
      lrc=$?
      (cd "$pkg" && "${TO[@]}" "${CE[@]}" CARGO_MANIFEST_DIR="$pkg" "$exe" --list --ignored --format terse) \
        > "$L/$n.ignored.list" 2>> "$L/$n.list.log"
      irc=$?
      if [ "$lrc" -ne 0 ] || [ "$irc" -ne 0 ]; then
        echo "split-missing $n: list exits $lrc/$irc" >> "$L/summary.txt"; fin "split-$n" 101; continue
      fi
      out=$(python3 - "$L/$n.all.list" "$L/$n.ignored.list" <<'PYLIST'
import sys

def names(path):
    rows = [line[:-6] for line in open(path).read().splitlines() if line.endswith(': test')]
    if len(rows) != len(set(rows)):
        raise ValueError('duplicate listed test')
    return rows

try:
    complete, ignored = (names(path) for path in sys.argv[1:])
    if not complete or not set(ignored).issubset(complete):
        raise ValueError('empty or inconsistent complete/ignored lists')
    print('\n'.join(name for name in complete if name not in set(ignored)))
except (OSError, ValueError) as error:
    print('split listing: ' + str(error), file=sys.stderr)
    sys.exit(101)
PYLIST
      )
      prc=$?
      if [ "$prc" -ne 0 ]; then
        echo "split-missing $n: invalid listing" >> "$L/summary.txt"; fin "split-$n" 101; continue
      fi
      while IFS= read -r ignored; do
        [[ $ignored = *': test' ]] || continue
        echo "split-skipped $n/${ignored%: test}" >> "$L/summary.txt"
      done < "$L/$n.ignored.list"
      LISTED[$n]=$out
      SPLIT_EXE[$n]=$exe SPLIT_PKG[$n]=$pkg
    done
  fi

  for k in "${SLOW_FIRST[@]}"; do
    case $k in
      test-*)
        found=
        for f in "${SHELL_ALL[@]}"; do
          if [ "$(basename "$f" .sh)" = "$k" ]; then found=$f; fi
        done
        if [ -z "$found" ]; then echo "slow-first-missing $k" >> "$L/summary.txt"; fin "missing-${k//[:\/]/_}" 101; continue; fi
        emit_shell "$found"
        ;;
      */*)
        [ "$BUILD_RC" -eq 0 ] || continue
        tgt=${k%%/*}; t=${k#*/}
        if [ -n "${LISTED[$tgt]:-}" ] && grep -qxF -- "$t" <<< "${LISTED[$tgt]}"; then
          emit_one "${SPLIT_EXE[$tgt]}" "${SPLIT_PKG[$tgt]}" "$t"
        else
          echo "slow-first-missing $k" >> "$L/summary.txt"
          fin "missing-${k//[:\/]/_}" 101
        fi
        ;;
      *)
        [ "$BUILD_RC" -eq 0 ] || continue
        if row_for "$k"; then emit_bin "$exe" "$pkg"; else echo "slow-first-missing $k" >> "$L/summary.txt"; fin "missing-${k//[:\/]/_}" 101; fi
        ;;
    esac
  done
  st shell-suites
  for f in "${SHELL_ALL[@]}"; do
    if [ "$(basename "$f")" = test-test-body.sh ]; then
      if [ "$BUILD_RC" -eq 0 ] && row_for test_body_fixture; then
        emit_shell "$f" --all "$exe"
      else
        fin "$(shell_key "$f")" 101
      fi
    else
      emit_shell "$f"
    fi
  done
  [ "$BUILD_RC" -eq 0 ] || return 0
  for n in "${SPLIT[@]}"; do
    [ -n "${LISTED[$n]:-}" ] || continue
    while IFS= read -r t; do
      emit_one "${SPLIT_EXE[$n]}" "${SPLIT_PKG[$n]}" "$t"
    done <<< "${LISTED[$n]}"
  done
  while IFS=$'\t' read -r _ exe pkg; do
    emit_bin "$exe" "$pkg"
  done < "$L/tests.tsv"
}

if [ "${1:-}" = --job ]; then
  envs
  IFS=$'\t' read -r kind a b c d <<< "$2"
  case $kind in
    julia) job_julia ;;
    lifetime) job_lifetime ;;
    cargo-chain) job_cargo_chain ;;
    lint) job_lint "$a" ;;
    under-bash) job_under_bash ;;
    shell) job_shell "$b" ${c:+"$c" "$d"} ;;
    bin) job_test "$a" "$b" "$c" ;;
    one) job_test "$a" "$b" "$c" "$d" ;;
  esac
  exit 0
fi

if [ "${1:-}" = --pipeline ]; then
  envs
  BUILD_RC=$RCG_BUILD_RC
  set -o pipefail
  # the listing is taken whole before any job starts, so a refused listing (emit's exit 2) starts no job
  listing=$(producer) || exit $?
  printf '%s\n' "$listing" | xargs -d '\n' -n 1 -P "$2" bash "$SELF" --job
  exit $?
fi

# ---- main flow ----
CAP=${3:-10}
if [ $# -lt 2 ] || [ -z "${CARGO_TARGET_DIR:-}" ] || ! [[ $CAP =~ ^[1-9][0-9]*$ ]]; then
  echo "usage: CARGO_TARGET_DIR=... rc-gate.sh <checkout> <logdir> [cap >= 1 [line]]" >&2
  exit 2
fi
# One target per checkout, for good: the first run names its checkout in the target; a run for another is refused.
D=$(cd "$1" && pwd) || exit 2
CLAIM=$CARGO_TARGET_DIR/rc-gate-checkout
if [ -e "$CLAIM" ] && [ "$(cat "$CLAIM")" != "$D" ]; then
  echo "rc-gate: $CARGO_TARGET_DIR holds the build of $(cat "$CLAIM"); give $D a target of its own" >&2
  exit 2
fi
mkdir -p "$CARGO_TARGET_DIR" && printf '%s\n' "$D" > "$CLAIM" || exit 2
R=/sys/fs/cgroup; [ -e "$R/cgroup.controllers" ] || R=/sys/fs/cgroup/unified
CG=$R$(sed -n 's/^0:://p' /proc/self/cgroup)
[ -r "$CG/cgroup.procs" ] || { echo "rc-gate: needs a cgroup v2 control group" >&2; exit 2; }
mapfile -t P < "$CG/cgroup.procs"
[ "${P[*]}" = "$$" ] || { echo "rc-gate: run it as the main process of its own container: scripts/tests/in-container.sh UNIT -- bash $SELF ..." >&2; exit 2; }
[ -z "${DBUS_SESSION_BUS_ADDRESS+x}" ] && [ -n "${XDG_RUNTIME_DIR-}" ] && [ ! -e "$XDG_RUNTIME_DIR/bus" ] &&
  [ ! -e "$XDG_RUNTIME_DIR/systemd" ] && [[ $CG == *.service ]] ||
  { echo "rc-gate: run it in a test container (in-container.sh without --user-manager): its jobs must reach no user manager and the container must end with it" >&2; exit 2; }
if [ -d "$2" ] && [ -n "$(ls -A "$2")" ]; then
  echo "rc-gate: logdir $2 exists and is not empty" >&2
  exit 2
fi
mkdir -p "$2" || exit 2
L=$(cd "$2" && pwd)
mkdir -p "$L/rust" "$L/steps" "$L/home" || exit 2
CARGO_BIN=$(command -v cargo) || { echo "rc-gate: cargo not found" >&2; exit 2; }
JULIA_BIN=$(command -v julia) || { echo "rc-gate: julia not found" >&2; exit 2; }
# The binary it starts, asked under the runner's HOME, where a juliaup launcher finds its installation: a launcher on the
# jobs' PATH would install a Julia from the network under each HOME a test makes.
JULIA_BIN=$(timeout 60 "$JULIA_BIN" --startup-file=no -e 'print(Base.julia_cmd()[1])') && [ -x "$JULIA_BIN" ] ||
  { echo "rc-gate: $(command -v julia) starts no Julia binary" >&2; exit 2; }
NODE_BIN=$(command -v node) || { echo "rc-gate: node not found" >&2; exit 2; }
[ -d "$D/rust/backend/sidecars/mathjax/node_modules" ] || { echo "rc-gate: run npm ci in rust/backend/sidecars/mathjax" >&2; exit 2; }
command -v jq > /dev/null || { echo "rc-gate: jq not found" >&2; exit 2; }
PATH=/usr/bin:/bin command -v Xvfb > /dev/null || { echo "rc-gate: Xvfb not found in /usr/bin or /bin (the sotd sidecar tests start it)" >&2; exit 2; }
# The jobs get node and julia, each alone from a folder of its own (on the servers node's folder also holds real agent
# CLIs, and a julia in /usr/bin would put /usr/bin's node ahead of the node chosen here). Those two folders lead the Rust
# jobs' PATH, ahead of cargo's folder: it is wherever cargo is installed, and whatever else it holds is not the gate's.
mkdir -p "$L/bin" "$L/julia-bin" && ln -s "$NODE_BIN" "$L/bin/node" && ln -s "$JULIA_BIN" "$L/julia-bin/julia" || exit 2
# under-bash.sh runs when scripts/, comm/ or agents/ differ from the line (or no line is given), under the cached bash 3.2.57
RCG_BASH32=
LINE=${4:-}
if [ -n "$LINE" ]; then
  (cd "$D" && git rev-parse --verify -q "$LINE^{commit}" > /dev/null) || { echo "rc-gate: $LINE is no commit in $D" >&2; exit 2; }
fi
if [ -n "$LINE" ] && (cd "$D" && git diff --quiet "$LINE" HEAD -- scripts comm agents); then
  SKIPPED="under-bash skipped: scripts, comm and agents equal $LINE"
else
  RCG_BASH32=$(bash "$D/scripts/tests/bash32.sh") || { echo "rc-gate: no bash 3.2.57" >&2; exit 2; }
fi
export RCG_BASH32
export RCG_D=$D RCG_L=$L RCG_CARGO_DIR=$(dirname "$CARGO_BIN") RCG_JULIA_DIR=$L/julia-bin RCG_NODE_DIR=$L/bin
envs
INTR=0
trap 'INTR=1' INT TERM HUP

echo "head $(git -C "$D" rev-parse HEAD) tree $(git -C "$D" rev-parse 'HEAD^{tree}')" > "$L/summary.txt"
echo "cap $CAP" >> "$L/summary.txt"
echo "julia $JULIA_BIN" >> "$L/summary.txt"
[ -z "${SKIPPED:-}" ] || echo "$SKIPPED" >> "$L/summary.txt"

# every process in this container but rc-gate itself, read again after 5 s so one that is exiting is not counted
census() {
  local p f=$L/.census
  find "$CG" -name cgroup.procs -exec cat {} + > "$f"; mapfile -t P < "$f"
  for p in "${P[@]}"; do [ -d "/proc/$p" ] && [ "$p" != "$$" ] && { sleep 5; break; }; done
  find "$CG" -name cgroup.procs -exec cat {} + > "$f"; mapfile -t P < "$f"; rm -f "$f"
  for p in "${P[@]}"; do [ -d "/proc/$p" ] && [ "$p" != "$$" ] && echo "$p $(tr '\0' ' ' < "/proc/$p/cmdline" 2> /dev/null)"; done
}

T0=$(date +%s)
M=$D/rust/Cargo.toml
BUILD_RC=0
st build-capsule
"${CE[@]}" cargo build --manifest-path "$M" -p sot-log --bin sot-capsule --locked > "$L/build-capsule.log" 2>&1
r=$?; rc build-capsule $r; [ $r -ne 0 ] && BUILD_RC=$r
: > "$L/tests.tsv"
if [ "$INTR" -eq 0 ]; then
  st rust-build
  "${CE[@]}" cargo test --manifest-path "$M" --workspace --locked --no-run --message-format=json \
    > "$L/rust-build.json" 2> "$L/rust-build.log"
  r=$?; rc rust-build $r; [ $r -ne 0 ] && BUILD_RC=$r
fi
if [ "$BUILD_RC" -eq 0 ] && [ "$INTR" -eq 0 ]; then
  if ! jq -r 'select(.reason=="compiler-artifact" and .profile.test==true and .executable!=null)
    | [.target.name, .executable, (.manifest_path|rtrimstr("/Cargo.toml"))] | @tsv' \
    "$L/rust-build.json" > "$L/tests.tsv" || ! [ -s "$L/tests.tsv" ]; then
    BUILD_RC=1
    rc rust-build 1 tests.tsv
  fi
fi
export RCG_BUILD_RC=$BUILD_RC
# The daemon lifetime harness's own build; its result stays unrun, a failure, unless its job runs.
echo unrun > "$L/rust/daemon_lifetime-faults.rc"
RCG_LIFETIME=
if [ "$INTR" -eq 0 ]; then
  st build-lifetime
  "${CE[@]}" CARGO_TARGET_DIR="$CARGO_TARGET_DIR/daemon-lifetime" cargo build --manifest-path "$M" -p sot-log \
    --features native-barrier --bin sot-capsule --locked > "$L/build-lifetime.log" 2>&1 &&
    "${CE[@]}" CARGO_TARGET_DIR="$CARGO_TARGET_DIR/daemon-lifetime" cargo test --manifest-path "$M" -p sot-backend \
      --features daemon-lifetime-faults --test daemon_lifetime --locked --no-run --message-format=json \
      > "$L/build-lifetime.json" 2>> "$L/build-lifetime.log"
  r=$?; rc build-lifetime $r
  [ $r -eq 0 ] && RCG_LIFETIME=$(jq -r 'select(.reason=="compiler-artifact" and .profile.test==true and .executable!=null)
    | .executable' "$L/build-lifetime.json")
fi
export RCG_LIFETIME

XS=130
if [ "$INTR" -eq 0 ]; then
  setsid bash "$SELF" --pipeline "$CAP" &
  PG=$!
  wait "$PG"
  XS=$?
  [ "$XS" -eq 0 ] && [ "$INTR" -eq 0 ] && census > "$L/leftovers.txt"
fi
# anything the tests left in the container is reported; the container's end kills it
LEFT=$(cat "$L/leftovers.txt" 2> /dev/null)
# the verdict is being written: from here no signal changes it
trap '' INT TERM HUP

# one sweep over every result file: a failure or a job that never ran is a failure
RC=0
NOTES=
UNRUN=0 STEPFAIL=0
for f in "$L"/rust/*.rc "$L"/steps/*.rc; do
  [ -e "$f" ] || continue
  v=$(cat "$f")
  case $f in
    "$L"/rust/*) [ "$v" = unrun ] && UNRUN=1
       [ "$v" = 0 ] || { RC=101; NOTES+="rust-failed $(basename "$f" .rc) $v"$'\n'; } ;;
    *) [ "$v" = unrun ] && { UNRUN=1; NOTES+="$(basename "$f" .rc) rc=unrun"$'\n'; }
       [ "$v" = 0 ] || STEPFAIL=1 ;;
  esac
done
[ "$BUILD_RC" -eq 0 ] || RC=$BUILD_RC
rc rust-workspace "$RC"
[ -n "$NOTES" ] && printf '%s' "$NOTES" >> "$L/summary.txt"
[ "$XS" -eq 0 ] || rc pipeline "$XS"
[ -z "$LEFT" ] || sed 's/^/leftover /' <<< "$LEFT" >> "$L/summary.txt"
# peak load from the samples every job start took
PEAK=$({ awk '$2 == "start" {print $5}' "$L/summary.txt"
  awk 'FNR == 1 && $1 == "Running" {print $NF}' "$L"/rust/*.log 2> /dev/null; } | sort -n | tail -n 1)
echo "end $(date +%H:%M:%S) load $(cut -d' ' -f1-3 /proc/loadavg) cargo=$(pgrep -c cargo) peak-load ${PEAK:-0} wall $(($(date +%s) - T0))s" >> "$L/summary.txt"
if [ "$RC" -eq 0 ] && [ "$STEPFAIL" -eq 0 ] && [ "$XS" -eq 0 ] && [ "$INTR" -eq 0 ] && [ "$UNRUN" -eq 0 ] &&
  [ -z "$LEFT" ]; then
  echo ALLDONE >> "$L/summary.txt"
else
  echo "ALLDONE FAILED" >> "$L/summary.txt"
fi
[ "$INTR" -eq 1 ] && exit 130
exit 0
