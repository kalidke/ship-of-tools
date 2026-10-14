#!/usr/bin/env bash
# rc-gate.sh <checkout> <logdir> [cap]
#
# The rc gate as concurrent jobs under one cap (default 10). Linux only. It
# covers what the old full gate covered: the Rust workspace tests (the four
# slow binaries split per test), the doc tests, the windows-gnu and darwin
# cross checks, every Julia suite, and the shell suites; and, as rust.yml's Linux job runs it, the daemon lifetime
# harness, built in CARGO_TARGET_DIR/daemon-lifetime, since its features (the capsule's phase barriers, the daemon's
# faults) would replace the `sotd` and `sot-capsule` the other jobs run.
#
# Runs as the main process of its own test container: scripts/tests/in-container.sh UNIT -- bash rc-gate.sh ...
# (exit 2 otherwise). Reads HOME, PATH (only for `command -v cargo`, `julia` and `node`),
# XDG_RUNTIME_DIR (the container's private folder) and CARGO_TARGET_DIR (required); forwards JULIA_DEPOT_PATH (to the
# Julia suites and the Rust tests, whose WGL page test adds WGLMakie from it), SSH_AUTH_SOCK and
# CARGO_PROFILE_DEV_DEBUG when set. CARGO_TARGET_DIR is the gate's alone while it runs: its jobs run the binaries in it (the
# tests.tsv paths, `sotd`, `sot-capsule`), and summary.txt's first line names one head and tree; a build of another
# checkout into it would replace them mid-run. It belongs to one checkout for good: the first run names that checkout in
# <target>/rc-gate-checkout, and a run for any other checkout is refused (exit 2) before anything is built, since cargo
# keys a workspace crate by its path inside the workspace and trusts file times, so a second tree built into one target
# can run the first tree's binaries. Every job gets the container's private XDG_RUNTIME_DIR and no
# DBUS_SESSION_BUS_ADDRESS, so no test scope reaches the user's systemd manager. Needs jq. Exit 2 on bad args;
# otherwise 0 (130 if interrupted) and the verdict is in <logdir>/summary.txt: it ends in ALLDONE, or ALLDONE FAILED
# when any job's rc is nonzero, the job runner failed, a job never ran, the run was interrupted, or a test left a
# process behind (any process left in the container). Every job's result file starts as `unrun`, so a job that never
# ran cannot pass; each job's command has a 1200 s end (the lifetime harness 3600 s; the builds none). The container's
# end kills what is left.
# ALLDONE means every rc line is 0; the rc lines say which job failed. A container stopped from outside SIGKILLs
# rc-gate, so summary.txt gets no ALLDONE line; a summary without one is a failed run.
# A selected Rust job's result is zero only when its exact body completed successfully. A missing,
# ignored-only or mismatched body produces a failed result even if the executable exited zero.
# Ignored names from split binaries are reported as skipped and are not ordinary jobs. The checked job
# results alone decide ALLDONE or ALLDONE FAILED, and the summary sums no test counts: a test that re-runs its binary
# prints the child's own libtest summary into the job's log, so a sum of summary lines miscounts.

SELF=$(readlink -f "$0")
source "$(dirname "$SELF")/lib-test-body.sh" || exit 2
TO=(timeout -k 10 1200)
# The lifetime harness runs every case one at a time: 963 s inside a whole gate and 1035 s alone, on a shared test
# host. So it has an end of its own, inside the 75 min rust.yml gives its job.
LTO=(timeout -k 10 3600)
SPLIT=(capsule_workspaces comm_wake lane_bridge fe_client)
JULIA_PKGS=(core julia/kernel julia/repl julia/plugins/pdf-file julia/plugins/video-file julia/sotlog)

envs() {
  D=$RCG_D L=$RCG_L
  CE=(env -i HOME="$HOME" PATH="$RCG_CARGO_DIR:${RCG_NODE_DIR:+$RCG_NODE_DIR:}/usr/bin:/bin" TMPDIR=/tmp CARGO_TARGET_DIR="$CARGO_TARGET_DIR"
    XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR"
    ${CARGO_PROFILE_DEV_DEBUG:+CARGO_PROFILE_DEV_DEBUG="$CARGO_PROFILE_DEV_DEBUG"}
    ${JULIA_DEPOT_PATH:+JULIA_DEPOT_PATH="$JULIA_DEPOT_PATH"})
  SE=(env -i HOME="$HOME" PATH="${RCG_NODE_DIR:+$RCG_NODE_DIR:}/usr/bin:/bin" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" ${SSH_AUTH_SOCK:+SSH_AUTH_SOCK="$SSH_AUTH_SOCK"})
  JE=(env -i HOME="$HOME" PATH="$RCG_JULIA_DIR:/usr/bin:/bin" XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" ${JULIA_DEPOT_PATH:+JULIA_DEPOT_PATH="$JULIA_DEPOT_PATH"})
}

st() { echo "$1 start $(date +%H:%M:%S) load $(cut -d' ' -f1-3 /proc/loadavg) cargo=$(pgrep -c cargo)" >> "$L/summary.txt"; }
rc() { echo "$1 rc=$2 ${3:-}" >> "$L/summary.txt"; }
# a step's verdict: its result file (swept at the end) and its summary line
fin() { echo "$2" > "$L/steps/$1.rc"; rc "$1" "$2"; }

job_julia() {
  local p s
  st julia
  "${TO[@]}" "${JE[@]}" julia --project="$D" -e 'using Pkg; Pkg.test()' > "$L/julia-root.log" 2>&1
  fin julia-root $?
  for p in "${JULIA_PKGS[@]}"; do
    s=julia-${p//\//-}
    "${TO[@]}" "${JE[@]}" julia --project="$D/$p" -e 'using Pkg; Pkg.instantiate(); Pkg.test()' > "$L/$s.log" 2>&1
    fin "$s" $?
  done
}

job_cargo_chain() {
  local M="$D/rust/Cargo.toml" r
  st rust-doc
  "${TO[@]}" "${CE[@]}" cargo test --manifest-path "$M" --workspace --locked --doc --no-fail-fast > "$L/rust/doc.log" 2>&1
  r=$?
  echo "$r" > "$L/rust/doc.rc"
  rc rust-doc "$r"
  st win-check
  "${TO[@]}" "${CE[@]}" CC_x86_64_pc_windows_gnu=gcc AR_x86_64_pc_windows_gnu=ar \
    cargo check --manifest-path "$M" --target x86_64-pc-windows-gnu -p sot-log -p sot-backend -p sot-frontend --locked \
    > "$L/win-check.log" 2>&1 &&
  "${TO[@]}" "${CE[@]}" CC_x86_64_pc_windows_gnu=gcc AR_x86_64_pc_windows_gnu=ar \
    cargo check --manifest-path "$M" --target x86_64-pc-windows-gnu -p sot-backend -p sot-log --all-targets --locked \
    >> "$L/win-check.log" 2>&1
  fin win-check $?
  st darwin-check
  "${TO[@]}" "${CE[@]}" cargo check --manifest-path "$M" --target aarch64-apple-darwin -p sot-log -p sot-backend --all-targets --locked \
    > "$L/darwin-check.log" 2>&1
  fin darwin-check $?
}

job_shell() {
  local b
  b=$(basename "$1" .sh)
  "${TO[@]}" "${SE[@]}" bash "$@" > "$L/$b.log" 2>&1
  fin "$b" $?
}

# job_lifetime: the daemon lifetime harness as rust.yml's Linux job runs it: Pluto's environment prepared in the gate's
# depot with one notebook run (fixtures/warm_pluto.jl) and handed to the cases, then every case one at a time, the Julia
# ones on the gate's julia
job_lifetime() {
  local log=$L/rust/daemon_lifetime-faults.log pkg=$D/rust/backend warm=$L/lifetime-warm r
  echo "     Running ($RCG_LIFETIME) load $(cut -d' ' -f1 /proc/loadavg)" > "$log"
  mkdir -p "$warm" && cp "$pkg/tests/daemon_lifetime/fixtures/notebook.jl" "$warm/" &&
    (cd "$D" && "${TO[@]}" "${JE[@]}" julia --startup-file=no --project=julia/pluto \
      rust/backend/tests/daemon_lifetime/fixtures/warm_pluto.jl "$warm/notebook.jl") > "$L/lifetime-warm.log" 2>&1
  r=$?
  if [ "$r" -eq 0 ]; then
    (cd "$pkg" && "${LTO[@]}" "${CE[@]}" CARGO_MANIFEST_DIR="$pkg" SOT_JULIA_BIN="$RCG_JULIA_DIR/julia" \
      SOT_L2_PLUTO_MANIFEST="$D/julia/pluto/Manifest.toml" "$RCG_LIFETIME" --include-ignored --test-threads=1) \
      >> "$log" 2>> "$L/rust/daemon_lifetime-faults.stderr"
    r=$?
  fi
  echo "$r" > "$L/rust/daemon_lifetime-faults.rc"
}

# job_test <key> <exe> <pkgdir> [test]
job_test() {
  local key=$1 exe=$2 pkg=$3 t=${4:-} log r
  log=$L/rust/$key.log
  echo "     Running ($exe) load $(cut -d' ' -f1 /proc/loadavg)" > "$log"
  (cd "$pkg" && "${TO[@]}" "${CE[@]}" CARGO_MANIFEST_DIR="$pkg" "$exe" \
    ${t:+--exact "$t" --format pretty --color never --show-output --test-threads=1}) >> "$log" 2>> "$L/rust/$key.stderr"
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

# emit <key> <line> [resultfile]: once per key; the result starts as unrun
declare -A EMITTED
emit() {
  [ -n "${EMITTED[$1]:-}" ] && return
  EMITTED[$1]=1
  [ -n "${3:-}" ] && echo unrun > "$3"
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
  emit "shell:$1" "$(printf 'shell\t%s' "$name"; printf '\t%s' "$@")" "$L/steps/$name.rc"
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
    test-hub-files
    supervisor
    test-status-floor
    test-relay-file-first
    test-registry-lock
  )
  # shell suites: the SLOW_FIRST ones are emitted early, the rest at step 5
  for f in "$D"/comm/tests/test-*.sh "$D"/agents/tests/test-*.sh; do
    case $(basename "$f" .sh) in
      test-comm-e2e-readers|test-inbox-lock-onehost|test-inbox-lock-twohost|test-registry-twohost|test-registry-lock-twohost) ;;
      *) SHELL_ALL+=("$f") ;;
    esac
  done
  SHELL_ALL+=("$D/scripts/tests/installer-state.sh" "$D/scripts/tests/installer-apply.sh" "$D/scripts/tests/installer-shared-home.sh" "$D/scripts/tests/test-topology-plan.sh" "$D/scripts/tests/test-test-body.sh")

  [ -n "${RCG_LIFETIME:-}" ] && emit lifetime lifetime
  for s in julia-root "${JULIA_PKGS[@]/#/julia-}"; do echo unrun > "$L/steps/${s//\//-}.rc"; done
  emit julia julia
  echo unrun > "$L/rust/doc.rc"
  echo unrun > "$L/steps/win-check.rc"
  echo unrun > "$L/steps/darwin-check.rc"
  emit cargo-chain cargo-chain
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
        fin test-test-body 101
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
  producer | xargs -d '\n' -n 1 -P "$2" bash "$SELF" --job
  exit $?
fi

# ---- main flow ----
CAP=${3:-10}
if [ $# -lt 2 ] || [ -z "${CARGO_TARGET_DIR:-}" ] || ! [[ $CAP =~ ^[1-9][0-9]*$ ]]; then
  echo "usage: CARGO_TARGET_DIR=... rc-gate.sh <checkout> <logdir> [cap >= 1]" >&2
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
mkdir -p "$L/rust" "$L/steps"
CARGO_BIN=$(command -v cargo) || { echo "rc-gate: cargo not found" >&2; exit 2; }
JULIA_BIN=$(command -v julia) || { echo "rc-gate: julia not found" >&2; exit 2; }
NODE_BIN=$(command -v node) || { echo "rc-gate: node not found" >&2; exit 2; }
[ -d "$D/rust/backend/sidecars/mathjax/node_modules" ] || { echo "rc-gate: run npm ci in rust/backend/sidecars/mathjax" >&2; exit 2; }
command -v jq > /dev/null || { echo "rc-gate: jq not found" >&2; exit 2; }
PATH=/usr/bin:/bin command -v Xvfb > /dev/null || { echo "rc-gate: Xvfb not found in /usr/bin or /bin (the sotd sidecar tests start it)" >&2; exit 2; }
# The jobs get node and nothing else from its folder (on the servers it also holds real agent CLIs).
mkdir -p "$L/bin" && ln -s "$NODE_BIN" "$L/bin/node" || exit 2
export RCG_D=$D RCG_L=$L RCG_CARGO_DIR=$(dirname "$CARGO_BIN") RCG_JULIA_DIR=$(dirname "$JULIA_BIN") RCG_NODE_DIR=$L/bin
envs
INTR=0
trap 'INTR=1' INT TERM HUP

echo "head $(git -C "$D" rev-parse HEAD) tree $(git -C "$D" rev-parse 'HEAD^{tree}')" > "$L/summary.txt"
echo "cap $CAP" >> "$L/summary.txt"

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
