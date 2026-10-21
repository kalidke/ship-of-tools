#!/usr/bin/env bash
# Finite behavior proofs of the selected-test verdict and its shell callers, using an already-built
# real libtest fixture and scratch-owned body witnesses. No comm tool, daemon, peer host or full
# candidate gate is run.
# The proof binds its shell scratch root to a validated absolute directory before installing cleanup; behavior controls observe the driver and cleanup paths under relative TMPDIR.
# Child output is decoded as strict UTF-8; unreadable output fails its named case.
set -u
[ "$#" -eq 2 ] || { echo 'usage: test-test-body.sh --portable|--all ABSOLUTE_FIXTURE' >&2; exit 2; }
case ${1:-} in --portable|--all) mode=${1#--} ;; *) echo 'usage: test-test-body.sh --portable|--all ABSOLUTE_FIXTURE' >&2; exit 2 ;; esac
[ -f "$2" ] || { echo 'compiled fixture required' >&2; exit 2; }
proof_bash=${BASH:-}
case "$proof_bash" in /*|[A-Za-z]:[\\/]*) ;; *) echo 'selected-body proof: absolute native Bash required' >&2; exit 2 ;; esac
if ! [ -f "$proof_bash" ] || ! [ -x "$proof_bash" ] ||
   ! proof_bash_version=$("$proof_bash" -c 'printf "%s\n" "${BASH_VERSION:-}"') || [ -z "$proof_bash_version" ]; then
    echo 'selected-body proof: executable native Bash required' >&2
    exit 2
fi
base=${TMPDIR:-/tmp}
raw_root=$(mktemp -d "$base/iso-sh-proof.XXXXXX") || { echo 'selected-body proof: scratch creation failed' >&2; exit 2; }
if ! root=$(python3 - "$raw_root" <<'ROOT'
import os
from pathlib import Path
import sys
try:
    raw = sys.argv[1]
    if not raw:
        raise ValueError('empty scratch path')
    created = Path(raw)
    absolute = created.resolve(strict=True)
    if not absolute.is_dir() or not os.path.isabs(str(absolute)) or absolute.parent == absolute:
        raise ValueError('scratch path must be a proper absolute directory')
    if not created.samefile(absolute):
        raise ValueError('scratch directory identity changed')
    print(absolute.as_posix())
except (OSError, ValueError):
    print('selected-body proof: invalid scratch directory', file=sys.stderr)
    sys.exit(2)
ROOT
); then
    echo 'selected-body proof: scratch normalization failed' >&2
    exit 2
fi
trap 'rm -rf -- "${root:?}"' EXIT
python3 -X utf8 - "$mode" "$2" "$(dirname "${BASH_SOURCE[0]}")/../.." "$root" "$proof_bash" <<'PY'
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
mode, fixture, checkout, temp, native_bash = sys.argv[1:]
assert os.path.isabs(fixture), 'fixture path must be absolute'
fixture = str(Path(fixture).resolve())
repo, root = Path(checkout).resolve(), Path(temp).resolve()
env = {k: v for k, v in os.environ.items() if not k.startswith('SOT_')}
for name in ('HOME', 'USERPROFILE', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'XDG_CONFIG_HOME', 'XDG_STATE_HOME'):
    env[name] = str(root / 'home')
(root / 'home').mkdir()
helper = repo / 'scripts/tests/lib-test-body.sh'
gate = (repo / 'scripts/tests/rc-gate.sh').read_text(encoding='utf-8', errors='strict')
failures = []
serial = 0
q = lambda v: shlex.quote(str(v))
pretty = ['--format', 'pretty', '--color', 'never', '--show-output', '--test-threads=1']

def fresh(label):
    global serial
    serial += 1
    p = root / f'{serial} {label}'
    p.mkdir(); (p / 'iso-sh-fixture-request').touch(); (p / 'inbox').mkdir()
    return p

def run(argv, p, extra=None):
    actual = dict(env); actual.update(extra or {})
    try:
        r = subprocess.run(argv, cwd=p, env=actual, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=45)
        text = r.stdout.decode('utf-8', 'strict').replace('\r\n', '\n').replace('\r', '\n')
        return subprocess.CompletedProcess(r.args, r.returncode, text, None)
    except UnicodeDecodeError as error:
        raise AssertionError(f'child output is not UTF-8 ({argv[0]}): {type(error).__name__}: {error}')

def shell(body, p, extra=None):
    return run([native_bash, '-c', body], p, extra)

def check(ok, detail):
    if not ok: raise AssertionError(detail)

def case(name, body):
    try:
        body(); print(f'PASS case {name}', flush=True)
    except (AssertionError, subprocess.TimeoutExpired, UnicodeError) as error:
        if isinstance(error, UnicodeError):
            error = f'{type(error).__name__}: {error}'
        failures.append(name); print(f'FAIL case {name}: {error}', flush=True)

def unexpected_arguments():
    p = fresh('unexpected arguments')
    r = bootstrap_run(p, 'absolute', arguments=['--portable', fixture, 'unexpected-extra-argument'])
    print(f'unexpected argument: exit {r.returncode}; output {r.stdout.strip()!r}', flush=True)
    check(r.returncode != 0 and len(r.stdout.splitlines()) == 1,
          'unexpected extra argument accepted or rejection was not one line')

def bootstrap_run(p, setup, arguments=None):
    # Load the actual bootstrap; replace only its driver and removal execution ports.
    text = (repo / 'scripts/tests/test-test-body.sh').read_text(encoding='utf-8', errors='strict')
    bootstrap = text[:text.index('python3 -X utf8 - "$mode"')]
    record = p / 'ports'; record.mkdir()
    tmp = p / ('relative temp spaces' if setup == 'relative' else 'absolute temp spaces')
    tmp.mkdir(); (p / 'sentinel').write_text('outside inner root')
    ports = (f'proof_record={q(record)}\n'
             'rm() { printf "%s\\n" "${@: -1}" > "$proof_record/cleanup"; }\n'
             'proof_driver() { printf "%s\\n" "$4" > "$proof_record/driver"; }\n')
    if setup in ('empty', 'invalid', 'volume-root'):
        value = {'empty': '', 'invalid': str(p / 'not-created'),
                 'volume-root': Path(p.anchor).as_posix()}[setup]
        ports += f'mktemp() {{ printf "%s" {q(value)}; }}\n'
    elif setup == 'mktemp-failure':
        ports += 'mktemp() { return 17; }\n'
    elif setup == 'normalization-failure':
        ports += 'python3() { return 17; }\n'
    script = p / 'bootstrap.sh'
    script.write_text(ports + bootstrap +
        'proof_driver "$mode" "$2" "$(dirname "${BASH_SOURCE[0]}")/../.." "$root"\n')
    r = run([native_bash, str(script)] + (arguments or ['--portable', fixture]), p,
            {'TMPDIR': tmp.name if setup == 'relative' else tmp.as_posix()})
    (p / 'bootstrap.log').write_text(r.stdout)
    return r

case('unexpected_arguments_fail_loud', unexpected_arguments)

def bootstrap_paths():
    errors = []
    for setup in ('relative', 'absolute'):
        p = fresh('bootstrap ' + setup); r = bootstrap_run(p, setup)
        observed = [(p / 'ports' / port).read_text(encoding='utf-8', errors='strict').strip() for port in ('driver', 'cleanup')]
        created = list((p / ('relative temp spaces' if setup == 'relative' else 'absolute temp spaces')).iterdir())
        absolute = all(os.path.isabs(value) for value in observed)
        same = len(created) == 1 and all((p / value).resolve() == created[0].resolve() for value in observed)
        print(f'bootstrap {setup}: exit {r.returncode}; driver absolute {os.path.isabs(observed[0])}; '
              f'cleanup absolute {os.path.isabs(observed[1])}; same created directory {same}; '
              f'outside sentinel {(p / "sentinel").is_file()}', flush=True)
        if r.returncode != 0 or not absolute or not same:
            errors.append(setup + ': driver or cleanup received a relative or mismatched root')
        check((p / 'sentinel').is_file(), 'outside sentinel removed')
    check(not errors, '; '.join(errors))
case('relative_tmpdir_has_absolute_driver_and_cleanup_root', bootstrap_paths)

def bootstrap_failures():
    errors = []
    for setup in ('empty', 'invalid', 'volume-root', 'mktemp-failure', 'normalization-failure'):
        p = fresh('bootstrap ' + setup); r = bootstrap_run(p, setup)
        entered = (p / 'ports/driver').exists(); cleaned = (p / 'ports/cleanup').exists()
        print(f'bootstrap {setup}: exit {r.returncode}; driver entered {entered}; cleanup called {cleaned}', flush=True)
        if r.returncode != 2 or entered or cleaned or not r.stdout.strip():
            errors.append(setup + ': setup failure did not stop before driver and cleanup')
    check(not errors, '; '.join(errors))
case('invalid_scratch_setup_stops_before_driver_and_cleanup', bootstrap_failures)

def function(text, name):
    # Load the owner's definition for execution; no lexical property is asserted.
    return re.search(r'^' + re.escape(name) + r'\(\).*?^}', text, re.M | re.S)[0]

def observe(label, r, p):
    names = re.findall(r'^test (.*?) \.\.\. (?:ok|FAILED|ignored)', r.stdout, re.M)
    print(f'{label}: exit {r.returncode}; completed {names}; witnesses {sorted(x.name for x in p.glob("witness-*"))}', flush=True)

def adapter(p):
    a = p / 'cargo-adapter'
    a.write_text('#!/usr/bin/env python3\nimport os,sys,json\n'
        + f'fixture={fixture!r}\nrecord={str(p / "argv.jsonl")!r}\n'
        + 'args=sys.argv[1:]; open(record,"a").write(json.dumps(args)+"\\n")\n'
        + 'h=args.index("--"); before=args[1:h]; after=args[h+1:]; skip=False; names=[]\n'
        + 'for arg in before:\n'
        + ' if skip: skip=False; continue\n'
        + ' if arg in ("-p","--test","--manifest-path"): skip=True; continue\n'
        + ' if not arg.startswith("-"): names.append(arg)\n'
        + 'import subprocess\nsys.exit(subprocess.run([fixture]+names+after).returncode)\n')
    a.chmod(0o755); return a

def checker_controls():
    p = fresh('checker spaces'); (p / 'go').touch()
    controls = [('ordinary', ['ordinary','--exact'],0), ('absent',['absent','--exact'],101),
        ('qualified::near',['near','--exact'],101),
        ('qualified::ignored_positive',['qualified::ignored_positive','--exact'],101),
        ('near',['near'],101), ('ignored_panic',['ignored_panic','--ignored','--exact'],101),
        ('qualified::ignored_positive',['qualified::ignored_positive','--ignored','--exact'],0),
        ('absent',['misleading','--exact'],101), ('misleading',['misleading','--exact'],0)]
    for i,(name,args,want) in enumerate(controls):
        p=fresh('checker '+str(i)); (p/'go').touch()
        raw=run([fixture]+args+pretty,p); log=p/f'raw-{i}.log'; log.write_text(raw.stdout)
        r=shell(f'source {q(helper)}; test_body_check {q(name)} {raw.returncode} {q(log)}',p)
        observe('checker '+name+' raw',raw,p)
        print(f'checker {name}: owner exit {r.returncode} expected {want}',flush=True)
        check(r.returncode==want,r.stdout)
    raw=run([fixture,'ordinary','--exact']+pretty,p)
    for label,data,want in [('CRLF',raw.stdout.replace('\n','\r\n'),0),('truncated',raw.stdout.split('test result:')[0],101)]:
        log=p/f'{label}.log'; log.write_bytes(data.encode())
        r=shell(f'source {q(helper)}; test_body_check ordinary 0 {q(log)}',p)
        check(r.returncode==want,label+' '+r.stdout)
    log=p/'reused.log'
    for name,want in [('ordinary',0),('absent',101)]:
        r=shell(f'source {q(helper)}; test_body_run {name} {q(log)} -- {q(fixture)} {name} --exact '+ ' '.join(pretty),p)
        check(r.returncode==want,'runner '+r.stdout)
    check('test ordinary ... ok' not in log.read_text(encoding='utf-8', errors='strict'),'old log survived truncation')
    check(shell(f'source {q(helper)}; test_body_check ordinary 17 {q(log)}',p).returncode==17,'raw nonzero lost')
    check(shell(f'source {q(helper)}; test_body_check ordinary 0 absent-file',p).returncode==2,'unreadable API')
case('checker_controls',checker_controls)

def captured_boundaries():
    p = fresh('captured boundaries')
    raw = run([fixture, 'misleading', '--exact'] + pretty, p)
    observe('captured misleading raw', raw, p)
    check(raw.returncode == 0 and (p / 'witness-misleading').is_file(), 'real misleading body did not complete')
    lines = raw.stdout.splitlines(keepends=True)
    opening = lines.index('---- misleading stdout ----\n')
    fake = next(i for i in range(opening + 1, len(lines)) if lines[i].startswith('test result:'))
    closing = next(i for i in range(fake + 1, len(lines)) if lines[i] == 'successes:\n')
    result = next(i for i in range(closing + 1, len(lines)) if lines[i].startswith('test result:'))
    selected = next(i for i in range(closing + 1, result) if lines[i].strip() == 'misleading')
    controls = [('complete', raw.stdout, 0), ('complete-CRLF', raw.stdout.replace('\n', '\r\n'), 0),
        ('truncated-captured-summary', ''.join(lines[:fake + 1]), 101),
        ('missing-capture-closure', ''.join(lines[:closing]), 101),
        ('missing-selected-list', ''.join(lines[:closing + 1]), 101),
        ('missing-outer-result', ''.join(lines[:result]), 101),
        ('duplicate-opening', ''.join(lines[:opening] + [lines[opening]] + lines[opening:]), 101),
        ('duplicate-closure', ''.join(lines[:closing] + [lines[closing]] + lines[closing:]), 101),
        ('duplicate-selected-list', ''.join(lines[:selected] + [lines[selected]] + lines[selected:]), 101),
        ('missing-opening', ''.join(lines[:opening] + lines[opening + 1:]), 101),
        ('contradictory-closure', ''.join(lines[:closing] + ['failures:\n'] + lines[closing + 1:]), 101),
        ('duplicate-outer-result', raw.stdout + lines[result], 101),
        ('extra-outer-run', raw.stdout + raw.stdout, 101),
        ('trailing-progress', raw.stdout + lines[2], 101),
        ('trailing-captured-data', raw.stdout + lines[opening + 1], 101)]
    errors = []
    for label, data, want in controls:
        log = p / (label + '.log'); log.write_bytes(data.encode())
        r = shell(f'source {q(helper)}; test_body_check misleading {raw.returncode} {q(log)}', p)
        (p / (label + '.verdict')).write_text(str(r.returncode) + '\n' + r.stdout)
        print(f'capture {label}: raw exit {raw.returncode}; owner exit {r.returncode} expected {want}', flush=True)
        if r.returncode != want:
            errors.append(label + ': incomplete or ambiguous captured log accepted' if want else label + ': complete output rejected')
    check(not errors, '; '.join(errors))
case('captured_fake_summary_cannot_replace_truncated_outer_result', captured_boundaries)

def cargo_parity():
    records=Path(os.environ['ISO_SH_CARGO_PROOFS'])
    for key,name,args,want in [
        ('normal','ordinary',['ordinary','--exact'],0),
        ('ignored','qualified::ignored_positive',['qualified::ignored_positive','--ignored','--exact'],0),
        ('absent','absent',['absent','--exact'],101),
        ('substring','near',['near'],101)]:
        p=fresh('cargo parity'); (p/'go').touch(); a=adapter(p)
        r=run([native_bash,'-c','exec \"$@\"','_',str(a),'test','-p','sot-log','--test','test_body_fixture',name,'--']+args[1:]+pretty,p)
        direct_names=re.findall(r'^test (.*?) \.\.\. (?:ok|FAILED|ignored)',r.stdout,re.M)
        cargo_log=(records/key/'cargo.log').read_text(encoding='utf-8', errors='strict')
        cargo_names=re.findall(r'^test (.*?) \.\.\. (?:ok|FAILED|ignored)',cargo_log,re.M)
        direct_witnesses={x.name for x in p.glob('witness-*')}
        cargo_witnesses={x.name for x in (records/key).glob('witness-*')}
        check(direct_names==cargo_names and direct_witnesses==cargo_witnesses,'Cargo/adapter body mismatch '+key)
        checked=shell(f'source {q(helper)}; test_body_check {q(name)} 0 {q(records/key/"cargo.log")}',p)
        check(checked.returncode==want,'Cargo verdict mismatch '+key)
        print(f'Cargo/adapter {key}: completed {cargo_names}; witnesses {sorted(cargo_witnesses)}; checked {want}',flush=True)
if 'ISO_SH_CARGO_PROOFS' in os.environ:
    case('cargo_parity',cargo_parity)

def bind_cargo(code, command):
    # The command is inserted literally: a Windows path holds backslash sequences re.sub would read as escapes.
    return re.sub(r'\bcargo(?= test)', lambda _: q(command), code)

def wake_binding_is_literal():
    for path in [r'C:\Users\x\cargo-adapter', r'D:\a\1\g\b\cargo-adapter', '/tmp/a b/\\n']:
        got = bind_cargo('cargo test --exact one\n', path)
        check(got == q(path) + ' test --exact one\n', f'path altered by binding: {got!r}')
case('wake_binding_keeps_a_path_literal',wake_binding_is_literal)

if mode=='all':
    def gate_job():
        errors=[]
        check(sys.platform.startswith('linux'),'gate controls require Linux')
        for name,want,panic in [('absent',101,False),('qualified::ignored_positive',101,False),('ordinary',0,False),('ordinary',101,True)]:
            p=fresh('gate job'); (p/'rust').mkdir(); (p/'steps').mkdir()
            if panic: (p/'iso-sh-panic-request').touch()
            row='\t'.join(['one','control',fixture,str(p),name])
            extra={'RCG_D':str(repo),'RCG_L':str(p),'RCG_CARGO_DIR':'/usr/bin','RCG_JULIA_DIR':'/usr/bin'}
            r=run([native_bash,str(repo/'scripts/tests/rc-gate.sh'),'--job',row],p,extra)
            rc=(p/'rust/control.rc').read_text(encoding='utf-8', errors='strict').strip()
            print(f'--job one {name} panic={panic}: leaf exit {r.returncode}; result {rc}; witnesses {sorted(x.name for x in p.glob("witness-*"))}',flush=True)
            if rc!=str(want): errors.append(f'{name}: selected job recorded {rc} instead of {want}')
        p=fresh('empty bin'); (p/'rust').mkdir(); (p/'steps').mkdir(); a=p/'empty-bin'
        a.write_text('#!/usr/bin/env bash\nexec '+q(fixture)+' absent --exact\n'); a.chmod(0o755)
        run([native_bash,str(repo/'scripts/tests/rc-gate.sh'),'--job','\t'.join(['bin','empty',str(a),str(p)])],p,
            {'RCG_D':str(repo),'RCG_L':str(p),'RCG_CARGO_DIR':'/usr/bin','RCG_JULIA_DIR':'/usr/bin'})
        check((p/'rust/empty.rc').read_text(encoding='utf-8', errors='strict').strip()=='0','empty whole binary rejected')
        # the jobs' PATH holds the julia the gate chose: a job's `command -v julia` names the stub in RCG_JULIA_DIR, not the
        # one in cargo's folder (a folder of the case's own, so the proof holds whatever the host's /usr/bin holds)
        p=fresh('julia path'); (p/'rust').mkdir(); (p/'steps').mkdir(); (p/'jbin').mkdir(); (p/'cargo-bin').mkdir()
        j=p/'jbin/julia'
        for f in (p/'cargo-bin/julia',j): f.write_text('#!/bin/sh\nexit 0\n'); f.chmod(0o755)
        a=p/'julia-probe'; a.write_text('#!/bin/sh\ncommand -v julia > '+q(p/'julia-path')+'\nexit 0\n'); a.chmod(0o755)
        run([native_bash,str(repo/'scripts/tests/rc-gate.sh'),'--job','\t'.join(['bin','probe',str(a),str(p)])],p,
            {'RCG_D':str(repo),'RCG_L':str(p),'RCG_CARGO_DIR':str(p/'cargo-bin'),'RCG_JULIA_DIR':str(p/'jbin')})
        got=(p/'julia-path').read_text(encoding='utf-8', errors='strict').strip() if (p/'julia-path').exists() else 'no julia'
        check(got==str(j),f'the job found julia at {got}, not the gate-chosen {j}')
        check(not errors,'; '.join(errors))
    case('one_job_requires_completed_selected_body',gate_job)

    def producer_controls():
        for listing in ['real','empty','failed','inconsistent','missing-fixture','duplicate-fixture']:
            p=fresh('producer'); (p/'rust').mkdir(); (p/'steps').mkdir(); exe=p/'listing-adapter'
            exe.write_text('#!/usr/bin/env python3\nimport os,sys\n'+f'fixture={fixture!r}; mode={listing!r}\n'
                +'if "--ignored" in sys.argv:\n if mode=="empty": sys.exit(0)\n if mode=="failed": sys.exit(17)\n if mode=="inconsistent": print("absent: test"); sys.exit(0)\n'
                +'os.execv(fixture,[fixture]+sys.argv[1:])\n'); exe.chmod(0o755)
            rows=f'comm_wake\t{exe}\t{p}\n'
            fixture_row=f'test_body_fixture\t{fixture}\t{p}\n'
            rows+=fixture_row*(0 if listing=='missing-fixture' else 2 if listing=='duplicate-fixture' else 1)
            (p/'tests.tsv').write_text(rows)
            prefix=gate[gate.index('envs()'):gate.index('if [ "${1:-}" = --job ]')]
            body=f'source {q(helper)}\nD={q(repo)}; L={q(p)}; BUILD_RC=0; TO=(timeout 30); CE=(env); SPLIT=(comm_wake); JULIA_PKGS=()\n'+prefix+'\nproducer\n'
            r=shell(body,p); jobs=[s.split('\t') for s in r.stdout.splitlines() if s.startswith('one\t')]; emitted=[row[-1] for row in jobs]
            summary=(p/'summary.txt').read_text(encoding='utf-8', errors='strict'); raw=run([fixture,'--list','--ignored','--format','terse'],p)
            ignored=[s[:-6] for s in raw.stdout.splitlines() if s.endswith(': test')]
            print(f'producer {listing}: emitted {emitted}; skipped {summary.count("split-skipped")}; step results {[x.read_text(encoding="utf-8", errors="strict").strip() for x in (p/"steps").glob("*.rc")]}',flush=True)
            if listing in ('missing-fixture','duplicate-fixture'):
                check((p/'steps/scripts-tests-test-test-body.rc').read_text(encoding='utf-8', errors='strict').strip()=='101','fixture artifact error accepted')
                check(not any(row[0]=='shell' and row[1]=='test-test-body' for row in (line.split('\t') for line in r.stdout.splitlines())),'invalid fixture proof job emitted')
            elif listing=='real':
                check(not set(emitted).intersection(ignored),'ignored names emitted as ordinary jobs')
                check(summary.count('split-skipped')==len(ignored),'ignored skip records absent')
                normal=run([fixture,'--list','--format','terse'],p); ordinary={s[:-6] for s in normal.stdout.splitlines() if s.endswith(': test')}-set(ignored)
                check(set(emitted)==ordinary and len(emitted)==len(ordinary),'ordinary partition not once each')
            elif listing=='empty': check(bool(emitted),'empty ignored list rejected')
            else: check((p/'steps/split-comm_wake.rc').read_text(encoding='utf-8', errors='strict').strip()=='101','listing failure has no failed partition result')
            if listing not in ('missing-fixture','duplicate-fixture'):
                proof_jobs=[line.split('\t') for line in r.stdout.splitlines() if line.startswith('shell\ttest-test-body\t')]
                check(len(proof_jobs)==1 and proof_jobs[0][-2:]==['--all',fixture],'proof fixture argv/discovery lost')
            check((p/'steps/missing-fe_client_supervisor_word__unresponsive_supervisor_expires_the_health_window.rc').read_text(encoding='utf-8', errors='strict').strip()=='101','missing slow-first request not a result failure')
    case('split_producer_does_not_schedule_ignored_as_ordinary',producer_controls)
if failures:
    print('FAIL: selected Rust body proofs ('+mode+'): '+', '.join(failures),flush=True); sys.exit(1)
print('PASS: selected Rust body proofs ('+mode+')',flush=True)
PY
