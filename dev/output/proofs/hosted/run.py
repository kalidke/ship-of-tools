"""Run exact T1 observations on the current replay checkout, with one validator."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--platform', choices=['Linux', 'macOS', 'Windows', 'linux', 'macos', 'windows'])
    parser.add_argument('--local', action='store_true', help='Exclude previously denied socket bodies; no hosted verdict')
    args = parser.parse_args()
    platform = (args.platform or {'linux': 'linux', 'darwin': 'macos', 'win32': 'windows'}[sys.platform]).lower()
    package = Path(__file__).resolve().parent
    root = package.parents[3]
    logs = package / 'logs'
    logs.mkdir(exist_ok=True)
    os.umask(0o022)
    spec = importlib.util.spec_from_file_location('t1_validator', package / 'validate.py')
    validator = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(validator)
    control = subprocess.run([sys.executable, str(package / 'test-validator.py')], cwd=root,
                             capture_output=True, text=True, timeout=30)
    (logs / 'validator-controls.log').write_text(control.stdout + control.stderr)
    print(control.stdout, end='', flush=True)
    if control.returncode:
        print(control.stderr, flush=True)
        return 1
    cases = json.loads((package / 'cases.json').read_text())
    applicable = [case for case in cases if platform in case.get('platforms', ['linux', 'macos', 'windows'])]
    if not applicable:
        print('INVALID: no declared case applies to this platform', flush=True)
        return 1
    nice = shutil.which('nice')
    if not nice:
        print('INVALID: required nice utility is unavailable', flush=True)
        return 1
    cargo_env = dict(os.environ, CARGO_NET_OFFLINE='true', CARGO_PROFILE_DEV_DEBUG='line-tables-only')
    target = Path(cargo_env.get('CARGO_TARGET_DIR', str(root / 'rust/target')))
    if not target.is_absolute():
        target = root / 'rust' / target
    target_logs = target / 'logs'
    target_logs.mkdir(parents=True, exist_ok=True)
    failures, ran = 0, 0
    summary = []
    for case in applicable:
        label = case['label']
        if args.local and case.get('not_runnable_here'):
            line = label + ' not runnable here: previously denied socket fixture; not retried'
            print(line, flush=True)
            summary.append(line)
            continue
        command = [nice, '-n', '10', 'cargo', 'test', '-p', 'sot-frontend', '--bin', 'sot',
                   '-j', '8', '--locked', case['test'], '--', '--exact', '--show-output']
        log = logs / (label + '.log')
        with log.open('w') as output:
            process = subprocess.Popen(command, cwd=root / 'rust', env=cargo_env, stdout=output,
                                       stderr=subprocess.STDOUT)
            # Only this recorded child may be ended by the runner's timeout.
            (logs / (label + '.pid')).write_text(str(process.pid) + '\n')
            try:
                code = process.wait(timeout=540)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=30)
                output.write('\nfixture unavailable: runner deadline expired\n')
                code = 124
        raw = log.read_text(errors='replace')
        (logs / (label + '.exit')).write_text(str(code) + '\n')
        shutil.copyfile(log, target_logs / ('r2-' + label + '.log'))
        comparison = f'{label} exit {code} expected {case["exit"]}'
        print(comparison, flush=True)
        # These are verbatim observed log lines; declarations are never printed as evidence.
        for line in raw.splitlines():
            if (line.startswith(('test ', 'T1 ', 'forced occupied-path', 'thread ', 'test result:', 'error'))
                    or case['assertion'] in line):
                print(line, flush=True)
        errors = validator.validate(case, raw, code)
        verdict = label + (' INVALID: ' + '; '.join(errors) if errors else
                           ' VALID: named result, assertion, body and fixtures observed')
        (logs / (label + '.validation')).write_text(verdict + '\n')
        print(verdict, flush=True)
        summary += [comparison, verdict]
        failures += bool(errors)
        ran += 1
    (logs / 'summary.txt').write_text('\n'.join(summary) + '\n')
    if args.local:
        print(f'LOCAL observations: {ran} runs, {failures} invalid; hosted socket/platform evidence remains pending', flush=True)
    return int(bool(failures))


if __name__ == '__main__':
    sys.exit(main())
