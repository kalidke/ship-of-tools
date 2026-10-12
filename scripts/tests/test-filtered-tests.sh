#!/usr/bin/env bash
# Real libtest controls for the filtered-group verdict; all witnesses belong to temporary roots.
set -euo pipefail
if [[ $# != 1 ]]; then
    echo 'usage: test-filtered-tests.sh ABSOLUTE_FIXTURE' >&2
    exit 2
fi
python3 - "$1" "$(dirname -- "${BASH_SOURCE[0]}")/../.." <<'PY'
from dataclasses import dataclass
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
# Wrappers around the fixture: one reports success whatever libtest returns, one starts the output with a NUL byte.
HIDE_STATUS = ('bash', '-c', '"$@"; exit 0', 'hide-status')
NUL_FIRST = ('bash', '-c', 'printf "\\0"; exec "$@"', 'nul-first')
USAGE = 'expected positive count, log, -- and command'
FAILED = 'command or capture failed'
WANT = 'want exactly one summary line, starting "test result: ok. 1 passed; 0 failed; 0 ignored;"'


@dataclass
class Case:
    name: str
    selection: list  # the fixture's arguments before the flags; None runs the runner with no command
    summaries: list  # every log line starting "test result: ", in order, each by its prefix after that text
    ran: list  # the fixture bodies that wrote their witness
    status: int  # the runner's exit status
    said: list  # every line the runner prints itself, in order, each by its prefix after "filtered-tests: "
    count: int = 1
    exact: bool = True
    log: str = 'fresh'  # fresh, new-folder, stale (holds an earlier summary) or directory
    wrap: tuple = ()
    panic: bool = False
    separator: bool = True


def check(ok, detail):
    if not ok:
        raise AssertionError(detail)


def starts(lines, prefixes):
    return len(lines) == len(prefixes) and all(line.startswith(prefix) for line, prefix in zip(lines, prefixes))


def run(case):
    """The runner in a fresh root: exit status, output, libtest log and the bodies that ran, saved as evidence."""
    evidence.clear()
    with tempfile.TemporaryDirectory(prefix=f'filtered-count-{case.name}.', dir=output) as temporary:
        root = Path(temporary).resolve()
        (root / 'iso-sh-fixture-request').touch()
        if case.panic:
            (root / 'iso-sh-panic-request').touch()
        log = root if case.log == 'directory' else root / ('new' if case.log == 'new-folder' else '.') / 'libtest.log'
        if case.log == 'stale':
            log.write_text('test result: ok. 9 passed; 0 failed; 0 ignored; left by an earlier run\n')
        argv = ['bash', str(runner), str(case.count), str(log)] + (['--'] if case.separator else [])
        if case.selection is not None:
            argv += [*case.wrap, str(fixture), *case.selection] + (['--exact'] if case.exact else []) + flags
        try:
            result = subprocess.run(argv, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60)
            code, child = result.returncode, result.stdout
        except subprocess.TimeoutExpired as error:
            code, child = 'timeout after 60 seconds', error.output or b''
        ran = sorted(path.name.removeprefix('witness-') for path in root.glob('witness-*'))
        text = child.decode('utf-8', 'replace')
        evidence['status'] = json.dumps({'exit': code, 'witnesses': ran})
        evidence['output'] = text.replace(str(root), '<temporary-root>').replace(str(fixture), '<fixture>')
        evidence['libtest'] = log.read_text() if log.is_file() else ''
    for suffix, saved in evidence.items():
        (output / f'filtered-count-{case.name}.{suffix}').write_text(saved + '\n')
    check(isinstance(code, int), 'child wait timed out after 60 seconds')
    return code, text, evidence['libtest'], ran


def found(summary):
    return f'found: test result: {summary}'


cases = [
    Case('absent', ['absent'], ['ok. 0 passed; 0 failed; 0 ignored;'], [], 1,
         [WANT, found('ok. 0 passed; 0 failed; 0 ignored;')]),
    Case('ordinary', ['ordinary'], ['ok. 1 passed; 0 failed; 0 ignored;'], ['ordinary'], 0, ['expected=1 ok=true'],
         log='new-folder'),
    Case('group', ['near'], ['ok. 2 passed; 0 failed; 0 ignored;'], ['near', 'qualified-near'], 0,
         ['expected=2 ok=true'], count=2, exact=False),
    Case('wrong-count', ['near'], ['ok. 2 passed; 0 failed; 0 ignored;'], ['near', 'qualified-near'], 1,
         [WANT, found('ok. 2 passed; 0 failed; 0 ignored;')], exact=False),
    Case('ignored', ['qualified::ignored_positive'], ['ok. 0 passed; 0 failed; 1 ignored;'], [], 1,
         [WANT, found('ok. 0 passed; 0 failed; 1 ignored;')]),
    Case('ignored-beside', ['ordinary', 'qualified::ignored_positive'], ['ok. 1 passed; 0 failed; 1 ignored;'],
         ['ordinary'], 1, [WANT, found('ok. 1 passed; 0 failed; 1 ignored;')]),
    Case('failed', ['ordinary'], ['FAILED. 0 passed; 1 failed; 0 ignored;'], ['ordinary'], 1, [FAILED], panic=True),
    Case('hidden-failure', ['ordinary', 'positive'], ['FAILED. 1 passed; 1 failed; 0 ignored;'],
         ['ordinary', 'positive'], 1, [WANT, found('FAILED. 1 passed; 1 failed; 0 ignored;')], wrap=HIDE_STATUS,
         panic=True),
    # The first summary line is the one the misleading body prints into its captured output.
    Case('misleading', ['misleading', 'ordinary'],
         ['ok. 1 passed; 0 failed; 0 ignored;', 'ok. 2 passed; 0 failed; 0 ignored;'], ['misleading', 'ordinary'], 1,
         [WANT, found('ok. 1 passed; 0 failed; 0 ignored;'), found('ok. 2 passed; 0 failed; 0 ignored;')]),
    Case('no-summary', ['ordinary', '--list'], [], [], 1, [WANT, 'found: no summary line']),
    Case('nul-byte', ['ordinary'], ['ok. 1 passed; 0 failed; 0 ignored;'], ['ordinary'], 0, ['expected=1 ok=true'],
         wrap=NUL_FIRST),
    Case('stale-log', ['ordinary'], ['ok. 1 passed; 0 failed; 0 ignored;'], ['ordinary'], 0, ['expected=1 ok=true'],
         log='stale'),
    Case('capture-error', ['ordinary'], [], ['ordinary'], 1, [FAILED], log='directory'),
    Case('invalid-count', ['ordinary'], [], [], 2, [USAGE], count=0),
    Case('no-separator', ['ordinary'], [], [], 2, [USAGE], separator=False),
    Case('no-command', None, [], [], 2, [USAGE]),
]

name = 'setup'
try:
    for case in cases:
        name = case.name
        code, text, raw, ran = run(case)
        summaries = [line for line in raw.splitlines() if line.startswith('test result: ')]
        said = [line.removeprefix('filtered-tests: ') for line in text.splitlines()
                if line.startswith('filtered-tests: ')]
        check(starts(summaries, ['test result: ' + summary for summary in case.summaries]),
              f'summary lines {summaries}, want {case.summaries}')
        check(ran == case.ran, f'bodies that ran: {ran}, want {case.ran}')
        check(code == case.status, f'runner exit {code}, want {case.status}')
        check(starts(said, case.said), f'runner said {said}, want {case.said}')
        check(not case.panic or 'requested fixture panic' in raw, 'missing the failed body cause')
        print(f'filtered-count proof={name} ok=true', flush=True)
    print(f'filtered-count completed_cases={len(cases)} ok=true', flush=True)
except (AssertionError, OSError, UnicodeError) as error:
    print(f'filtered-count FAIL case={name}: {error}', file=sys.stderr)
    for suffix, saved in evidence.items():
        print(f'--- filtered-count-{name}.{suffix} ---\n{saved}', file=sys.stderr)
    sys.stderr.flush()
    sys.exit(1)
PY
