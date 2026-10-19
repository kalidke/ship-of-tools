#!/usr/bin/env bash
# test-rc-gate-basename.sh: rc-gate.sh names a shell suite's result and log by its path in the checkout, so two suites
# with one basename (comm/tests and agents/tests each hold a test-temp-files.sh) keep their own verdicts, and emit refuses
# a result file that two jobs would share, before any job runs. It runs the real --pipeline over stand-in suites and stub
# cargo and julia, so nothing real is built or tested. Exit: failed checks.
set -u
gate=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/rc-gate.sh
# macOS's TMPDIR ends in a slash: strip it, or the paths below hold `//`.
base="${TMPDIR:-/tmp}"
work=$(mktemp -d "${base%/}/rc-gate-basename-XXXXXX") || exit 2
trap 'rm -rf "${work:?}"' EXIT
fail=0
check() { if eval "$2"; then echo "PASS: $1"; else echo "FAIL: $1"; fail=$((fail + 1)); fi; }
# the main flow's rule over result files: each reads 0, and a missing one fails
verdict() {
  local f
  for f in "$@"; do [ "$(cat "$f" 2> /dev/null)" = 0 ] || { echo "ALLDONE FAILED"; return; }; done
  echo ALLDONE
}
# the suite results as name=value
results() {
  local f
  for f in "$1"/steps/*test-temp-files.rc; do printf '%s=%s ' "$(basename "$f")" "$(cat "$f")"; done
  echo
}
# stand-in checkout: two suites with one basename, one failing (comm) and one passing (agents)
repo=$work/repo
mkdir -p "$repo/comm/tests" "$repo/agents/tests"
printf '#!/usr/bin/env bash\necho comm stand-in\nexit 1\n' > "$repo/comm/tests/test-temp-files.sh"
printf '#!/usr/bin/env bash\necho agents stand-in\nexit 0\n' > "$repo/agents/tests/test-temp-files.sh"
# a case's folder: its logdir, an empty tests.tsv, and stub cargo and julia that only record that they ran
case_dir() {
  mkdir -p "$1/L/rust" "$1/L/steps" "$1/stub" "$1/ran"
  for t in cargo julia; do printf '#!/bin/sh\ntouch "%s/ran/%s"\n' "$1" "$t" > "$1/stub/$t"; chmod +x "$1/stub/$t"; done
  : > "$1/L/tests.tsv"
}
# pipeline <case>: the real --pipeline over the case's stand-ins; sets rcRun and errRun
pipeline() {
  errRun=$(RCG_D=$repo RCG_L=$1/L RCG_BUILD_RC=0 RCG_CARGO_DIR=$1/stub RCG_JULIA_DIR=$1/stub \
    bash "$gate" --pipeline 1 2>&1 > /dev/null)
  rcRun=$?
}

A=$work/A
case_dir "$A"
pipeline "$A"
rcA=$rcRun
echo "A: one basename, two suites (comm fails, agents passes)"
echo "  pipeline exit $rcA; results: $(results "$A/L"); verdict: $(verdict "$A"/L/steps/*test-temp-files.rc)"
check "the failing suite keeps its own result" '[ "$(cat "$A/L/steps/comm-tests-test-temp-files.rc" 2> /dev/null)" = 1 ]'
check "the passing suite keeps its own result" '[ "$(cat "$A/L/steps/agents-tests-test-temp-files.rc" 2> /dev/null)" = 0 ]'
check "each suite logs to its own file" 'grep -qx "comm stand-in" "$A/L/comm-tests-test-temp-files.log" && grep -qx "agents stand-in" "$A/L/agents-tests-test-temp-files.log"'
check "the verdict over the suite results is the comm failure" '[ "$(verdict "$A"/L/steps/*test-temp-files.rc)" = "ALLDONE FAILED" ]'
check "the pipeline refuses nothing that has distinct results" '[ "$rcA" -eq 0 ]'

B=$work/B
case_dir "$B"
mkdir -p "$B/a" "$B/b"
for n in a b; do printf '#!/bin/sh\ntouch "%s/ran/tool-%s"\n' "$B" "$n" > "$B/$n/tool"; chmod +x "$B/$n/tool"; done
printf 'tool\t%s\t%s\ntool\t%s\t%s\n' "$B/a/tool" "$B/a" "$B/b/tool" "$B/b" > "$B/L/tests.tsv"
pipeline "$B"
rcB=$rcRun
errB=$errRun
echo "B: two binaries with one basename"
echo "  pipeline exit $rcB; stderr: $errB; jobs run: $(ls -A "$B/ran" | tr '\n' ' ')"
check "the second emit of one result file is refused with exit 2" '[ "$rcB" -eq 2 ]'
check "the refusal names the shared result" '[[ $errB == *"rust/tool.rc"* ]]'
check "the refusal runs no job" '[ -z "$(ls -A "$B/ran")" ]'

echo "failed: $fail"
exit "$fail"
