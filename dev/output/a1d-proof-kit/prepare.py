"""Archive C/P/head and overlay identical real fixtures with each independent reversed acceptance/operation."""
import io
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tarfile
C = '7c36f4d0974f653b4843ff88024f2ba395b0f1ab'
P = '877eac1cd98a4988967357c6b0d918273d9584f3'
ISO = 'rust/log/src/test_isolated.rs'
DIAG = 'rust/log/tests/socket_unix/diagnostics.rs'
MAIN = 'rust/log/tests/socket_unix/main.rs'
MANIFEST = [ISO, DIAG, 'rust/log/CLAUDE.md', 'rust/log/tests/CLAUDE.md']
head, dest = sys.argv[1:]
repo = Path.cwd().resolve()
root = Path(dest).resolve()
root.mkdir(parents=True, exist_ok=True)
os.umask(0o022)
def git(*args):
    return subprocess.check_output(['bash', '-c', '( cd ' + shlex.quote(str(repo)) + ' && git ' + shlex.join(args) + ' )'])
def archive(ref, label):
    tree = root / label
    tree.mkdir(exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(git('archive', C if ref == 'WORKING' else ref))) as content:
        content.extractall(tree)
    if ref == 'WORKING':
        for path in MANIFEST:
            (tree / path).write_bytes((repo / path).read_bytes())
    print('prepared ' + label, flush=True)
    return tree

def replace(s, old, new):
    assert s.count(old) == 1, 'proof overlay anchor moved'
    return s.replace(old, new)
source = archive(head, 'head')
archive(C, 'parent')
archive(P, 'n3-parent')
iso = (source / ISO).read_text()
diag = (source / DIAG).read_text()
main = (source / MAIN).read_text()
old_readiness = '    assert!(start, "readiness proof did not observe fixture start");\n    assert!(matched, "readiness proof observed the wrong failure");'
broad_readiness = '    assert!(outcome.work.is_err(), "readiness failure not observed");'
readiness = replace(iso, old_readiness, broad_readiness)
old_capture = '''            let decoder = failure.stream == "stderr"
                && failure.kind == Some(std::io::ErrorKind::InvalidData)
                && failure.reason == "stream did not contain valid UTF-8";'''
capture = replace(iso, old_capture, '            let decoder = true;')
# No validation is changed in the corrected N3 rows. The old syscall's real I/O error remains observable.
legacy_bytes = replace(iso, '''                let mut bytes = Vec::new();
                let result = pipe.read_to_end(&mut bytes).map(|_| ());''', '''                let mut decoded = String::new();
                let result = pipe.read_to_string(&mut decoded).map(|_| ());
                let bytes = decoded.into_bytes();''')
# P raised failed readiness before normal finalization. Observe that absence BEFORE controller-owned recovery.
# The controller then uses the existing ISO owner on its recorded child; no second waiter/polling loop is added.
legacy_ready = replace(iso, '''    if work.is_err() {
        drop(draining.child.stdin.take());
    }''', '''    let bypassed = work.is_err();
    if bypassed {
        eprintln!("proof-observation child={pid} readiness=failed normal-finalization=absent");
        drop(draining.child.stdin.take());
    }''')
legacy_ready = replace(legacy_ready, '    let termination = match draining.child.try_wait() {',
    '    let mut termination = match draining.child.try_wait() {')
legacy_ready = replace(legacy_ready, '    FixtureOutcome {', '''    if bypassed {
        eprintln!("proof-recovery child={pid} status={:?} cleanup={} entry={}", wait,
            if termination.is_ok() { "confirmed" } else { "unconfirmed" },
            if entry.is_ok() { "once" } else { "failed" });
        termination = Err("normal finalization absent before controller recovery".into());
    }
    FixtureOutcome {''')
for label, ref, altered in [
    ('parent-cause', C, readiness), ('revert-cause', head, readiness),
    ('parent-capture', C, capture), ('revert-capture', head, capture),
    ('parent-ready', P, legacy_ready), ('revert-ready', head, legacy_ready),
    ('parent-utf8', P, legacy_bytes), ('revert-utf8', head, legacy_bytes),
]:
    tree = archive(ref, label)
    # Tests, fixtures and narrowly retained observations are identical; P wait/entry/cutoff operations are unchanged.
    (tree / ISO).write_text(altered)
    (tree / DIAG).write_text(diag)
    (tree / MAIN).write_text(main)
(root / 'manifest.txt').write_text(f'''C={C}\nP={P}\nhead={head}\n
parent-cause/capture: C plus identical new fixtures, witnesses, retained output observations, controls and broad C acceptance.
revert-cause/capture: head with only the corresponding cause acceptance reversed.
parent-ready: P plus identical corrected fixture/verifier and pre-recovery absence observation; the controller completes ISO recovery after observing the bypass.
revert-ready: head with only scoped failed-readiness finalization reversed to that same observation/recovery adapter.
parent-utf8: P plus identical corrected fixture/verifier and independently retained stream/status observations; real P read_to_string remains.
revert-utf8: head with only byte reading reversed to read_to_string, retaining all independent observations and exact cause checks.
P socket rows also retain the head socket main fixture/capture adapters for the identical new diagnostic bodies; no socket wait bound or transport changes. All overlays are test-only mechanical observations. Head scope/byte code in other rows is prerequisite preservation, not the fix assigned red in that row. No transport or admission operation changes.
''')
