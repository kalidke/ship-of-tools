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
if ! grep -Fq "test result: ok. $filtered_count passed; 0 failed; 0 ignored;" "$filtered_log"; then
    printf 'filtered-tests: missing exact pass summary expected=%s\n' "$filtered_count" >&2
    exit 1
fi
printf 'filtered-tests: expected=%s ok=true\n' "$filtered_count"
