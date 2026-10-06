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

starts = [i for i, line in enumerate(lines) if re.fullmatch(r'running \d+ tests?', line)]
reason = 'missing or ambiguous outer harness'
if starts:
    start = starts[0]
    boundary = next((i for i in range(start + 1, len(lines))
                     if lines[i] in ('successes:', 'failures:') or lines[i].startswith('test result:')), len(lines))
    completed = [line for line in lines[start + 1:boundary] if line.startswith('test ')]
    summaries = [(i, lines[i]) for i in range(boundary, len(lines)) if lines[i].startswith('test result:')]
    # Captured stdout can contain arbitrary harness-like lines. Only the final outer record counts;
    # no test output is printed between that record and Cargo's following diagnostic/footer.
    result_at, result = summaries[-1] if summaries else (len(lines), '')
    expected = re.fullmatch(r'test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; \d+ filtered out; finished in [0-9.]+s', result)
    if lines[start] != 'running 1 test':
        reason = 'outer harness did not run exactly one body'
    elif completed != [f'test {name} ... ok']:
        reason = 'exact successful body completion missing or mismatched'
    elif not expected:
        reason = 'outer successful one-body result missing or mismatched'
    elif any(line.startswith(('running ', 'test ', '---- ')) for line in lines[result_at + 1:]):
        reason = 'ambiguous trailing harness output'
    else:
        sys.exit(0)
print(f'selected test {name}: {reason}', file=sys.stderr)
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
