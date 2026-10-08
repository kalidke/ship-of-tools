#!/usr/bin/env bash
# Shared verdict for one selected Rust test. Success requires the exact selected body's successful
# libtest completion, one passed test, no failures or ignored bodies, and child status zero. A
# zero-test or ignored-only run fails. Callers supply fresh invocation logs, their hang bound and any
# stronger content proof.
# The checker accepts the outer result only after complete, unambiguous captured-output boundaries; a captured summary or truncated capture cannot supply that result.

test_body_check() {
    [ "$#" -eq 3 ] && [ -n "$1" ] && [[ $2 =~ ^[0-9]+$ ]] && [ "$2" -le 255 ] && [ -r "$3" ] || {
        echo 'selected test: invalid arguments or unreadable log' >&2; return 2;
    }
    if [ "$2" -ne 0 ]; then
        printf 'selected test %s: child exit %s\n' "$1" "$2" >&2
        return "$2"
    fi
    python3 - "$1" "$3" <<'PY'
import re
import sys
name, path = sys.argv[1:]
try:
    lines = open(path, encoding='utf-8').read().splitlines()
except (OSError, UnicodeError) as error:
    print(f'selected test {name}: unreadable log: {error}', file=sys.stderr)
    sys.exit(2)

def structural(line):
    return line in ('successes:', 'failures:') or line.startswith('---- ')

def parse_outer():
    # Cargo may precede the harness with build diagnostics. Once the run starts, consume every
    # outer record in order; count/progress/result strings inside stdout remain captured data.
    start = next((i for i, line in enumerate(lines) if re.fullmatch(r'running \d+ tests?', line)), None)
    if start is None or any(structural(line) or line.startswith('test ') for line in lines[:start]):
        raise ValueError('missing or ambiguous outer harness')
    if lines[start] != 'running 1 test':
        raise ValueError('outer harness did not run exactly one body')
    position = start + 1

    def take():
        nonlocal position
        while position < len(lines) and not lines[position].strip():
            position += 1
        if position == len(lines):
            raise ValueError('incomplete captured/outer structure')
        line = lines[position]
        position += 1
        return line

    if take() != f'test {name} ... ok':
        raise ValueError('exact successful body completion missing or mismatched')
    record = take()
    if record == 'successes:':
        record = take()
        if record == f'---- {name} stdout ----':
            # The first structural boundary must close stdout. Another opening or closing
            # delimiter is ambiguous, even if test output printed it deliberately.
            while position < len(lines) and not structural(lines[position]):
                position += 1
            record = take()
        if record != 'successes:' or take() != f'    {name}':
            raise ValueError('missing or ambiguous capture closure/selected-name list')
        record = take()
    if not re.fullmatch(r'test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; \d+ filtered out; finished in [0-9.]+s', record):
        raise ValueError('outer successful one-body result missing or mismatched')
    if any(line.strip() for line in lines[position:]):
        raise ValueError('ambiguous trailing harness output')

try:
    parse_outer()
except ValueError as error:
    print(f'selected test {name}: {error}', file=sys.stderr)
    sys.exit(101)
PY
}

test_body_run() {
    [ "$#" -ge 4 ] && [ -n "$1" ] && [ "$3" = -- ] || {
        echo 'selected test: invalid runner arguments' >&2; return 2;
    }
    local name=$1 log=$2 raw checked
    shift 3
    : > "$log" || return 2
    if "$@" > "$log" 2>&1; then raw=0; else raw=$?; fi
    if test_body_check "$name" "$raw" "$log"; then checked=0; else checked=$?; fi
    cat "$log"
    return "$checked"
}
