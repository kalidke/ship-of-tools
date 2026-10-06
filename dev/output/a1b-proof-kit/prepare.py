"""Build disposable archive proof trees; transformations implement named negative controls, never source proofs."""
import io
import os
from pathlib import Path
import subprocess
import sys
import tarfile

PARENT = 'c9ae5ba7ff7e1877c1abf3e96770a0c78d3c71e7'
head, destination = sys.argv[1:]
root = Path(destination).resolve()
root.mkdir(parents=True, exist_ok=True)

def archive(ref, name):
    tree = root / name
    tree.mkdir(exist_ok=True)
    data = subprocess.check_output(['git', 'archive', PARENT if ref == 'WORKING' else ref])
    with tarfile.open(fileobj=io.BytesIO(data)) as content:
        content.extractall(tree, filter='data')
    if ref == 'WORKING':
        for path in subprocess.check_output(['git', 'diff', 'HEAD', '--name-only'], text=True).splitlines():
            (tree / path).write_bytes(Path(path).read_bytes())
    return tree

def read(tree, path):
    return (tree / path).read_text()

def write(tree, path, text):
    (tree / path).write_text(text)

def change(tree, path, before, after):
    text = read(tree, path)
    if text.count(before) != 1:
        raise RuntimeError(f'negative-control anchor moved: {path}')
    write(tree, path, text.replace(before, after))

def function(tree, path, signature, body):
    text = read(tree, path)
    start = text.index(signature)
    opening = text.index('{', start)
    depth = 1
    end = opening + 1
    # These selected functions contain balanced braces in strings too.
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    write(tree, path, text[:opening+1] + '\n' + body + '\n' + text[end-1:])

PROGRESS = 'rust/log/src/lane/test_progress.rs'
WAIT = 'rust/log/tests/socket_unix/main.rs'
ISO = 'rust/log/src/test_isolated.rs'
DIAGNOSTICS = 'rust/log/tests/socket_unix/diagnostics.rs'
PAGE = 'rust/log/src/identity/peer_owner/mod.rs'
SESSION = 'rust/backend/src/server/listen.rs'
UNIX = 'rust/log/tests/socket_unix/connect.rs'
PIPE = 'rust/log/tests/pipe_win/connect.rs'
source = archive(head, 'head')
parent = archive(PARENT, 'parent')
# Identical bodies/observation seams; parent overlays are explicitly narrower claims.
fixtures = [PROGRESS, WAIT, ISO, DIAGNOSTICS, PAGE, SESSION, UNIX, PIPE,
 'rust/log/src/lane/socket_unix/server.rs', 'rust/log/tests/socket_unix/close.rs',
 'rust/log/tests/socket_unix/client.rs', 'rust/log/tests/socket_unix/teardown.rs',
 'rust/log/tests/other_account.rs', 'rust/log/src/lane/mod.rs']

def parent_overlay(name):
    tree = archive(PARENT, name)
    for path in fixtures:
        write(tree, path, read(source, path))
    return tree

def blocking(tree, original=False):
    before = '''        let mut ring = match self.ring.try_lock() {
            Ok(ring) => ring,
            Err(_) => {
                self.skipped.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };'''
    after = '''        let mut ring = self.ring.lock().expect("recorder lock");'''
    change(tree, PROGRESS, before, after)
    if original:
        # API layout supports identical fixtures, but the original admission never counts skipped work.
        function(tree, PROGRESS, 'pub(crate) fn note(&self', '''        let mut ring = self.ring.lock().expect("recorder lock");
        if ring.records.len() == CAPACITY { ring.records.pop_front(); ring.overwritten += 1; }
        ring.records.push_back(Checkpoint { conn, step, elapsed_ms: self.started.elapsed().as_millis(), result: result.to_string() });''')

def deadline(tree):
    change(tree, WAIT, 'wait_until(child, self.deadline)',
           'wait_until(child, Instant::now() + self.deadline.duration_since(self.started))')
    change(tree, WAIT, 'server.join_workers(self.deadline)',
           'server.join_workers(Instant::now() + TIMEOUT)')
    change(tree, WAIT, 'if ok { "ok" } else { "timeout" }', 'if ok { "ok" } else { "ok" }')
    change(tree, WAIT, 'run_isolated_until(&self.test, self.deadline)',
           'run_isolated_until(&self.test, Instant::now() + sot_log::test_isolated::ISOLATION_TIMEOUT)')

def separate_io(tree):
    # Leave the real read, deadline, outcome and actual error intact; remove only its server-bearing history path.
    change(tree, WAIT, 'Err(error) => self.complete(error.kind(blocking), Some(error), server)',
           'Err(error) => self.complete(error.kind(blocking), Some(error), None)')

def immediate(tree):
    function(tree, DIAGNOSTICS, 'fn available_during_fixture_hold', '''    let held = server.hold_progress_for_test();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let snapshot = server.progress_for_test();
        assert!(!snapshot.unavailable, "snapshot unavailable after Closed");
        snapshot
    }));
    drop(held); // Controller releases the real holder even when the unchanged availability assertion fails.
    result.unwrap_or_else(|panic| std::panic::resume_unwind(panic))''')

def disable_emit(tree):
    function(tree, WAIT, 'fn emit(&self', '        let _ = line; // Only actual emission/flush is disconnected; completion state and work remain live.')

def no_entry(tree):
    function(tree, ISO, 'pub fn assert_once', '        self.checked = true; let _ = pid;')

def page_bypass(tree):
    function(tree, PAGE, 'fn owner_decision', '        let _ = (listener, local, owner); true')

def session_bypass(tree):
    change(tree, SESSION, 'same_account(observed_account(Some(euid)), unsafe { libc::geteuid() })',
           '{ let _ = observed_account(Some(euid)); true }')
    change(tree, SESSION, 'same_account(observed_account(creds.euid()), own)',
           '{ let _ = observed_account(creds.euid()); true }')

def private_bypass(tree):
    # Metadata prerequisite and identical test assertions remain. This test-only seam removes just live protection
    # before the native foreign open; it does not change credentials, the open result or any assertion.
    text = read(tree, UNIX)
    at = text.index('    let (command, entry)', text.index('fn observe_foreign_denial'))
    bypass = '''    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path.parent().unwrap(), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666)).unwrap();
'''
    write(tree, UNIX, text[:at] + bypass + text[at:])

def pipe_bypass(tree):
    # The shared pipe descriptor owner creates both the session pipe and every capsule instance.
    path = 'rust/log/src/host/winsec.rs'
    text = read(tree, path)
    # Change only the pipe flavor's grant, preserving directory descriptors.
    anchor = 'owner_protected_descriptor_with_ace("", "FA")'
    if anchor not in text:
        raise RuntimeError('pipe descriptor anchor moved')
    write(tree, path, text.replace('let sid_string = token_user_sid_string()?;',
        'let sid_string = if flags.is_empty() { String::from("WD") } else { token_user_sid_string()? };'))


for name, mutate in [
 ('parent-admission', lambda t: None),
 ('parent-busy', lambda t: blocking(t, True)),
 ('parent-deadline', deadline), ('parent-io', separate_io),
 ('parent-output', lambda t: None), ('parent-retention', immediate),
 ('parent-entry', lambda t: None),
 ('parent-iso-deadline', lambda t: change(t, ISO, 'let kind = loop {', 'let deadline = Instant::now() + Duration::from_millis(600);\n    let kind = loop {')),
]:
    tree = parent_overlay(name)
    if name not in ('parent-busy', 'parent-retention'):
        blocking(tree, True)
    if name in ('parent-admission', 'parent-output', 'parent-entry'):
        deadline(tree)
    mutate(tree)
for name, mutate in [
 ('revert-busy', blocking), ('revert-deadline', deadline),
 ('revert-io', separate_io), ('revert-emission', disable_emit),
 ('revert-retention', immediate), ('revert-entry', no_entry),
 ('revert-iso-deadline', lambda t: change(t, ISO, 'let kind = loop {', 'let deadline = Instant::now() + Duration::from_millis(600);\n    let kind = loop {')),
 ('revert-page', page_bypass), ('revert-session', session_bypass),
 ('revert-private', private_bypass), ('revert-pipe', pipe_bypass),
 ('revert-windows-scope', lambda t: change(t, 'rust/log/src/lane/mod.rs',
    '#[cfg(unix)]\npub mod test_progress;', 'pub mod test_progress;')),
]:
    mutate(archive(head, name))
(root / 'manifest.txt').write_text(f'parent={PARENT}\nhead={head}\n'
 'All base trees are git archive copies. Parent-* overlays install identical test bodies and observation seams.\n'
 'Parent recorder layout is a compatibility scaffold; original blocking admission counts no skips.\n'
 'Parent deadline adapters expose original renewed durations and unconditional join-success classification.\n'
 'Parent I/O overlay preserves a separate no-server timeout path. Parent output observes existing emission.\n'
 'Parent retention has the explicitly required nonwaiting-recorder prerequisite overlay, then immediate availability.\n'
 'Admission parent overlays preserve native ownership decisions. Private negative control removes only OS modes after prerequisite observation.\n'
 'The direct ISO deadline parent overlay exposes the original full 600ms fixture wait through the new API; it is a narrower duration-adapter claim.\n'
 'No parent claim is described as unmodified-parent evidence. Revert trees retain other fixes.\n')
print(f'proof trees prepared parent={PARENT} head={head}')
