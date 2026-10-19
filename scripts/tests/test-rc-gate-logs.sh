#!/usr/bin/env bash
# test-rc-gate-logs.sh: no command an rc-gate job runs has its standard output or error in the logs folder. The command
# writes them in the container's private folder (XDG_RUNTIME_DIR, on the host's own disk), and the job appends them to
# its logs when the command ends, with the command's status as its result. The logs folder is wherever the caller reads
# it, often a network home, where each close of a written file waits for the file server to commit it; a child closes
# the standard streams it inherited as it sets up its own, so a test's bound on that setup would time the file server.
# Each job kind runs here, through `rc-gate.sh --job`, on stand-ins (julia, cargo, a test binary, a shell suite) that
# name their own standard streams and exit 7. Linux: the streams are read from /proc. Exit: failed checks.
set -u
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
base="${TMPDIR:-/tmp}"
work=$(mktemp -d "${base%/}/rc-gate-logs-XXXXXX") || exit 2
trap 'rm -rf "${work:?}"' EXIT
work=$(cd "$work" && pwd -P)
L=$work/logs
mkdir -p "$L/rust" "$L/steps" "$work/run" "$work/cargo-bin" "$work/julia-bin" "$work/pkg"
# A stand-in prints `out <its stdout>` and `err <its stderr>` and exits 7; the warm step's julia exits 0, so the
# lifetime job goes on to its harness.
for s in julia-bin/julia cargo-bin/cargo harness suite.sh; do
  cat > "$work/$s" << 'STAND_IN'
#!/bin/sh
echo "out $(readlink /proc/$$/fd/1)"
echo "err $(readlink /proc/$$/fd/2)" >&2
case "$*" in *warm_pluto.jl*) exit 0 ;; esac
exit 7
STAND_IN
  chmod +x "$work/$s"
done
job() {
  RCG_D=$(cd "$here/../.." && pwd) RCG_L=$L RCG_JULIA_DIR=$work/julia-bin RCG_CARGO_DIR=$work/cargo-bin \
    RCG_LIFETIME=$work/harness XDG_RUNTIME_DIR=$work/run CARGO_TARGET_DIR=$work/target HOME=$work \
    bash "$here/rc-gate.sh" --job "$1" > "$work/job.out" 2>&1 || { echo "FAIL: the job $1 itself failed"; cat "$work/job.out"; }
}
job lifetime
job julia
job cargo-chain
job "$(printf 'bin\tunit\t%s\t%s' "$work/harness" "$work/pkg")"
job "$(printf 'shell\tsuite\t%s' "$work/suite.sh")"

fail=0
# check LOG ERR RESULT: the stand-in's lines reached LOG and ERR, no stream they name is in the logs folder, and RESULT
# (- for none) holds the stand-in's status
check() {
  local lines rc=- files=("$1")
  [ "$2" = "$1" ] || files+=("$2")
  lines=$(cat "${files[@]}" 2> /dev/null | grep -E '^(out|err) ')
  [ "$3" = - ] || rc=$(cat "$3" 2> /dev/null)
  if grep -q '^out ' <<< "$lines" && grep -q '^err ' <<< "$lines" && ! grep -qF " $L/" <<< "$lines" &&
    { [ "$3" = - ] || [ "$rc" = 7 ]; }; then
    echo "PASS: ${1#"$L/"}"
  else
    echo "FAIL: ${1#"$L/"} (result $rc): $(tr '\n' ' ' <<< "${lines:-no stand-in line}")"
    fail=$((fail + 1))
  fi
}
check "$L/lifetime-warm.log" "$L/lifetime-warm.log" -
check "$L/rust/daemon_lifetime-faults.log" "$L/rust/daemon_lifetime-faults.stderr" "$L/rust/daemon_lifetime-faults.rc"
n=0
for r in "$L"/steps/julia-*.rc; do
  [ -e "$r" ] || continue
  n=$((n + 1)); s=$(basename "$r" .rc); check "$L/$s.log" "$L/$s.log" "$r"
done
[ "$n" -gt 1 ] || { echo "FAIL: the julia job left $n results"; fail=$((fail + 1)); }
check "$L/rust/doc.log" "$L/rust/doc.log" "$L/rust/doc.rc"
check "$L/win-check.log" "$L/win-check.log" "$L/steps/win-check.rc"
check "$L/darwin-check.log" "$L/darwin-check.log" "$L/steps/darwin-check.rc"
check "$L/rust/unit.log" "$L/rust/unit.stderr" "$L/rust/unit.rc"
check "$L/suite.log" "$L/suite.log" "$L/steps/suite.rc"
echo "failed: $fail"
exit "$fail"
