"""Synthetic log controls for the observation validator, not product evidence."""
import importlib.util
import pathlib

root = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('validator', root / 'validate.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
destination = root / 'validator-controls'
destination.mkdir(exist_ok=True)
name = 'fixture::named_behavior'
case = {'test': name, 'exit': 101, 'result': 'FAILED', 'assertion': 'owned resource removed',
        'observations': ['fixture ready'], 'isolated': False}
valid = f'''running 1 test
test {name} ... FAILED
---- {name} stdout ----
T1 body entered: {name}
fixture ready
thread '{name}' (1) panicked at fixture.rs:1:1:
owned resource removed
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s
'''
logs = {
    'zero-body': ('running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out\n', 0),
    'compilation-failure': ('error[E0308]: failed to compile\nexpected assertion: owned resource removed\nerror: could not compile fixture\n', 101),
    'unrelated-assertion': (valid.replace("owned resource removed\ntest result:", "different assertion failed\nexpected assertion: owned resource removed\ntest result:"), 101),
    'unavailable-fixture': (valid.replace("owned resource removed\ntest result:", 'bind: Os { kind: PermissionDenied, message: "Operation not permitted" }\nexpected assertion: owned resource removed\ntest result:'), 101),
    'wrong-body': (valid.replace(name, 'fixture::other_behavior'), 101),
    'multiple-body': (valid.replace('fixture ready', f'T1 body entered: {name}\nfixture ready'), 101),
    'unrelated-thread': (valid.replace(f"thread '{name}'", "thread 'other'"), 101),
    'synthetic-valid-red': (valid, 101),
    'synthetic-valid-green': (valid.replace('FAILED', 'ok').replace('0 passed; 1 failed', '1 passed; 0 failed').replace(
        f"thread '{name}' (1) panicked at fixture.rs:1:1:\nowned resource removed", 'T1 assertion passed: owned resource removed'), 0),
}
report = []
for label, (raw, code) in logs.items():
    candidate = dict(case)
    if label == 'synthetic-valid-green':
        candidate.update(exit=0, result='ok')
    errors = module.validate(candidate, raw, code)
    (destination / (label + '.log')).write_text(raw)
    expected_rejection = not label.startswith('synthetic-valid')
    assert bool(errors) == expected_rejection, (label, errors)
    line = label + (': rejected — ' + '; '.join(errors) if errors else ': accepted synthetic control')
    report.append(line)
    print(line)
(destination / 'results.txt').write_text('\n'.join(report) + '\n')
