#!/usr/bin/env bash
set -euo pipefail
if [[ $# -lt 4 || ! $1 =~ ^[1-9][0-9]*$ || $3 != -- ]]; then
    echo 'filtered-tests: expected positive count, log, -- and command' >&2
    exit 2
fi
filtered_count=$1
filtered_log=$2
shift 3
mkdir -p -- "$(dirname -- "$filtered_log")"
if ! "$@" 2>&1 | tee "$filtered_log"; then
    echo 'filtered-tests: command or capture failed' >&2
    exit 1
fi
# A body's captured output can print a summary line of its own, so the log must hold exactly one.
filtered_summary=$(grep '^test result: ' "$filtered_log" || true)
if [[ $filtered_summary == *$'\n'* ||
    $filtered_summary != "test result: ok. $filtered_count passed; 0 failed; 0 ignored;"* ]]; then
    printf 'filtered-tests: want exactly one summary line, starting "test result: ok. %s passed; 0 failed; 0 ignored;"\n' \
        "$filtered_count" >&2
    exit 1
fi
printf 'filtered-tests: expected=%s ok=true\n' "$filtered_count"
