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
cases = [
    ('absent', 1, 'absent', True, 0, 0, []),
    ('ordinary', 1, 'ordinary', True, 1, 0, ['ordinary']),
    ('group', 2, 'near', False, 2, 0, ['near', 'qualified-near']),
    ('wrong-count', 1, 'near', False, 2, 0, ['near', 'qualified-near']),
    ('ignored', 1, 'qualified::ignored_positive', True, 0, 1, []),
    ('failed', 1, 'ordinary', True, 0, 0, ['ordinary']),
    ('invalid-count', 0, 'ordinary', True, 0, 0, []),
    ('capture-error', 1, 'ordinary', True, 1, 0, ['ordinary']),
]

try:
    for name, expected, selected, exact, passed, ignored, witnesses in cases:
        with tempfile.TemporaryDirectory(prefix=f'filtered-count-{name}.', dir=output) as temporary:
            root = Path(temporary).resolve()
            (root / 'iso-sh-fixture-request').touch()
            if name == 'failed':
                (root / 'iso-sh-panic-request').touch()
            log = root if name == 'capture-error' else root / 'libtest.log'
            argv = ['bash', str(runner), str(expected), str(log), '--', str(fixture), selected]
            if exact:
                argv.append('--exact')
            argv.extend(flags)
            try:
                result = subprocess.run(argv, cwd=root, stdout=subprocess.PIPE,
                                        stderr=subprocess.STDOUT, timeout=60)
            except subprocess.TimeoutExpired as error:
                (output / f'filtered-count-{name}.output').write_bytes(error.output or b'')
                (output / f'filtered-count-{name}.status').write_text('timeout after 60 seconds\n')
                raise AssertionError(f'filtered-count case={name}: child wait timed out after 60 seconds')
            text = result.stdout.decode('utf-8', 'strict')
            raw = log.read_text() if log.is_file() else ''
            saved = text.replace(str(root), '<temporary-root>').replace(str(fixture), '<fixture>')
            (output / f'filtered-count-{name}.output').write_text(saved)
            (output / f'filtered-count-{name}.libtest').write_text(raw)
            (output / f'filtered-count-{name}.status').write_text(json.dumps({
                'exit': result.returncode,
                'witnesses': sorted(path.name for path in root.glob('witness-*')),
            }) + '\n')
            if name == 'invalid-count':
                assert result.returncode == 2, 'zero expected count was not a usage error'
                assert not (root / 'witness-ordinary').exists(), 'invalid count started the fixture'
            elif name == 'capture-error':
                assert result.returncode != 0, 'capture failure was accepted'
                assert 'Is a directory' in text, 'capture did not fail writing the directory'
            else:
                failed = 1 if name == 'failed' else 0
                status = 'FAILED' if failed else 'ok'
                summary = f'test result: {status}. {passed} passed; {failed} failed; {ignored} ignored;'
                assert summary in raw, f'{name}: missing real libtest summary: {summary}'
                if name == 'absent':
                    assert 'running 0 tests' in raw, 'absent filter entered a body'
                    assert result.returncode != 0, 'zero-test filter was accepted: expected=1 passed=0'
                elif name in ('ordinary', 'group'):
                    assert result.returncode == 0, f'{name}: completed selection was rejected'
                    assert f'filtered-tests: expected={expected} ok=true' in text, 'missing count receipt'
                else:
                    assert result.returncode != 0, f'{name}: invalid selection was accepted'
                for witness in witnesses:
                    assert (root / f'witness-{witness}').is_file(), f'{name}: missing body witness {witness}'
                if name == 'ignored':
                    assert not (root / 'witness-ignored-positive').exists(), 'ignored body executed'
                if name == 'failed':
                    assert 'requested fixture panic' in raw, 'missing actual failed-body cause'
            print(f'filtered-count proof={name} ok=true', flush=True)
    print('filtered-count completed_cases=8 ok=true', flush=True)
except (AssertionError, OSError, UnicodeError) as error:
    print(f'filtered-count FAIL: {error}', file=sys.stderr, flush=True)
    sys.exit(1)
PY
