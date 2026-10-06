#!/usr/bin/env python3
"""Validate actual libtest observations, never source text or exit status alone."""
import argparse
import json
import pathlib
import re
import sys


def validate(case, raw, code):
    errors = []
    name, status = case['test'], case['result']
    if code != case['exit']:
        errors.append(f"exit {code}, expected {case['exit']}")
    # A complete exact run has one summary per libtest process. Isolated tests
    # run their wrapper and one child; their substantive body still enters once.
    processes = 2 if case.get('isolated') else 1
    starts = re.findall(r'^running (\d+) tests?\s*$', raw, re.M)
    if starts != ['1'] * processes:
        errors.append('missing, filtered, or extra libtest body')
    summaries = re.findall(r'^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured;', raw, re.M)
    wanted = (status, '1' if status == 'ok' else '0', '0' if status == 'ok' else '1', '0', '0')
    if summaries != [wanted] * processes:
        errors.append('actual summaries do not describe the exact expected body')
    results = re.findall(r'^test (\S+) \.\.\. (ok|FAILED)\s*$', raw, re.M)
    # The isolated child's --nocapture output shares its unfinished result line;
    # the outer --show-output result must still be a complete named result line.
    if not results or any(item != (name, status) for item in results):
        errors.append('missing or wrong actual named test result line')
    if len(results) > processes:
        errors.append('multiple actual named test result lines')
    entered = 'T1 body entered: ' + name
    if raw.count(entered) != 1:
        errors.append('named substantive body did not enter exactly once')
    output_start = raw.find('running 1 test')
    output_end = raw.rfind('test result:')
    observed = raw[output_start:output_end] if output_start >= 0 else ''
    if case.get('isolated'):
        # Only the self-run child may emit this witness, after run_isolated
        # entered the body. The existing Entry checks remain in place.
        start = re.search(r'^test ' + re.escape(name) + r' \.\.\. T1 body entered: ' + re.escape(name) + r'\s*$', observed, re.M)
        if not start:
            errors.append('isolated child result/body witness missing')
    elif f'---- {name} stdout ----' not in observed:
        errors.append('named captured output section missing')
    for witness in case['observations']:
        if witness not in observed:
            errors.append('missing fixture observation: ' + witness)
    panics = re.findall(r"thread '([^']+)'[^\n]* panicked at [^\n]*:\n([^\n]*(?:\n(?:left:| right:|  left:| right)[^\n]*)?)", observed)
    assertion_panics = [message for thread, message in panics if thread == name and case['assertion'] in message]
    if status == 'FAILED':
        if len(assertion_panics) != 1:
            errors.append('specific assertion is not the named failing panic')
    elif 'T1 assertion passed: ' + case['assertion'] not in observed:
        errors.append('specific assertion-passed observation missing')
    for thread, message in panics:
        setup = case.get('occupied_bind') and message.startswith('bind: ') and 'kind: AddrInUse' in message
        wrapper = case.get('isolated') and message.startswith(f'isolated test {name} failed in its child process:') and status == 'FAILED'
        target = thread == name and case['assertion'] in message and status == 'FAILED'
        if thread != name or not (setup or wrapper or target):
            errors.append('unrelated panic or unavailable fixture: ' + message)
    if re.search(r'^error(?:\[|:(?! test failed))|could not compile|could not execute process', raw, re.M):
        errors.append('compilation or execution failure')
    if re.search(r'PermissionDenied|Operation not permitted|fixture unavailable', observed, re.I):
        errors.append('fixture unavailable or skipped')
    return errors


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('cases')
    parser.add_argument('label')
    parser.add_argument('log')
    parser.add_argument('exit', type=int)
    args = parser.parse_args()
    cases = json.loads(pathlib.Path(args.cases).read_text())
    case = next(item for item in cases if item['label'] == args.label)
    errors = validate(case, pathlib.Path(args.log).read_text(errors='replace'), args.exit)
    print(args.label + (' INVALID: ' + '; '.join(errors) if errors else ' VALID: named result, assertion, body and fixtures observed'))
    return int(bool(errors))


if __name__ == '__main__':
    sys.exit(main())
