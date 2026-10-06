"""Prepare archive copies and mechanical negative-control overlays; no source scan is a runtime proof."""
import io
import os
from pathlib import Path
import subprocess
import sys
import tarfile

PARENT = "877eac1cd98a4988967357c6b0d918273d9584f3"
ISO = "rust/log/src/test_isolated.rs"
MAIN = "rust/log/tests/socket_unix/main.rs"
DIAG = "rust/log/tests/socket_unix/diagnostics.rs"
MANIFEST = [ISO, MAIN, DIAG, "rust/log/CLAUDE.md", "rust/log/tests/CLAUDE.md"]
head, destination = sys.argv[1:]
repo = Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip())
root = Path(destination).resolve()
root.mkdir(parents=True, exist_ok=True)

def archive(ref, label):
    tree = root / label
    tree.mkdir(exist_ok=True)
    data = subprocess.check_output(["git", "archive", PARENT if ref == "WORKING" else ref], cwd=repo)
    with tarfile.open(fileobj=io.BytesIO(data)) as content:
        content.extractall(tree, filter="data")
    if ref == "WORKING":
        for path in MANIFEST:
            (tree / path).write_bytes((repo / path).read_bytes())
    print("prepared " + label, flush=True)
    return tree

def read(tree, path):
    return (tree / path).read_text()

def write(tree, path, text):
    (tree / path).write_text(text)

def replace(text, before, after):
    if text.count(before) != 1:
        raise RuntimeError("negative-control anchor moved")
    return text.replace(before, after)

source = archive(head, "head")
base = archive(PARENT, "parent")
iso = read(source, ISO)
main = read(source, MAIN)
diag = read(source, DIAG)

# Reversal N3.2 records the missing normal completion FIRST, then performs ISO recovery on its own child.
# Returned termination describes the pre-recovery observation; the separately printed recovery is the actual exit.
start = iso.index("pub fn supervise_fixture_until")
end = iso.index("/// A child whose piped stdout", start)
scope = iso[start:end]
adapter = replace(scope,
    "    if work.is_err() {\n        drop(draining.child.stdin.take());\n    }",
    """    let bypassed = work.is_err();
    if bypassed {
        eprintln!("proof-observation child={pid} readiness=failed normal-finalization=absent");
        // Only after that missing-finalization observation may the proof controller recover its own child.
        drop(draining.child.stdin.take());
    }""")
adapter = replace(adapter,
    "    let termination = match draining.child.try_wait() {",
    "    let mut termination = match draining.child.try_wait() {")
adapter = replace(adapter, "    FixtureOutcome {",
    """    if bypassed {
        eprintln!("proof-recovery child={pid} cleanup={} entry={}",
            if termination.is_ok() { "confirmed" } else { "unconfirmed" },
            if entry.is_ok() { "once" } else { "failed" });
        termination = Err("readiness failure bypassed owned-child finalization (observed before proof recovery)".into());
    }
    FixtureOutcome {""")
reverse_scope = iso[:start] + adapter + iso[end:]

# Reversal N3.3 restores P's real read_to_string decoder in the pipe reader.
# Mapping the resulting String back to bytes is only a receiver-type adapter for the unchanged scoped API.
reverse_bytes = replace(iso,
    "                let mut bytes = Vec::new();\n                let result = pipe.read_to_end(&mut bytes).map(|_| ());",
    """                let mut decoded = String::new();
                let result = pipe.read_to_string(&mut decoded).map(|_| ());
                let bytes = decoded.into_bytes();""")

# Reversal N3.1 restores P's discarded prerequisite and unconditional available-history validation.
pdiag = read(base, DIAG)
pstart = pdiag.index("fn history(")
pend = pdiag.index("\nfn server(", pstart)
old_history = pdiag[pstart:pend]
new_start = diag.index("fn checked_history(")
new_end = diag.index("\nfn server(", new_start)
old_checked = "fn checked_history(capture: &Captured, _test: &str) { history(capture); }\n"
reverse_diag = diag[:new_start] + old_history + "\n" + old_checked + diag[new_end:]
reverse_main = replace(main, ".prerequisite_history(&server)", ".available_snapshot(&server)") if ".prerequisite_history(&server)" in main else main
reverse_diag = replace(reverse_diag, ".prerequisite_history(&server);", ".available_snapshot(&server);")
# The caller above is in diagnostics; the unchanged new emission owner is deliberately no longer reached.

for label, path, text in [
    ("revert-ready", ISO, reverse_scope),
    ("revert-utf8", ISO, reverse_bytes),
    ("revert-history", DIAG, reverse_diag),
]:
    tree = archive(head, label)
    write(tree, path, text)

# Tests-first P overlays carry identical head test bodies and observations. P's wait/entry implementations are retained.
# The narrow API adapter exposes P's readiness-before-finalization shape and recovers only after observing the bypass.
parent_iso = read(base, ISO)
parent_prefix = parent_iso[:parent_iso.index("/// A child whose piped stdout")]
type_start = iso.index("/// A direct fixture retains")
type_end = iso.index("/// Own the child immediately", type_start)
types = iso[type_start:type_end]
# Keep P's actual duration/deadline wait, command, Entry and isolated rerun code; adapt the old string drain to
# the result-returning output interface needed by mechanical direct-fixture observation.
drain_start = parent_iso.index("/// A child whose piped stdout")
run_start = parent_iso.index("/// In the isolated child:", drain_start)
old_drain = parent_iso[drain_start:run_start]
output_method = """
    fn output_until(&mut self, deadline: Instant) -> Result<(String, String), String> {
        let read = |reader: Option<Receiver<std::io::Result<String>>>, what: &str| {
            let Some(reader) = reader else { return Ok(String::new()); };
            let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1));
            match reader.recv_timeout(left) {
                Ok(result) => result.map_err(|e| format!("reading the child's {what}: {e}")),
                Err(error) => Err(format!("reading the child's {what}: {error}")),
            }
        };
        let out = read(self.out.take(), "stdout");
        let err = read(self.err.take(), "stderr");
        match (out, err) {
            (Ok(out), Ok(err)) => Ok((out, err)),
            (out, err) => Err(format!("stdout: {out:?}; stderr: {err:?}")),
        }
    }
"""
old_drain = replace(old_drain, "impl Draining {", "impl Draining {" + output_method)
test_start = iso.index("#[cfg(test)]\nmod tests {")
parent_test_start = parent_iso.index("#[cfg(test)]\nmod tests {")
parent_adapted = parent_prefix + types + adapter + old_drain + parent_iso[run_start:parent_test_start] + iso[test_start:]
for label in ["parent-ready", "parent-history"]:
    tree = archive(PARENT, label)
    write(tree, ISO, parent_adapted)
    write(tree, MAIN, main)
    write(tree, DIAG, reverse_diag if label == "parent-history" else diag)

# The original tests-first UTF8 overlay changes only the role bytes and its pipe-path test at P.
tree = archive(PARENT, "parent-utf8")
patch = Path(__file__).with_name("tests-first-utf8.patch").read_bytes()
subprocess.run(["git", "apply", "-"], input=patch, cwd=tree,
               env={**os.environ, "GIT_CEILING_DIRECTORIES": str(tree.parent)}, check=True)
# Whitespace-only formatting of the identical final outer test is immaterial to its byte/status oracle.
(root / "manifest.txt").write_text(
    "parent=" + PARENT + "\nhead=" + head + "\n"
    "parent-utf8: tests-first byte role and identical pipe oracle over P production.\n"
    "parent-ready: identical fixtures; API observation adapter records bypass before ISO recovery.\n"
    "parent-history: identical busy/poisoned fixtures; P history/discard path and wait/entry/decoder owners.\n"
    "revert-history: only history emission/validation reversed; readiness/bytes retained.\n"
    "revert-ready: only normal failure finalization reversed; bytes/history retained; recovery follows observation.\n"
    "revert-utf8: only pipe byte capture reversed to P decoder; readiness/history retained.\n")
