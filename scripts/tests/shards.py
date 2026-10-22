#!/usr/bin/env python3
"""shards.py: the workspace's test shards, from one table, .github/test-shards.txt.

  shards.py plan landing|gate   the jobs of one workflow, as a JSON list of {os, shard} (a job matrix reads it); a shard
                                is a name, or names joined by + for the shards one job runs in turn (`fold` lines)
  shards.py build JOB           link the test executables of a job's rest shards (outside the timed step)
  shards.py run JOB             run one job's shards from the workspace folder (rust/)
  shards.py check               fail, naming it, a test that runs in no shard or in two
  shards.py gated TARGET        read test names on stdin, print those no gate-* shard runs (rc-gate.sh, which leaves the
                                release gate's tests to release-gate.yml)
  shards.py gate-whole TARGET   exit 0 when a gate-* shard runs the whole target
  shards.py parts [all]         the landing shards' rows (with `all`, the gate's too) that cut a target by filters, as
                                shard, kind, target, filters separated by tabs

A table row is `shard kind target [libtest filters]`: a target is a test executable (kind bin, test, example or bench).
A row with no filters runs the target whole; a row with filters runs the tests they select. Positional filters are
alternatives, as in libtest; `--skip X` removes. A target's rows together must select each of its tests exactly once.
A line `fold SYSTEM a+b c` lists the jobs of linux, windows or macos: shards joined by + run in turn in one job, and a
shard in no fold is a job of its own (a system's seconds differ: macOS and Windows run the groups of Linux's slow
binaries in a fraction of the time, and macOS runs at most five jobs at once).
A shard named gate-* runs in release-gate.yml, every other in rust.yml, and `rest-0` and `rest-1` (never named) in
rust.yml run every test executable the table does not name, alternate ones each, rest-0 then the doc tests.
"""
import json
import os
import pathlib
import subprocess
import sys

TABLE = pathlib.Path(__file__).resolve().parents[2] / '.github' / 'test-shards.txt'
KINDS = ('bin', 'test', 'example', 'bench')
ROW_KINDS = KINDS + ('lib',)  # a row may also name one library's unit tests, by the library's target name
REST = ('rest-0', 'rest-1', 'rest-2')
OSES = {'linux': 'ubuntu-latest', 'windows': 'windows-latest', 'macos': 'macos-latest'}


def rows():
    out = []
    for line in TABLE.read_text().splitlines():
        line = line.split('#', 1)[0].strip()
        if line and not line.startswith('fold '):
            shard, kind, target, *filters = line.split()
            if kind not in ROW_KINDS:
                sys.exit(f'{TABLE}: {line!r}: kind {kind} is not one of {ROW_KINDS}')
            out.append((shard, kind, target, filters))
    return out


def folds(system):
    """The jobs of one system: each `fold SYSTEM a+b c` line names the shards one job runs in turn."""
    out = []
    for line in TABLE.read_text().splitlines():
        words = line.split('#', 1)[0].split()
        if words[:2] == ['fold', system]:
            out += words[2:]
    return out


def jobs(system):
    """Every landing shard once: in its fold, or a job of its own."""
    names = [n for n in shard_names(rows()) if not n.startswith('gate-')] + list(REST)
    named = [n for job in folds(system) for n in job.split('+')]
    if len(named) != len(set(named)) or set(named) - set(names):
        sys.exit(f'fold {system}: a shard twice or not in the table: {sorted(set(named) ^ set(names) & set(named))}')
    return folds(system) + [n for n in names if n not in named]


def shard_names(table):
    return list(dict.fromkeys(row[0] for row in table))


def selects(filters, name):
    positive, skip, it = [], [], iter(filters)
    for word in it:
        if word == '--skip':
            skip.append(next(it))
        else:
            positive.append(word)
    return (not positive or any(p in name for p in positive)) and not any(s in name for s in skip)


def built_tests():
    out = subprocess.run(['cargo', 'test', '--workspace', '--locked', '--no-run', '--message-format=json-render-diagnostics'],
                         stdout=subprocess.PIPE, text=True, check=True).stdout
    built = {}
    for line in out.splitlines():
        message = json.loads(line) if line.startswith('{') else {}
        if message.get('reason') == 'compiler-artifact' and message['profile']['test'] and message['executable']:
            built[(message['target']['kind'][0], message['target']['name'])] = message
    return built


def declared_tests():
    """(kind, name) of every test executable `cargo test --workspace` builds, from the manifests: no build."""
    out = subprocess.run(['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked'],
                         stdout=subprocess.PIPE, text=True, check=True).stdout
    found = set()
    for package in json.loads(out)['packages']:
        for target in package['targets']:
            if target['test'] and not target.get('required-features'):
                found.add((target['kind'][0], target['name']))
    return found


def cargo_test(shard, selection, libtest=()):
    command = ['cargo', 'test', '--workspace', '--locked', '--no-fail-fast', *selection, '--', *libtest, '--nocapture']
    print('shard ' + shard + ': ' + ' '.join(command), flush=True)
    return subprocess.run(command).returncode != 0


def cargo_lib(shard, target, libtest=()):
    command = ['cargo', 'test', '-p', target.replace('_', '-'), '--lib', '--locked', '--no-fail-fast', '--', *libtest, '--nocapture']
    print('shard ' + shard + ': ' + ' '.join(command), flush=True)
    return subprocess.run(command).returncode != 0


def selection_of(shard, table):
    """The cargo arguments of a rest shard: its slice of the unnamed targets, the libraries in rest-0."""
    declared = declared_tests()
    named = {(kind, target) for _, kind, target, _ in table}
    if named - declared:
        sys.exit('the table names targets that build no test executable: ' + repr(sorted(named - declared)))
    everything = sorted(declared - named)
    rest = [t for t in everything if t[0] in KINDS][REST.index(shard)::len(REST)]
    libs = shard == REST[0] and any(kind not in KINDS for kind, _ in everything)
    apart = [arg for kind, name in sorted(named) if kind == 'lib' for arg in ('--exclude', name.replace('_', '-'))]
    return (['--lib'] + apart if libs else []) + [arg for kind, name in rest for arg in ('--' + kind, name)]


def build(job):
    """Link what the job's rest shards run, outside the timed test step."""
    table = rows()
    for shard in job.split('+'):
        if shard in REST:
            subprocess.run(['cargo', 'test', '--workspace', '--locked', '--no-run', *selection_of(shard, table)], check=True)


def run(job):
    table = rows()
    results = []
    for shard in job.split('+'):
        results += run_shard(shard, table)
    sys.exit(1 if any(results) else 0)


def run_shard(shard, table):
    if shard in REST:
        selection = selection_of(shard, table)
        print(f'shard {shard}: {len(selection)} arguments', flush=True)
        return [cargo_test(shard, selection)] + ([cargo_test(shard, ['--doc'])] if shard == REST[0] else [])
    mine = [row for row in table if row[0] == shard]
    if not mine:
        sys.exit(f'no shard {shard} in {TABLE}')
    return [cargo_test(shard, ['--' + kind, target], filters) if kind != 'lib' else cargo_lib(shard, target, filters)
            for _, kind, target, filters in mine]


def listed(message, extra):
    exe = message['executable']
    manifest = pathlib.Path(message['manifest_path']).parent
    out = subprocess.run([exe, '--list', '--format', 'terse', *extra], cwd=manifest, text=True, stdout=subprocess.PIPE,
                         env={**os.environ, 'CARGO_MANIFEST_DIR': str(manifest)}, check=True).stdout
    return {line[:-6] for line in out.splitlines() if line.endswith(': test')}


def check():
    table = rows()
    if set(REST) & set(shard_names(table)):
        sys.exit('the table names a rest shard; rest runs only what the table does not name')
    built = built_tests()
    problems = []
    if declared_tests() != {(k, n) for k, n in built}:
        problems.append('the manifests declare other test executables than cargo built: '
                        + repr(sorted(declared_tests() ^ {(k, n) for k, n in built})))
    for target in sorted({(kind, target) for _, kind, target, _ in table}):
        if target not in built:
            problems.append(f'{target[1]}: the table names a target that builds no test executable')
            continue
        mine = [(shard, filters) for shard, kind, name, filters in table if (kind, name) == target]
        for test in sorted(listed(built[target], []) - listed(built[target], ['--ignored'])):
            where = [shard for shard, filters in mine if selects(filters, test)]
            if len(where) != 1:
                problems.append(f'{target[1]}: {test} runs ' + ('nowhere' if not where else 'in ' + ' and '.join(where)))
    if problems:
        more = [f'... and {len(problems) - 20} more'] if len(problems) > 20 else []
        sys.exit(f'test shard coverage failed, {len(problems)} tests:\n  ' + '\n  '.join(problems[:20] + more))
    print(f'shard coverage: {len(built)} test executables, every test of the {len(set((k, t) for _, k, t, _ in table))} '
          f'named targets runs in exactly one shard')


def gated(target):
    gate = [filters for shard, _, name, filters in rows() if shard.startswith('gate-') and name == target]
    for line in sys.stdin.read().splitlines():
        if not any(selects(filters, line) for filters in gate):
            print(line)


def gate_whole(target):
    sys.exit(0 if any(shard.startswith('gate-') and name == target and not filters for shard, _, name, filters in rows()) else 1)


def parts(everything):
    seen = {}
    for shard, kind, target, filters in rows():
        seen[shard] = seen.get(shard, 0) + 1
        if filters and (everything or not shard.startswith('gate-')):
            print('\t'.join([f'{shard}-{seen[shard]}', kind, target, ' '.join(filters)]))


def main():
    command, *args = sys.argv[1:] or ['']
    if command == 'plan' and args in (['landing'], ['gate']):
        gate = [n for n in shard_names(rows()) if n.startswith('gate-')]
        if args == ['gate']:
            plan = [{'os': os_, 'shard': n} for os_ in OSES.values() for n in gate]
        else:
            plan = [{'os': os_, 'shard': job} for system, os_ in OSES.items() for job in jobs(system)]
        print(json.dumps(plan))
    elif command == 'build' and len(args) == 1:
        build(args[0])
    elif command == 'run' and len(args) == 1:
        run(args[0])
    elif command == 'check' and not args:
        check()
    elif command == 'gated' and len(args) == 1:
        gated(args[0])
    elif command == 'gate-whole' and len(args) == 1:
        gate_whole(args[0])
    elif command == 'parts' and args in ([], ['all']):
        parts(bool(args))
    else:
        sys.exit(__doc__)


main()
