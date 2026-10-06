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
python3 - "$mode" "$2" "$(dirname "${BASH_SOURCE[0]}")/../.." "$root" "$proof_bash" <<'PY'
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
twohost = (repo / 'comm/tests/test-inbox-lock-twohost.sh').read_text(encoding='utf-8', errors='strict')
e2e = (repo / 'comm/tests/test-comm-e2e-readers.sh').read_text(encoding='utf-8', errors='strict')
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
        return subprocess.run(argv, cwd=p, env=actual, text=True, encoding='utf-8', errors='strict',
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=45)
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
    bootstrap = text[:text.index('python3 - "$mode"')]
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
        + 'os.execv(fixture,[fixture]+names+after)\n')
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

def arm_setup(p):
    a=adapter(p)
    return f'source {q(helper)}\nLOCAL={q(p)}; RUST_DIR={q(p)}\ncargo() {{ {q(a)} "$@"; }}\n'+function(twohost,'rust_arm')+'\n'

def twohost_selection():
    errors=[]
    p=fresh('T3 zero')
    start=twohost.index('c="$(new_case t3)"')
    leaf=twohost[start:twohost.index('# Wait until',start)]
    setup=arm_setup(p)+function(twohost,'verdict')+'\nPASS=0; FAIL=0; new_case() { echo .; }\n'
    r=shell(setup+leaf+'\nprintf "T3 FAIL=%s\\n" "$FAIL"; [ "$FAIL" -eq 1 ]',p)
    print(r.stdout,end='',flush=True)
    if r.returncode!=0: errors.append('T3 accepted zero-body arm')
    for name,want in [('absent',101),('ignored_positive',101),('qualified::ignored_positive',0)]:
        p=fresh('arm'); (p/'go').touch()
        r=shell(arm_setup(p)+f'rust_arm {q(p)} {q(name)}',p); observe('rust_arm '+name,r,p)
        if r.returncode!=want: errors.append(f'actual arm {name}: owner exit {r.returncode} expected {want}')
        if want==0:
            check((p/'witness-ignored-positive').exists() and (p/'rust.ready').exists(),'positive body/readiness absent')
            check('filed fixture' in r.stdout and 'route local' in r.stdout,'payload lost')
    check(not errors,'; '.join(errors))
case('twohost_zero_selection_is_failure',twohost_selection)

def async_results():
    errors=[]
    start=twohost.index('c="$(new_case a1)"; mkfifo'); leaf=twohost[start:twohost.index('# ---- (a2)',start)]
    for failing,reverse in [(False,False),(True,False),(True,True)]:
        p=fresh('async')
        if failing: (p/'iso-sh-panic-request').touch()
        setup=arm_setup(p)+function(twohost,'verdict')+'\n'
        collector=re.search(r'^wait_writers\(\).*?^}',twohost,re.M|re.S)
        if collector: setup+=collector[0]+'\n'
        setup+=(f'PASS=0; FAIL=0; PROOF_BAD=0; PEER=fixture; APPEND=:; c={q(p)}\nnew_case() {{ echo {q(p)}; }}\n'
            'peer_sh() { echo ready; read -r _; echo "filed peer"; }\n'
            'check_inbox() { :; }; overlap() { :; }; interleaved() { :; }\n'
            'mkfifo() { : > "$1"; }; jq() { printf "peer\\nrust\\npeer\\n"; }\n')
        setup+=function(twohost,'proof_case')+'\n'
        if reverse:
            setup+='peer_sh() { echo ready; read -r _; while [ ! -e "$c/rust-done" ]; do :; done; echo "filed peer"; }\n'
            setup+='eval '+q(function(twohost,'rust_arm').replace('rust_arm()','inner_arm()',1))+'\n'
            setup+='rust_arm() { inner_arm "$@"; local r=$?; touch "$c/rust-done"; return "$r"; }\n'
        # Bind the real Rust body's cwd to the caller's inbox readiness port.
        setup=setup.replace('RUST_DIR='+q(p),'RUST_DIR='+q(p/'inbox'))
        (p/'inbox/iso-sh-fixture-request').touch()
        if failing: (p/'inbox/iso-sh-panic-request').touch()
        r=shell(setup+leaf+'\nprintf "owner FAIL=%s\\n" "$FAIL"; [ "$FAIL" -eq '+('1' if failing else '0')+' ]',p)
        observe(f'async failing={failing} peer-last={reverse}',r,p)
        print(r.stdout, end='',flush=True)
        payload=(p/'a1.rust').read_text(encoding='utf-8', errors='strict'); print('Rust writer output: '+payload.strip(),flush=True)
        print('Rust writer witnesses: '+str(sorted(x.name for x in (p/'inbox').glob('witness-*'))),flush=True)
        if r.returncode!=0: errors.append(f'earlier writer failure erased; peer-last={reverse}')
        check((p/'inbox/witness-appender').exists() and (p/'inbox/rust.ready').exists() and (p/'inbox/go').exists(),'ready/go/body absent')
    check(not errors,'; '.join(errors))
case('twohost_each_async_result_is_required',async_results)

def wake_script():
    return re.search(r'cat > "\$E/wake.sh" <<\x27EOF\x27\n(.*?)\nEOF',e2e,re.S)[1]

def report_setup(p,ping=True,result=True):
    (p/'strict-start').write_text('250\n'); (p/'final-here').write_text('250\n')
    if ping: (p/'ping-here.log').write_text('100 ping\n200 ping\n300 ping\n')
    if result:
        (p/'wake-result-here').write_text('0\n'); (p/'wake-wait-here').write_text('0\n')
    (p/'pollout-here.log').write_text('400 m-one\n')
    for name in ['here','peer']: (p/f'send-e2e-snd-{name}.log').write_text('50 fixture m-one filed\n')
    start=e2e.index('skew_of()'); leaf=e2e[start:e2e.index('nwire=0',start)]
    return (function(e2e,'verdict')+'\nPASS=0; FAIL=0; HANDLES=(fixture); QUIET=60; '
        f'L={q(p)}; skew_peer=0; skew_v3=0; tag_of() {{ echo here; }}\n'+leaf)

def wake_completion():
    p=fresh('staged wake'); (p/'log').mkdir(); e=p/'e2e'; e.mkdir()
    (e/'lib-test-body.sh').write_bytes(helper.read_bytes()); a=adapter(p)
    # Bind only the external command word; preserve generated selector and verdict logic.
    code=re.sub(r'\bcargo(?= test)',q(a),wake_script()); (e/'wake.sh').write_text(code)
    r=run([native_bash,str(e/'wake.sh'),str(p),'fixture','here'],p,{'SOT_E2E_MANIFEST':str(p/'unused-manifest')})
    observe('generated wake',r,p)
    check((p/'witness-wake').exists() and not (p/'witness-nested-wake').exists(),'substring ran extra wake body')
    check(r.returncode==0,r.stdout)
    check((p/'log/ping-here.log').stat().st_size>0,'ping absent')
    check((p/'log/wake-result-here').read_text(encoding='utf-8', errors='strict').strip()=='0','checked terminal result absent')
    for broken in ['panic','missing-helper']:
        p=fresh('wake '+broken); (p/'log').mkdir(); e=p/'e2e'; e.mkdir()
        if broken=='panic':
            (p/'iso-sh-panic-request').touch()
            (e/'lib-test-body.sh').write_bytes(helper.read_bytes())
        a=adapter(p); (e/'wake.sh').write_text(re.sub(r'\bcargo(?= test)',q(a),wake_script()))
        r=run([native_bash,str(e/'wake.sh'),str(p),'fixture','here'],p,{'SOT_E2E_MANIFEST':str(p/'unused-manifest')})
        print(f'generated wake {broken}: exit {r.returncode}',flush=True)
        check(r.returncode!=0,'broken generated control accepted')
        if broken=='panic': check((p/'log/wake-result-here').read_text(encoding='utf-8', errors='strict')=='101\n','failed control status lost')
case('wake_exact_selection',wake_completion)

def wake_decisions():
    for ping,result in [(False,True),(True,False),(False,False),(True,True)]:
        p=fresh('wake decision'); body=report_setup(p,ping,result)
        r=shell(body+'\nprintf "FAIL=%s\\n" "$FAIL"',p); want=0 if ping and result else 1
        print(f'wake decision ping={ping} result={result}: {r.stdout.strip()}',flush=True)
        check(f'FAIL={want}\n' in r.stdout,'absent control accepted')
        text='exact one-shot wake control completed with ping evidence' if want==0 else 'one-shot wake control lacks successful completion or ping evidence'
        check(text in r.stdout,'mandatory verdict absent')
case('wake_control_requires_exact_completion',wake_decisions)

def wake_report():
    p=fresh('wake report'); body=report_setup(p); first=shell(body,p)
    (p/'pollout-here.log').write_text('1 m-one\n'); (p/'send-e2e-snd-here.log').write_text('500 fixture m-one filed\n')
    second=shell(body,p); print(first.stdout,end='',flush=True)
    check('private one-shot control: 2 pings observed before strict start; shared-message wake coverage is not checked' in first.stdout,'report falsely attributes shared messages')
    check(first.stdout==second.stdout,'shared timestamps changed private-control report')
    check('NOTE: fixture: 1 late control pings observed after the final poll; sustained shared-inbox wake behavior is not checked.' in first.stdout,'late pings treated as wake-storm proof')
case('wake_report_is_private_observation',wake_report)

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
                check((p/'steps/test-test-body.rc').read_text(encoding='utf-8', errors='strict').strip()=='101','fixture artifact error accepted')
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
