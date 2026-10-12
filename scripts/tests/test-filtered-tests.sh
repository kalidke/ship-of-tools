#!/usr/bin/env bash
# Real libtest controls for the filtered-group verdict, and its mutation table; all witnesses belong to temporary roots.
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
import re
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
# Wrappers around the fixture: success whatever libtest returns, failure after libtest succeeds, a NUL byte before
# libtest's output, and a line holding "test result: " away from the line's start.
HIDE_STATUS = ('bash', '-c', '"$@"; exit 0', 'hide-status')
FAIL_AFTER = ('bash', '-c', '"$@"; exit 3', 'fail-after')
NUL_FIRST = ('bash', '-c', 'printf "\\0"; exec "$@"', 'nul-first')
MID_LINE = ('bash', '-c', 'echo "note: test result: mid-line"; exec "$@"', 'mid-line')
USAGE = 'expected positive count, log, -- and command'
FAILED = 'command or capture failed'


def want(count):
    return f'want exactly one summary line, starting "test result: ok. {count} passed; 0 failed; 0 ignored;"'


def found(summary):
    return f'found: test result: {summary}'


@dataclass
class Case:
    name: str
    selection: list  # the fixture's arguments before the flags; None runs no fixture
    summaries: list  # every log line starting "test result: ", in order, each by its prefix after that text
    ran: list  # the fixture bodies that wrote their witness
    status: int  # the runner's exit status
    said: list  # every line the runner prints itself, in order, each by its prefix after "filtered-tests: "
    count: object = 1  # the runner's first argument; None runs the runner with no arguments
    exact: bool = True
    log: str = 'fresh'  # fresh, new-folder, under-file (its folder is a file), stale (holds a summary) or directory
    wrap: tuple = ()
    panic: bool = False
    separator: bool = True
    command: tuple = ()  # a command run instead of the fixture


def check(ok, detail):
    if not ok:
        raise AssertionError(detail)


def starts(lines, prefixes):
    return len(lines) == len(prefixes) and all(line.startswith(prefix) for line, prefix in zip(lines, prefixes))


def run(case, judged, save):
    """The runner in a fresh root: exit status, output, libtest log and the bodies that ran, kept as evidence."""
    evidence.clear()
    with tempfile.TemporaryDirectory(prefix=f'filtered-count-{case.name}.', dir=output) as temporary:
        root = Path(temporary).resolve()
        (root / 'iso-sh-fixture-request').touch()
        if case.panic:
            (root / 'iso-sh-panic-request').touch()
        folder = root / {'new-folder': 'new', 'under-file': 'file'}.get(case.log, '.')
        log = root if case.log == 'directory' else folder / 'libtest.log'
        if case.log == 'under-file':
            folder.touch()  # mkdir -p fails on a file, whoever runs it
        if case.log == 'stale':
            log.write_text('test result: ok. 9 passed; 0 failed; 0 ignored; left by an earlier run\n')
        argv = ['bash', str(judged)]
        if case.count is not None:
            argv += [str(case.count), str(log)] + (['--'] if case.separator else []) + [*case.command]
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
    if save:
        for suffix, saved in evidence.items():
            (output / f'filtered-count-{case.name}.{suffix}').write_text(saved + '\n')
    check(isinstance(code, int), 'child wait timed out after 60 seconds')
    return code, text, evidence['libtest'], ran


def holds(case, judged, save=False):
    """Raises unless the runner at `judged` gives this case's exact outcome."""
    code, text, raw, ran = run(case, judged, save)
    summaries = [line for line in raw.splitlines() if line.startswith('test result: ')]
    said = [line.removeprefix('filtered-tests: ') for line in text.splitlines() if line.startswith('filtered-tests: ')]
    check(starts(summaries, ['test result: ' + summary for summary in case.summaries]),
          f'summary lines {summaries}, want {case.summaries}')
    check(ran == case.ran, f'bodies that ran: {ran}, want {case.ran}')
    check(code == case.status, f'runner exit {code}, want {case.status}')
    check(starts(said, case.said), f'runner said {said}, want {case.said}')
    check(not case.panic or 'requested fixture panic' in raw, 'missing the failed body cause')


def names(entries):
    """The table's entries by name: a row's is its first field, a fragment's is its text."""
    return [entry if isinstance(entry, str) else entry[0] for entry in entries]


ONE = 'ok. 1 passed; 0 failed; 0 ignored;'
cases = [
    Case('absent', ['absent'], ['ok. 0 passed; 0 failed; 0 ignored;'], [], 1,
         [want(1), found('ok. 0 passed; 0 failed; 0 ignored;')]),
    Case('ordinary', ['ordinary'], [ONE], ['ordinary'], 0, ['expected=1 ok=true'], log='new-folder'),
    Case('group', ['near'], ['ok. 2 passed; 0 failed; 0 ignored;'], ['near', 'qualified-near'], 0,
         ['expected=2 ok=true'], count=2, exact=False),
    Case('wrong-count', ['near'], ['ok. 2 passed; 0 failed; 0 ignored;'], ['near', 'qualified-near'], 1,
         [want(3), found('ok. 2 passed; 0 failed; 0 ignored;')], count=3, exact=False),
    # Also guards a runner that counts "running N tests", or passed plus ignored: no single edit of this one does.
    Case('ignored', ['qualified::ignored_positive'], ['ok. 0 passed; 0 failed; 1 ignored;'], [], 1,
         [want(1), found('ok. 0 passed; 0 failed; 1 ignored;')]),
    Case('ignored-beside', ['ordinary', 'qualified::ignored_positive'], ['ok. 1 passed; 0 failed; 1 ignored;'],
         ['ordinary'], 1, [want(1), found('ok. 1 passed; 0 failed; 1 ignored;')]),
    Case('fail-after-ok', ['ordinary'], [ONE], ['ordinary'], 1, [FAILED], wrap=FAIL_AFTER),
    Case('hidden-failure', ['ordinary', 'positive'], ['FAILED. 1 passed; 1 failed; 0 ignored;'],
         ['ordinary', 'positive'], 1, [want(1), found('FAILED. 1 passed; 1 failed; 0 ignored;')], wrap=HIDE_STATUS,
         panic=True),
    # The first summary line is the one the misleading body prints into its captured output.
    Case('misleading', ['misleading', 'ordinary'], [ONE, 'ok. 2 passed; 0 failed; 0 ignored;'],
         ['misleading', 'ordinary'], 1, [want(1), found(ONE), found('ok. 2 passed; 0 failed; 0 ignored;')]),
    Case('mid-line', ['ordinary'], [ONE], ['ordinary'], 0, ['expected=1 ok=true'], wrap=MID_LINE),
    Case('no-summary', ['ordinary', '--list'], [], [], 1, [want(1), 'found: no summary line']),
    Case('nul-byte', ['ordinary'], [ONE], ['ordinary'], 0, ['expected=1 ok=true'], wrap=NUL_FIRST),
    Case('stale-log', ['ordinary'], [ONE], ['ordinary'], 0, ['expected=1 ok=true'], log='stale'),
    Case('capture-error', ['ordinary'], [], ['ordinary'], 1, [FAILED], log='directory'),
    Case('folder-error', ['ordinary'], [], [], 1, [], log='under-file'),
    Case('invalid-count', ['ordinary'], [], [], 2, [USAGE], count=0),
    Case('count-suffix', ['ordinary'], [], [], 2, [USAGE], count='1x'),
    Case('count-prefix', ['ordinary'], [], [], 2, [USAGE], count='x1'),
    Case('no-separator', ['ordinary'], [], [], 2, [USAGE], separator=False),
    Case('no-command', None, [], [], 2, [USAGE]),
    Case('bare-command', None, [], [], 1, [want(1), 'found: no summary line'], command=('true',)),
    Case('no-arguments', None, [], [], 2, [USAGE], count=None),
]

# One table in the runner's order. A string is a fragment of the runner's code that no row's edit text holds; a row,
# placed where its edit text starts (rows that start together, in any order), removes one check by one exact edit, and
# the case it names is the first to fail on that edit. What the table guarantees, and what review must still catch,
# is the run-filtered-tests.sh rule in scripts/tests/CLAUDE.md.
# Not checks, so no rows of their own, though inside rows' texts: 2>&1, as libtest prints its summary on stdout; set -u,
# as line 3 counts the arguments before it reads one; each line's stream and tee's copy to stdout, which only a reader
# sees.
WANT_LINE = ('    printf \'filtered-tests: want exactly one summary line, starting "test result: ok. %s passed; '
             '0 failed; 0 ignored;"\\n\' \\\n        "$filtered_count" >&2\n')
FOUND_LINE = '    sed \'s/^/filtered-tests: found: /\' <<< "${filtered_summary:-no summary line}" >&2\n'
RECEIPT = 'printf \'filtered-tests: expected=%s ok=true\\n\' "$filtered_count"\n'
MUTANTS = [
    ('pipefail', 'set -euo pipefail', 'set -eu', 'fail-after-ok'),
    ('errexit', 'set -euo pipefail', 'set -uo pipefail', 'folder-error'),
    'if [[',
    ('argument-order', '$# -lt 4 || ! $1 =~ ^[1-9][0-9]*$ || ', '! $1 =~ ^[1-9][0-9]*$ || $# -lt 4 || ',
     'no-arguments'),
    ('arguments', '$# -lt 4 || ', '', 'no-command'),
    ('argument-boundary', '$# -lt 4', '$# -le 4', 'bare-command'),
    ('positive-count', '! $1 =~ ^[1-9][0-9]*$ || ', '', 'invalid-count'),
    ('count-start', '^[1-9][0-9]*$', '[1-9][0-9]*$', 'count-prefix'),
    ('count-end', '^[1-9][0-9]*$', '^[1-9][0-9]*', 'count-suffix'),
    ('separator', ' || $3 != --', '', 'no-separator'),
    ']]; then',
    ('usage-line', "    echo 'filtered-tests: expected positive count, log, -- and command' >&2\n", '',
     'invalid-count'),
    ('usage-exit', '    exit 2\n', '    exit 1\n', 'invalid-count'),
    'fi', 'filtered_count=$1', 'filtered_log=$2', 'shift 3',
    ('log-folder', 'mkdir -p -- "$(dirname -- "$filtered_log")"\n', '', 'ordinary'),
    ('capture-status', 'if ! "$@" 2>&1 | tee "$filtered_log"; then',
     'if ! { "$@" 2>&1 | tee "$filtered_log"; true; }; then', 'fail-after-ok'),
    ('tee-status', '| tee "$filtered_log"; then', '| { tee "$filtered_log" || true; }; then', 'capture-error'),
    ('fresh-log', '| tee "$filtered_log"', '| tee -a "$filtered_log"', 'stale-log'),
    ('capture-line', "    echo 'filtered-tests: command or capture failed' >&2\n", '', 'fail-after-ok'),
    ('capture-exit', '    exit 1\nfi\n#', '    exit 0\nfi\n#', 'fail-after-ok'),
    'filtered_summary=$(',
    ('line-start', "grep -a '^test result: '", "grep -a 'test result: '", 'mid-line'),
    ('nul-byte', 'grep -a ', 'grep ', 'nul-byte'),
    '"$filtered_log"',
    ('no-match', ' || true)', ')', 'no-summary'),
    ('empty-summary', 'if [[ $filtered_summary ==', 'if [[ -n $filtered_summary ]] && [[ $filtered_summary ==',
     'no-summary'),
    ('one-summary', "$filtered_summary == *$'\\n'* ||\n    ", '', 'misleading'),
    '$filtered_summary !=',
    ('no-failure', '"test result: ok. $filtered_count passed; 0 failed;',
     '"test result: "*" $filtered_count passed; "*" failed;', 'hidden-failure'),
    ('pass-count', 'ok. $filtered_count passed;', 'ok. "*" passed;', 'absent'),
    ('zero-ignored', ' 0 failed; 0 ignored;"*', ' 0 failed;"*', 'ignored-beside'),
    ']]; then',
    ('want-line', WANT_LINE, '', 'absent'),
    ('want-count', '        "$filtered_count" >&2\n', '        1 >&2\n', 'wrong-count'),
    ('found-lines', FOUND_LINE, '', 'absent'),
    ('empty-report', '${filtered_summary:-no summary line}', '${filtered_summary}', 'no-summary'),
    ('reject-exit', '    exit 1\nfi\nprintf', '    exit 0\nfi\nprintf', 'absent'),
    ('found-on-accept', RECEIPT, FOUND_LINE.strip() + '\n' + RECEIPT, 'ordinary'),
    ('receipt', RECEIPT, '', 'ordinary'),
    ('receipt-count', 'ok=true\\n\' "$filtered_count"', 'ok=true\\n\' 1', 'group'),
]
ROWS = [entry for entry in MUTANTS if isinstance(entry, tuple)]

name = 'setup'
try:
    for case in cases:
        name = case.name
        holds(case, runner, save=True)
        print(f'filtered-count proof={name} ok=true', flush=True)
    print(f'filtered-count completed_cases={len(cases)} ok=true', flush=True)
    evidence.clear()
    source = runner.read_bytes().decode()  # as bash reads it: a carriage return is a character, not a line break
    name = 'order'
    check('\0' not in source, 'the runner holds a NUL byte, which bash drops')
    code = list(source)  # the runner with its comment lines and every row's edit text blanked, its newlines kept
    for comment in re.finditer('(?m)^[ \t]*#.*', source):
        code[comment.start():comment.end()] = '\0' * len(comment.group())
    found = []  # (offset, entry): each row where its edit text starts, each fragment of code left where it starts
    for row in ROWS:
        name = f'mutant {row[0]}'
        check(source.count(row[1]) == 1, f'its edit text occurs {source.count(row[1])} times in the runner')
        start = source.index(row[1])
        code[start:start + len(row[1])] = [c if c == '\n' else '\0' for c in row[1]]
        found.append((start, row))
    found += [(piece.start(), piece.group().strip(' \t')) for piece in re.finditer('[^\0\n]+', ''.join(code))
              if piece.group().strip(' \t')]
    name = 'order'
    order = [entry for _, entry in sorted(found, key=lambda item: item[0])]
    check(order == MUTANTS, f'the runner reads {names(order)}, the table {names(MUTANTS)}')
    print(f'filtered-count order={len(MUTANTS)} ok=true', flush=True)
    with tempfile.TemporaryDirectory(prefix='filtered-count-mutants.', dir=output) as temporary:
        for mutant, old, new, killer in ROWS:
            evidence.clear()
            name = f'mutant {mutant}'
            judged = Path(temporary) / f'{mutant}.sh'
            judged.write_text(source.replace(old, new))
            first = None
            for case in cases:
                try:
                    holds(case, judged)
                except AssertionError:
                    first = case.name
                    break
            check(first == killer, f'first failing case {first}, want {killer}')
            print(f'filtered-count mutant={mutant} killed_by={killer} ok=true', flush=True)
    print(f'filtered-count mutants={len(ROWS)} ok=true', flush=True)
except (AssertionError, OSError, UnicodeError) as error:
    print(f'filtered-count FAIL case={name}: {error}', file=sys.stderr)
    for suffix, saved in evidence.items():
        print(f'--- filtered-count-{name}.{suffix} ---\n{saved}', file=sys.stderr)
    sys.stderr.flush()
    sys.exit(1)
PY
