#!/usr/bin/env bash
# rc-gate.sh <checkout> <logdir> [cap]
#
# The rc gate as concurrent jobs under one cap (default 10). Linux only. It
# covers what the old full gate covered: the Rust workspace tests (the four
# slow binaries split per test), the doc tests, the windows-gnu and darwin
# cross checks, every Julia suite, and the shell suites.
#
# Reads HOME, PATH (only for `command -v cargo` and `command -v julia`) and
# CARGO_TARGET_DIR (required); forwards JULIA_DEPOT_PATH, SSH_AUTH_SOCK and
# CARGO_PROFILE_DEV_DEBUG when set. Rust jobs never get XDG_RUNTIME_DIR or
# DBUS_SESSION_BUS_ADDRESS: they would put test scopes into the user's systemd
# manager. Needs jq. Exit 2 on bad args; otherwise 0 (130 if interrupted) and the
# verdict is in <logdir>/summary.txt (ends in ALLDONE, or ALLDONE FAILED when
# the job runner itself failed). Every job's result file starts as `unrun`, so
# a job that never ran cannot pass; each command has a 1200 s end.

SELF=$(readlink -f "$0")
TO=(timeout -k 10 1200)
JULIA_PKGS=(core julia/kernel julia/repl julia/plugins/pdf-file julia/plugins/video-file julia/sotlog)

envs() {
  D=$RCG_D L=$RCG_L
  CE=(env -i HOME="$HOME" PATH="$RCG_CARGO_DIR:/usr/bin:/bin" TMPDIR=/tmp CARGO_TARGET_DIR="$CARGO_TARGET_DIR"
    ${CARGO_PROFILE_DEV_DEBUG:+CARGO_PROFILE_DEV_DEBUG="$CARGO_PROFILE_DEV_DEBUG"})
  SE=(env -i HOME="$HOME" PATH=/usr/bin:/bin ${SSH_AUTH_SOCK:+SSH_AUTH_SOCK="$SSH_AUTH_SOCK"})
  JE=(env -i HOME="$HOME" PATH="$RCG_JULIA_DIR:/usr/bin:/bin" ${JULIA_DEPOT_PATH:+JULIA_DEPOT_PATH="$JULIA_DEPOT_PATH"})
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
  "${TO[@]}" "${SE[@]}" bash "$1" > "$L/$b.log" 2>&1
  fin "$b" $?
}

# job_test <key> <exe> <pkgdir> [test]
job_test() {
  local key=$1 exe=$2 pkg=$3 t=${4:-} log r
  log=$L/rust/$key.log
  echo "     Running ($exe) load $(cut -d' ' -f1 /proc/loadavg)" > "$log"
  (cd "$pkg" && "${TO[@]}" "${CE[@]}" CARGO_MANIFEST_DIR="$pkg" "$exe" ${t:+--exact "$t"}) >> "$log" 2>&1
  r=$?
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
emit_bin() { emit "bin:$1" "$(printf 'bin\t%s\t%s' "$1" "$2")" "$L/rust/$(basename "$1").rc"; }
emit_one() { emit "one:$1:$3" "$(printf 'one\t%s\t%s\t%s' "$1" "$2" "$3")" "$L/rust/$(basename "$1")__${3//:/_}.rc"; }
emit_shell() { emit "shell:$1" "$(printf 'shell\t%s' "$1")" "$L/steps/$(basename "$1" .sh).rc"; }

producer() {
  local k tgt t n exe pkg f s found out lrc
  local -A LISTED SPLIT_EXE SPLIT_PKG
  local SHELL_ALL=()
  local SLOW_FIRST=(
    lane_bridge/a_blackhole_is_unreachable_and_retried
    lane_bridge/a_daemon_outage_past_the_window_keeps_retrying
    lane_bridge/a_terminal_row_is_terminal_after_the_window
    fe_client/unresponsive_supervisor_expires_the_health_window
    test-spawn-capsule-workspace
    capsule_workspaces/capsule_supervisor_spawn_survives_fence_contention_without_marking_terminal
    test-hub-files
    supervisor
    test-status-floor
    test-relay-file-first
    test-registry-lock
  )
  # shell suites: the SLOW_FIRST ones are emitted early, the rest at step 5
  for f in "$D"/comm/core/tests/test-*.sh; do
    case $(basename "$f" .sh) in
      test-comm-e2e-readers|test-inbox-lock-onehost|test-inbox-lock-twohost|test-registry-twohost|test-registry-lock-twohost) ;;
      *) SHELL_ALL+=("$f") ;;
    esac
  done
  SHELL_ALL+=("$D/scripts/tests/installer-state.sh" "$D/scripts/tests/test-tunnel-plan.sh")

  for s in julia-root "${JULIA_PKGS[@]/#/julia-}"; do echo unrun > "$L/steps/${s//\//-}.rc"; done
  emit julia julia
  echo unrun > "$L/rust/doc.rc"
  echo unrun > "$L/steps/win-check.rc"
  echo unrun > "$L/steps/darwin-check.rc"
  emit cargo-chain cargo-chain
  [ "$BUILD_RC" -eq 0 ] && st rust-workspace

  # list the four slow binaries; a split binary runs only per test
  if [ "$BUILD_RC" -eq 0 ]; then
    for n in capsule_workspaces comm_wake lane_bridge fe_client; do
      if ! row_for "$n"; then echo "split-missing $n" >> "$L/summary.txt"; continue; fi
      out=$(cd "$pkg" && "${TO[@]}" "${CE[@]}" CARGO_MANIFEST_DIR="$pkg" "$exe" --list --format terse 2> "$L/rust/$n.list.log")
      lrc=$?
      out=$(grep ': test$' <<< "$out")
      if [ "$lrc" -ne 0 ] || [ -z "$out" ]; then echo "split-missing $n" >> "$L/summary.txt"; continue; fi
      LISTED[$n]=${out//: test/}
      SPLIT_EXE[$n]=$exe SPLIT_PKG[$n]=$pkg
      EMITTED["bin:$exe"]=1
    done
  fi

  for k in "${SLOW_FIRST[@]}"; do
    case $k in
      test-*)
        found=
        for f in "${SHELL_ALL[@]}"; do
          if [ "$(basename "$f" .sh)" = "$k" ]; then found=$f; fi
        done
        if [ -z "$found" ]; then echo "slow-first-missing $k" >> "$L/summary.txt"; continue; fi
        emit_shell "$found"
        ;;
      */*)
        [ "$BUILD_RC" -eq 0 ] || continue
        tgt=${k%%/*}; t=${k#*/}
        if [ -n "${LISTED[$tgt]:-}" ] && grep -qxF -- "$t" <<< "${LISTED[$tgt]}"; then
          emit_one "${SPLIT_EXE[$tgt]}" "${SPLIT_PKG[$tgt]}" "$t"
        else
          echo "slow-first-missing $k" >> "$L/summary.txt"
        fi
        ;;
      *)
        [ "$BUILD_RC" -eq 0 ] || continue
        if row_for "$k"; then emit_bin "$exe" "$pkg"; else echo "slow-first-missing $k" >> "$L/summary.txt"; fi
        ;;
    esac
  done
  st shell-suites
  for f in "${SHELL_ALL[@]}"; do
    emit_shell "$f"
  done
  [ "$BUILD_RC" -eq 0 ] || return 0
  for n in "${!LISTED[@]}"; do
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
  IFS=$'\t' read -r kind a b c <<< "$2"
  case $kind in
    julia) job_julia ;;
    cargo-chain) job_cargo_chain ;;
    shell) job_shell "$a" ;;
    bin) job_test "$(basename "$a")" "$a" "$b" ;;
    one) job_test "$(basename "$a")__${c//:/_}" "$a" "$b" "$c" ;;
  esac
  exit 0
fi

if [ "${1:-}" = --pipeline ]; then
  envs
  BUILD_RC=$RCG_BUILD_RC
  producer | xargs -d '\n' -n 1 -P "$2" bash "$SELF" --job
  exit $?
fi

# ---- main flow ----
CAP=${3:-10}
if [ $# -lt 2 ] || [ -z "${CARGO_TARGET_DIR:-}" ] || ! [[ $CAP =~ ^[1-9][0-9]*$ ]]; then
  echo "usage: CARGO_TARGET_DIR=... rc-gate.sh <checkout> <logdir> [cap >= 1]" >&2
  exit 2
fi
if [ -d "$2" ] && [ -n "$(ls -A "$2")" ]; then
  echo "rc-gate: logdir $2 exists and is not empty" >&2
  exit 2
fi
D=$(cd "$1" && pwd) || exit 2
mkdir -p "$2" || exit 2
L=$(cd "$2" && pwd)
mkdir -p "$L/rust" "$L/steps"
CARGO_BIN=$(command -v cargo) || { echo "rc-gate: cargo not found" >&2; exit 2; }
JULIA_BIN=$(command -v julia) || { echo "rc-gate: julia not found" >&2; exit 2; }
command -v jq > /dev/null || { echo "rc-gate: jq not found" >&2; exit 2; }
export RCG_D=$D RCG_L=$L RCG_CARGO_DIR=$(dirname "$CARGO_BIN") RCG_JULIA_DIR=$(dirname "$JULIA_BIN")
envs

echo "head $(git -C "$D" rev-parse HEAD) tree $(git -C "$D" rev-parse 'HEAD^{tree}')" > "$L/summary.txt"
echo "cap $CAP" >> "$L/summary.txt"

T0=$(date +%s)
M=$D/rust/Cargo.toml
BUILD_RC=0
st build-capsule
"${CE[@]}" cargo build --manifest-path "$M" -p sot-log --bin sot-capsule --locked > "$L/build-capsule.log" 2>&1
r=$?; rc build-capsule $r; [ $r -ne 0 ] && BUILD_RC=$r
st rust-build
"${CE[@]}" cargo test --manifest-path "$M" --workspace --locked --no-run --message-format=json \
  > "$L/rust-build.json" 2> "$L/rust-build.log"
r=$?; rc rust-build $r; [ $r -ne 0 ] && BUILD_RC=$r
: > "$L/tests.tsv"
if [ "$BUILD_RC" -eq 0 ]; then
  if ! jq -r 'select(.reason=="compiler-artifact" and .profile.test==true and .executable!=null)
    | [.target.name, .executable, (.manifest_path|rtrimstr("/Cargo.toml"))] | @tsv' \
    "$L/rust-build.json" > "$L/tests.tsv" || ! [ -s "$L/tests.tsv" ]; then
    BUILD_RC=1
    rc rust-build 1 tests.tsv
  fi
fi
export RCG_BUILD_RC=$BUILD_RC

( while kill -0 $$ 2> /dev/null; do cut -d' ' -f1 /proc/loadavg >> "$L/load.txt"; sleep 5; done ) &
SAMPLER=$!
INTR=0
setsid bash "$SELF" --pipeline "$CAP" &
PG=$!
trap 'INTR=1; kill -TERM -- -"$PG" 2> /dev/null; kill "$SAMPLER" 2> /dev/null' INT TERM
wait "$PG"
XS=$?
if [ "$INTR" -eq 1 ]; then wait "$PG" 2> /dev/null; fi

PASSED=0 FAILED=0 IGNORED=0 FL=0 BIN=0 SEEN=" "
for f in "$L"/rust/*.log; do
  [ -e "$f" ] || continue
  k=$(basename "$f" .log)
  case $k in
    *__*)
      p=${k%%__*}
      case $SEEN in *" $p "*) ;; *) SEEN+="$p "; BIN=$((BIN + 1)) ;; esac ;;
    *) BIN=$((BIN + $(grep -c '^test result' "$f"))) ;;
  esac
  read -r p1 f1 i1 < <(awk '/^test result/ {p+=$4; f+=$6; i+=$8} END {print p+0, f+0, i+0}' "$f")
  PASSED=$((PASSED + p1)) FAILED=$((FAILED + f1)) IGNORED=$((IGNORED + i1))
  FL=$((FL + $(grep -c '^test .* FAILED$' "$f")))
done

# one sweep over every result file: a failure or a job that never ran is a failure
RC=0
NOTES=
for f in "$L"/rust/*.rc "$L"/steps/*.rc; do
  [ -e "$f" ] || continue
  v=$(cat "$f")
  case $f in
    "$L"/rust/*) [ "$v" = 0 ] || { RC=101; NOTES+="rust-failed $(basename "$f" .rc) $v"$'\n'; } ;;
    *) [ "$v" = unrun ] && NOTES+="$(basename "$f" .rc) rc=unrun"$'\n' ;;
  esac
done
[ "$BUILD_RC" -eq 0 ] || RC=$BUILD_RC
rc rust-workspace "$RC" "binaries=$BIN passed=$PASSED failed=$FAILED ignored=$IGNORED $FL FAILED-lines"
[ -n "$NOTES" ] && printf '%s' "$NOTES" >> "$L/summary.txt"
[ "$XS" -eq 0 ] || rc xargs "$XS"
PEAK=$(sort -n "$L/load.txt" 2> /dev/null | tail -1)
echo "end $(date +%H:%M:%S) load $(cut -d' ' -f1-3 /proc/loadavg) cargo=$(pgrep -c cargo) peak-load ${PEAK:-0} wall $(($(date +%s) - T0))s" >> "$L/summary.txt"
if [ "$XS" -eq 0 ]; then echo ALLDONE >> "$L/summary.txt"; else echo "ALLDONE FAILED" >> "$L/summary.txt"; fi
[ "$INTR" -eq 1 ] && exit 130
exit 0
