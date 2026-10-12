#!/usr/bin/env bash
# Real libtest controls for the filtered-group verdict; all witnesses belong to temporary roots.
set -euo pipefail
if [[ $# != 1 ]]; then
    echo 'usage: test-filtered-tests.sh ABSOLUTE_FIXTURE' >&2
    exit 2
fi
python3 - "$1" "$(dirname -- "${BASH_SOURCE[0]}")/../.." <<'PY'
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

fixture = Path(sys.argv[1])
if not fixture.is_absolute() or not fixture.is_file() or not os.access(fixture, os.X_OK):
    print('filtered-count: absolute compiled executable fixture required', file=sys.stderr)
    sys.exit(2)
fixture = fixture.resolve()
output = Path(sys.argv[2]).resolve() / 'dev/output'
output.mkdir(parents=True, exist_ok=True)
runner = output.parent.parent / 'scripts/tests/run-filtered-tests.sh'
flags = ['--test-threads=1', '--format', 'pretty', '--color', 'never', '--show-output']
evidence = {}


def check(ok, detail):
    if not ok:
        raise AssertionError(detail)


def run(name, expected, selected, exact=True):
    """The runner over the fixture in a fresh root: exit status, output, libtest log and the bodies that ran."""
    evidence.clear()
    with tempfile.TemporaryDirectory(prefix=f'filtered-count-{name}.', dir=output) as temporary:
        root = Path(temporary).resolve()
        (root / 'iso-sh-fixture-request').touch()
        if name == 'failed':
            (root / 'iso-sh-panic-request').touch()
        log = root if name == 'capture-error' else root / 'libtest.log'
        argv = ['bash', str(runner), str(expected), str(log), '--', str(fixture), *selected]
        argv += ['--exact'] if exact else []
        try:
            result = subprocess.run(argv + flags, cwd=root, stdout=subprocess.PIPE,
                                    stderr=subprocess.STDOUT, timeout=60)
            code, child = result.returncode, result.stdout
        except subprocess.TimeoutExpired as error:
            code, child = 'timeout after 60 seconds', error.output or b''
        bodies = sorted(path.name.removeprefix('witness-') for path in root.glob('witness-*'))
        text = child.decode('utf-8', 'replace')
        evidence['status'] = json.dumps({'exit': code, 'witnesses': bodies})
        evidence['output'] = text.replace(str(root), '<temporary-root>').replace(str(fixture), '<fixture>')
        evidence['libtest'] = log.read_text() if log.is_file() else ''
    for suffix, saved in evidence.items():
        (output / f'filtered-count-{name}.{suffix}').write_text(saved + '\n')
    check(isinstance(code, int), 'child wait timed out after 60 seconds')
    return code, text, evidence['libtest'], bodies


# name, expected count, selection, --exact, the log's summary lines in order, bodies that ran, runner accepts
cases = [
    ('absent', 1, ['absent'], True, ['ok. 0 passed; 0 failed; 0 ignored;'], [], False),
    ('ordinary', 1, ['ordinary'], True, ['ok. 1 passed; 0 failed; 0 ignored;'], ['ordinary'], True),
    ('group', 2, ['near'], False, ['ok. 2 passed; 0 failed; 0 ignored;'], ['near', 'qualified-near'], True),
    ('wrong-count', 1, ['near'], False, ['ok. 2 passed; 0 failed; 0 ignored;'], ['near', 'qualified-near'], False),
    ('ignored', 1, ['ordinary', 'qualified::ignored_positive'], True, ['ok. 1 passed; 0 failed; 1 ignored;'],
     ['ordinary'], False),
    ('failed', 1, ['ordinary'], True, ['FAILED. 0 passed; 1 failed; 0 ignored;'], ['ordinary'], False),
    # The first line is the summary the misleading body prints into its captured output.
    ('misleading', 1, ['misleading', 'ordinary'], True,
     ['ok. 1 passed; 0 failed; 0 ignored;', 'ok. 2 passed; 0 failed; 0 ignored;'], ['misleading', 'ordinary'], False),
]

name = 'setup'
try:
    for name, expected, selected, exact, summaries, ran, accepted in cases:
        code, text, raw, bodies = run(name, expected, selected, exact)
        lines = [line for line in raw.splitlines() if line.startswith('test result: ')]
        check(len(lines) == len(summaries) and all(line.startswith(f'test result: {summary}')
                                                   for line, summary in zip(lines, summaries)),
              f'summary lines {lines}, want lines starting {summaries}')
        check(bodies == ran, f'bodies that ran: {bodies}, want {ran}')
        check((code == 0) == accepted, f'runner exit {code}: this selection must be '
              + ('accepted' if accepted else 'rejected'))
        check(not accepted or f'filtered-tests: expected={expected} ok=true' in text, 'missing count receipt')
        found = [line.removeprefix('filtered-tests: found: ') for line in text.splitlines()
                 if line.startswith('filtered-tests: found: ')]
        check(accepted or name == 'failed' or found == lines,
              f'runner reported {found}, want the summary lines {lines}')
        check(name != 'failed' or 'requested fixture panic' in raw, 'missing the failed body cause')
        print(f'filtered-count proof={name} ok=true', flush=True)
    name = 'invalid-count'
    code, text, raw, bodies = run(name, 0, ['ordinary'])
    check(code == 2 and not bodies, f'runner exit {code}, bodies {bodies}: want usage exit 2 before the fixture')
    print(f'filtered-count proof={name} ok=true', flush=True)
    name = 'capture-error'
    code, text, raw, bodies = run(name, 1, ['ordinary'])
    check(code != 0 and 'Is a directory' in text, f'runner exit {code}: an unwritable log must fail the capture')
    print(f'filtered-count proof={name} ok=true', flush=True)
    print(f'filtered-count completed_cases={len(cases) + 2} ok=true', flush=True)
except (AssertionError, OSError, UnicodeError) as error:
    print(f'filtered-count FAIL case={name}: {error}', file=sys.stderr)
    for suffix, saved in evidence.items():
        print(f'--- filtered-count-{name}.{suffix} ---\n{saved}', file=sys.stderr)
    sys.stderr.flush()
    sys.exit(1)
PY
