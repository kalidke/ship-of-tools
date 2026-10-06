//! Test-only (feature `test-support`): the one way a test re-runs its own binary for one named test, and waits on it
//! within a bound. A child that did not enter the named test's body fails its parent, so a name that matches no test
//! cannot pass, and an entry record dropped unchecked fails too. What this does not prove: that a killed child's
//! descendants are gone, or that its pipes reached their end; a kill whose exit is not seen within 5 s is reported as
//! unconfirmed.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The parent's wall-clock bound on one isolated test.
pub const ISOLATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Set, to the test's name, only in a `run_isolated` child.
const CHILD: &str = "SOT_TEST_ISOLATED_CHILD";
/// The record a re-run test's body appends `<name> <pid>` to as it starts.
const ENTERED: &str = "SOT_TEST_ISOLATED_ENTERED";

static NEXT: AtomicU32 = AtomicU32::new(0);

/// The record one re-run child's body writes as it enters. It must be checked with [`Entry::assert_once`]: dropping it
/// unchecked fails the test, unless the test is already failing.
pub struct Entry {
    name: String,
    path: PathBuf,
    checked: bool,
}

impl Entry {
    /// Fails unless the child `pid` entered this test's body exactly once.
    pub fn assert_once(mut self, pid: u32) {
        self.checked = true;
        let record = std::fs::read_to_string(&self.path).unwrap_or_default();
        assert!(
            !record.is_empty(),
            "isolated body did not enter: {} ran no test body in its child (is that its exact libtest name?)",
            self.name
        );
        assert_eq!(record, format!("{} {pid}\n", self.name), "isolated body entered other than once: {}", self.name);
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if !self.checked && !std::thread::panicking() {
            panic!("the entry record for {} was dropped without assert_once: its body was never checked", self.name);
        }
    }
}

/// This test binary, set to run exactly `test_name` (`--exact`, one thread, output not captured), and the record its
/// body's [`enter`] writes to.
pub fn test_command(test_name: &str) -> (Command, Entry) {
    let path = std::env::temp_dir().join(format!("sot-entered-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_file(&path);
    let mut command = Command::new(std::env::current_exe().expect("current_exe"));
    command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env_remove(CHILD)
        .env(ENTERED, &path);
    (command, Entry { name: test_name.to_string(), path, checked: false })
}

/// In a child [`test_command`] started: records that `test_name`'s body entered. A no-op in an ordinary run.
pub fn enter(test_name: &str) {
    if let Some(path) = std::env::var_os(ENTERED) {
        let mut record = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open the entry record");
        writeln!(record, "{test_name} {}", std::process::id()).expect("record the body's entry");
    }
}

/// Waits for `child` until `bound`. A child still running then, or one that cannot be polled, is killed, its exit is
/// awaited for at most 5 s, and the caller fails, saying when that exit went unconfirmed.
pub fn wait_within(child: &mut Child, bound: Duration) -> ExitStatus {
    let deadline = Instant::now() + bound;
    let why = loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => break format!("did not complete within {bound:?}"),
            Err(e) => break format!("could not be polled: {e}"),
        }
    };
    let _ = child.kill();
    let end = Instant::now() + Duration::from_secs(5);
    let confirmed = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(50)),
            _ => break false,
        }
    };
    panic!("the child test {why}{}", if confirmed { "" } else { "; its termination is unconfirmed" });
}

/// A child whose piped stdout and stderr are read on threads of their own, so a child that writes more than a pipe
/// holds is never stalled by its parent.
pub struct Draining {
    child: Child,
    out: Option<JoinHandle<std::io::Result<String>>>,
    err: Option<JoinHandle<std::io::Result<String>>>,
}

/// Starts reading `child`'s piped stdout and stderr (either may be absent) on threads of their own.
pub fn drain(mut child: Child) -> Draining {
    fn reader(pipe: Option<impl Read + Send + 'static>) -> Option<JoinHandle<std::io::Result<String>>> {
        pipe.map(|mut pipe| {
            std::thread::spawn(move || {
                let mut text = String::new();
                pipe.read_to_string(&mut text).map(|_| text)
            })
        })
    }
    let out = reader(child.stdout.take());
    let err = reader(child.stderr.take());
    Draining { child, out, err }
}

impl Draining {
    /// Waits for the child within `bound` ([`wait_within`]), then returns its status, stdout and stderr. A failed read
    /// fails the caller. The readers end when the child's pipes close, which for a child that starts no process of its
    /// own is its exit.
    pub fn wait_within(mut self, bound: Duration) -> (ExitStatus, String, String) {
        let status = wait_within(&mut self.child, bound);
        let text = |reader: Option<JoinHandle<std::io::Result<String>>>, what: &str| {
            reader.map_or_else(String::new, |reader| {
                reader
                    .join()
                    .expect("a pipe reader panicked")
                    .unwrap_or_else(|e| panic!("reading the child's {what}: {e}"))
            })
        };
        let out = text(self.out.take(), "stdout");
        let err = text(self.err.take(), "stderr");
        (status, out, err)
    }
}

/// In the isolated child: records the body's entry and returns `true`, so the caller runs its body. In the parent:
/// runs `test_name` as a child within [`ISOLATION_TIMEOUT`] and returns `false` once it exited 0 having entered the
/// body exactly once. A failed or overrunning child, and a child whose body did not enter, fail the calling test.
pub fn run_isolated(test_name: &str) -> bool {
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        enter(test_name);
        return true;
    }
    let (mut command, entry) = test_command(test_name);
    let mut child = command.env(CHILD, test_name).stdin(Stdio::null()).spawn().expect("spawn the isolated test");
    let status = wait_within(&mut child, ISOLATION_TIMEOUT);
    assert!(status.success(), "isolated test {test_name} failed in its child process: {status}");
    entry.assert_once(child.id());
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which job `a_role` does in a child; unset in an ordinary run.
    const ROLE: &str = "SOT_TEST_ISOLATED_ROLE";

    /// The control: a real body, selected by its exact name, enters exactly once, and the test passes.
    #[test]
    fn a_real_body_enters_exactly_once() {
        if !run_isolated("test_isolated::tests::a_real_body_enters_exactly_once") {
            return;
        }
    }

    /// A name that is not the exact libtest name runs no test, and its child exits 0: that fails, never passes.
    #[test]
    fn an_unqualified_name_fails_as_a_body_that_did_not_enter() {
        assert_fails_with("isolated body did not enter", || {
            run_isolated("a_real_body_enters_exactly_once");
        });
    }

    /// The same for a misspelled qualified name.
    #[test]
    fn a_misspelled_name_fails_as_a_body_that_did_not_enter() {
        assert_fails_with("isolated body did not enter", || {
            run_isolated("test_isolated::tests::a_real_body_enters_exactly_onse");
        });
    }

    /// The same for a role a parent starts with `test_command`: a child that exited 0 having run no test fails.
    #[test]
    fn a_role_that_selects_no_test_fails_as_a_body_that_did_not_enter() {
        let (mut command, entry) = test_command("test_isolated::tests::no_such_role");
        let mut child = command.stdin(Stdio::null()).spawn().expect("spawn");
        assert!(wait_within(&mut child, ISOLATION_TIMEOUT).success(), "a zero-test child exits 0");
        let pid = child.id();
        assert_fails_with("isolated body did not enter", || entry.assert_once(pid));
    }

    /// The check is not optional: an entry record dropped unchecked fails.
    #[test]
    fn an_entry_dropped_unchecked_fails() {
        let (_command, entry) = test_command("test_isolated::tests::a_real_body_enters_exactly_once");
        assert_fails_with("dropped without assert_once", || drop(entry));
    }

    /// The child the drain and bound tests re-run; in an ordinary run, nothing.
    #[test]
    fn a_role() {
        let Ok(role) = std::env::var(ROLE) else { return };
        enter("test_isolated::tests::a_role");
        match role.as_str() {
            // More than any platform's pipe holds (64 KiB on Linux and macOS, about 4 KiB on Windows), then exit.
            "flood" => {
                let mut out = std::io::stdout().lock();
                for _ in 0..64 {
                    out.write_all(&[b'x'; 4096]).expect("write the flood");
                }
            }
            "stall" => std::thread::sleep(Duration::from_secs(120)),
            other => panic!("unknown role {other}"),
        }
    }

    /// A child that writes more than its pipe holds is drained while it runs, so it finishes and is read whole.
    #[test]
    fn a_child_that_fills_its_pipe_is_drained_and_finishes() {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let child = command.env(ROLE, "flood").stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn");
        let pid = child.id();
        let (status, stdout, _) = drain(child).wait_within(ISOLATION_TIMEOUT);
        assert!(status.success(), "{status}");
        assert!(stdout.len() >= 64 * 4096, "the flood was not read whole: {} bytes", stdout.len());
        entry.assert_once(pid);
    }

    /// A child that never ends fails its parent at the bound, by name, and is killed.
    #[test]
    fn a_stalled_child_fails_at_its_bound() {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let child = command.env(ROLE, "stall").stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn");
        let pid = child.id();
        let started = Instant::now();
        assert_fails_with("did not complete within", move || {
            drain(child).wait_within(Duration::from_secs(5));
        });
        assert!(started.elapsed() < Duration::from_secs(15), "the bound was not kept: {:?}", started.elapsed());
        entry.assert_once(pid);
    }

    fn assert_fails_with(text: &str, check: impl FnOnce()) {
        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check))
            .expect_err("a check that must fail passed");
        let message = payload.downcast_ref::<String>().map(String::as_str).unwrap_or("");
        assert!(message.contains(text), "{message}");
    }

    /// The class pin: a test re-runs a test binary by name only through this module, or at a site listed here with
    /// its own proof that the selected body ran. No other Rust source spells libtest's `--exact`. A lexical check: a
    /// re-run that selects by substring without `--exact`, or builds the flag at run time, is outside it.
    #[test]
    fn no_test_reruns_a_binary_outside_this_module() {
        const LISTED: &[(&str, usize, &str)] = &[
            ("rust/backend/src/lifecycle/child_signal.rs", 2, "each scenario's exact \"test <name> ... ok\" line"),
            ("rust/backend/tests/comm_file.rs", 1, "runs through bash for `ulimit -f`; its \"FAILED the append failed: \" line"),
            ("rust/backend/tests/window_lease/main.rs", 1, "the window's bounded `LEASE ` line"),
            ("rust/log/src/host/lock.rs", 1, "the daemon role's \"1 passed\" and the supervisor role's `alive` file"),
            ("rust/log/src/identity/peer_owner/mod.rs", 1, "the child's `connected` line"),
            ("rust/log/src/supervisor/journal/fence.rs", 1, "each racer's ready file, \"1 passed\" and report file"),
            ("rust/log/tests/macos_kernel_facts/peertoken.rs", 1, "the client's report line"),
            ("rust/log/tests/challenge_macos.rs", 1, "runs as root through `sudo -n`; its pid-bearing readiness record, a skip failing on CI"),
        ];
        let flag = concat!("\"--", "exact\"");
        let mut found = Vec::new();
        for (rel, text) in crate::test_scan::rust_sources() {
            if rel == "rust/log/src/test_isolated.rs" {
                continue;
            }
            let count = text.matches(flag).count();
            let room = LISTED.iter().find(|(f, _, _)| *f == rel).map_or(0, |(_, n, _)| *n);
            if count != room {
                found.push(format!("{rel}: {count} (listed {room})"));
            }
        }
        assert!(found.is_empty(), "a test re-runs a binary by name outside sot_log::test_isolated:\n{}", found.join("\n"));
    }
}
