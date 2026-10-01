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
# manager. Needs jq. Exit 2 on bad args; otherwise 0 and the verdict is in
# <logdir>/summary.txt (ends in ALLDONE).

SELF=$(readlink -f "$0")

envs() {
  D=$RCG_D L=$RCG_L
  CE=(env -i HOME="$HOME" PATH="$RCG_CARGO_DIR:/usr/bin:/bin" TMPDIR=/tmp CARGO_TARGET_DIR="$CARGO_TARGET_DIR"
    ${CARGO_PROFILE_DEV_DEBUG:+CARGO_PROFILE_DEV_DEBUG="$CARGO_PROFILE_DEV_DEBUG"})
  SE=(env -i HOME="$HOME" PATH=/usr/bin:/bin ${SSH_AUTH_SOCK:+SSH_AUTH_SOCK="$SSH_AUTH_SOCK"})
  JE=(env -i HOME="$HOME" PATH="$RCG_JULIA_DIR:/usr/bin:/bin" ${JULIA_DEPOT_PATH:+JULIA_DEPOT_PATH="$JULIA_DEPOT_PATH"})
}

st() { echo "$1 start $(date +%H:%M:%S) load $(cut -d' ' -f1-3 /proc/loadavg) cargo=$(pgrep -c cargo)" >> "$L/summary.txt"; }
rc() { echo "$1 rc=$2 ${3:-}" >> "$L/summary.txt"; }

job_julia() {
  st julia
  "${JE[@]}" julia --project="$D" -e 'using Pkg; Pkg.test()' > "$L/julia-root.log" 2>&1
  rc julia-root $?
  local p s
  for p in core julia/kernel julia/repl julia/plugins/pdf-file julia/plugins/video-file julia/sotlog; do
    s=julia-$(echo "$p" | tr / -)
    "${JE[@]}" julia --project="$D/$p" -e 'using Pkg; Pkg.instantiate(); Pkg.test()' > "$L/$s.log" 2>&1
    rc "$s" $?
  done
}

job_cargo_chain() {
  local M="$D/rust/Cargo.toml" r
  st rust-doc
  "${CE[@]}" cargo test --manifest-path "$M" --workspace --locked --doc > "$L/rust/doc.log" 2>&1
  r=$?
  echo "$r" > "$L/rust/doc.rc"
  rc rust-doc "$r"
  st win-check
  "${CE[@]}" CC_x86_64_pc_windows_gnu=gcc AR_x86_64_pc_windows_gnu=ar \
    cargo check --manifest-path "$M" --target x86_64-pc-windows-gnu -p sot-log -p sot-backend -p sot-frontend --locked \
    > "$L/win-check.log" 2>&1 &&
  "${CE[@]}" CC_x86_64_pc_windows_gnu=gcc AR_x86_64_pc_windows_gnu=ar \
    cargo check --manifest-path "$M" --target x86_64-pc-windows-gnu -p sot-backend -p sot-log --all-targets --locked \
    >> "$L/win-check.log" 2>&1
  rc win-check $?
  st darwin-check
  "${CE[@]}" cargo check --manifest-path "$M" --target aarch64-apple-darwin -p sot-log -p sot-backend --all-targets --locked \
    > "$L/darwin-check.log" 2>&1
  rc darwin-check $?
}

job_shell() {
  local b
  b=$(basename "$1")
  "${SE[@]}" bash "$1" > "$L/$b.log" 2>&1
  rc "$b" $?
}

# job_test <key> <exe> <pkgdir> [test]
job_test() {
  local key=$1 exe=$2 pkg=$3 t=${4:-} log r
  log=$L/rust/$key.log
  echo "     Running ($exe) load $(cut -d' ' -f1 /proc/loadavg)" > "$log"
  if [ -n "$t" ]; then
    (cd "$pkg" && "${CE[@]}" CARGO_MANIFEST_DIR="$pkg" "$exe" --exact "$t") >> "$log" 2>&1
  else
    (cd "$pkg" && "${CE[@]}" CARGO_MANIFEST_DIR="$pkg" "$exe") >> "$log" 2>&1
  fi
  r=$?
  echo "$r" > "$L/rust/$key.rc"
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

# ---- main flow ----
CAP=${3:-10}
if [ $# -lt 2 ] || [ -z "${CARGO_TARGET_DIR:-}" ]; then
  echo "usage: CARGO_TARGET_DIR=... rc-gate.sh <checkout> <logdir> [cap]" >&2
  exit 2
fi
if [ -d "$2" ] && [ -n "$(ls -A "$2")" ]; then
  echo "rc-gate: logdir $2 exists and is not empty" >&2
  exit 2
fi
D=$(cd "$1" && pwd) || exit 2
mkdir -p "$2" || exit 2
L=$(cd "$2" && pwd)
mkdir -p "$L/rust"
CARGO_BIN=$(command -v cargo) || { echo "rc-gate: cargo not found" >&2; exit 2; }
JULIA_BIN=$(command -v julia) || { echo "rc-gate: julia not found" >&2; exit 2; }
command -v jq > /dev/null || { echo "rc-gate: jq not found" >&2; exit 2; }
export RCG_D=$D RCG_L=$L RCG_CAP=$CAP RCG_CARGO_DIR=$(dirname "$CARGO_BIN") RCG_JULIA_DIR=$(dirname "$JULIA_BIN")
export CARGO_TARGET_DIR
envs

echo "head $(git -C "$D" rev-parse HEAD) tree $(git -C "$D" rev-parse 'HEAD^{tree}')" > "$L/summary.txt"
echo "cap $CAP" >> "$L/summary.txt"

M=$D/rust/Cargo.toml
BUILD_RC=0
st build-capsule
"${CE[@]}" cargo build --manifest-path "$M" -p sot-log --bin sot-capsule --locked > "$L/build-capsule.log" 2>&1
r=$?; rc build-capsule $r; [ $r -ne 0 ] && BUILD_RC=$r
st rust-build
"${CE[@]}" cargo test --manifest-path "$M" --workspace --locked --no-run --message-format=json \
  > "$L/rust-build.json" 2> "$L/rust-build.log"
r=$?; rc rust-build $r; [ $r -ne 0 ] && BUILD_RC=$r
if [ "$BUILD_RC" -eq 0 ]; then
  jq -r 'select(.reason=="compiler-artifact" and .profile.test==true and .executable!=null)
    | [.target.name, .executable, (.manifest_path|rtrimstr("/Cargo.toml"))] | @tsv' \
    "$L/rust-build.json" > "$L/tests.tsv"
else
  : > "$L/tests.tsv"
fi

declare -A EMITTED
emit() {
  [ -n "${EMITTED[$1]:-}" ] && return
  EMITTED[$1]=1
  printf '%s\n' "$2"
}

# shell suites: the SLOW_FIRST ones are emitted early, the rest at step 5
SHELL_ALL=()
for f in "$D"/comm/core/tests/test-*.sh; do
  case $(basename "$f" .sh) in
    test-comm-e2e-readers|test-inbox-lock-onehost|test-inbox-lock-twohost|test-registry-twohost|test-registry-lock-twohost) ;;
    *) SHELL_ALL+=("$f") ;;
  esac
done
SHELL_ALL+=("$D/scripts/tests/installer-state.sh" "$D/scripts/tests/test-tunnel-plan.sh")

SPLIT_USED=()
rows_for() { awk -F'\t' -v n="$1" '$1==n' "$L/tests.tsv"; }

producer() {
  local k tgt t rows n exe pkg f line name found
  emit julia "$(printf 'julia')"
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
  if [ "$BUILD_RC" -eq 0 ]; then
    st rust-workspace
    emit cargo-chain "$(printf 'cargo-chain')"
  fi
  for k in "${SLOW_FIRST[@]}"; do
    case $k in
      test-*)
        found=
        for f in "${SHELL_ALL[@]}"; do
          if [ "$(basename "$f" .sh)" = "$k" ]; then found=$f; fi
        done
        if [ -z "$found" ]; then echo "slow-first-missing $k" >> "$L/summary.txt"; continue; fi
        emit "shell:$found" "$(printf 'shell\t%s' "$found")"
        ;;
      */*)
        [ "$BUILD_RC" -eq 0 ] || continue
        tgt=${k%%/*}; t=${k#*/}
        rows=$(rows_for "$tgt")
        if [ -z "$rows" ] || [ "$(echo "$rows" | wc -l)" -ne 1 ]; then
          echo "slow-first-missing $k" >> "$L/summary.txt"; continue
        fi
        IFS=$'\t' read -r _ exe pkg <<< "$rows"
        emit "one:$exe:$t" "$(printf 'one\t%s\t%s\t%s' "$exe" "$pkg" "$t")"
        ;;
      *)
        [ "$BUILD_RC" -eq 0 ] || continue
        rows=$(rows_for "$k")
        if [ -z "$rows" ] || [ "$(echo "$rows" | wc -l)" -ne 1 ]; then
          echo "slow-first-missing $k" >> "$L/summary.txt"; continue
        fi
        IFS=$'\t' read -r _ exe pkg <<< "$rows"
        emit "bin:$exe" "$(printf 'bin\t%s\t%s' "$exe" "$pkg")"
        ;;
    esac
  done
  st shell-suites
  for f in "${SHELL_ALL[@]}"; do
    emit "shell:$f" "$(printf 'shell\t%s' "$f")"
  done
  [ "$BUILD_RC" -eq 0 ] || return 0
  for n in capsule_workspaces comm_wake lane_bridge fe_client; do
    rows=$(rows_for "$n")
    if [ -z "$rows" ] || [ "$(echo "$rows" | wc -l)" -ne 1 ]; then
      echo "split-missing $n" >> "$L/summary.txt"
      while IFS=$'\t' read -r _ exe pkg; do
        [ -n "$exe" ] && emit "bin:$exe" "$(printf 'bin\t%s\t%s' "$exe" "$pkg")"
      done <<< "$rows"
      continue
    fi
    IFS=$'\t' read -r _ exe pkg <<< "$rows"
    echo "$n" >> "$L/split-used.txt"
    while IFS= read -r line; do
      case $line in
        *": test") name=${line%": test"}
          emit "one:$exe:$name" "$(printf 'one\t%s\t%s\t%s' "$exe" "$pkg" "$name")" ;;
      esac
    done < <(cd "$pkg" && "${CE[@]}" CARGO_MANIFEST_DIR="$pkg" "$exe" --list --format terse 2>/dev/null)
  done
  while IFS=$'\t' read -r _ exe pkg; do
    emit "bin:$exe" "$(printf 'bin\t%s\t%s' "$exe" "$pkg")"
  done < "$L/tests.tsv"
}

( while :; do cut -d' ' -f1 /proc/loadavg >> "$L/load.txt"; sleep 5; done ) &
SAMPLER=$!
T0=$(date +%s)
producer | xargs -d '\n' -n 1 -P "$CAP" bash "$SELF" --job
kill "$SAMPLER" 2> /dev/null
wait "$SAMPLER" 2> /dev/null

R=$(cat "$L"/rust/*.log 2> /dev/null | grep -c '^test result')
read -r PASSED FAILED IGNORED < <(cat "$L"/rust/*.log 2> /dev/null | awk '/^test result/ {p+=$4; f+=$6; i+=$8} END {print p+0, f+0, i+0}')
FL=$(cat "$L"/rust/*.log 2> /dev/null | grep -c '^test .* FAILED$')
ONE=$(find "$L/rust" -name '*__*.log' | wc -l)
NS=0
[ -f "$L/split-used.txt" ] && while read -r n; do
  ls "$L"/rust/"$n"-*__*.log > /dev/null 2>&1 && NS=$((NS + 1))
done < "$L/split-used.txt"
BIN=$((R - ONE + NS))
RC=0
for f in "$L"/rust/*.rc; do
  [ -e "$f" ] || continue
  [ "$(cat "$f")" = 0 ] || RC=101
done
[ "$BUILD_RC" -eq 0 ] || RC=$BUILD_RC
rc rust-workspace "$RC" "binaries=$BIN passed=$PASSED failed=$FAILED ignored=$IGNORED $FL FAILED-lines"
for f in "$L"/rust/*.rc; do
  [ -e "$f" ] || continue
  [ "$(cat "$f")" = 0 ] || echo "rust-failed $(basename "$f" .rc)" >> "$L/summary.txt"
done
PEAK=$(sort -n "$L/load.txt" 2> /dev/null | tail -1)
echo "end $(date +%H:%M:%S) load $(cut -d' ' -f1-3 /proc/loadavg) cargo=$(pgrep -c cargo) peak-load ${PEAK:-0} wall $(($(date +%s) - T0))s" >> "$L/summary.txt"
echo ALLDONE >> "$L/summary.txt"
