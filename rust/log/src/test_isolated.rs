//! Test-only (feature `test-support`): the one way a test re-runs its own binary for one named test, and waits on it
//! within a bound. A child that did not enter the named test's body fails its parent, so a name that matches no test
//! cannot pass, and an entry record dropped unchecked fails too. Every wait here is bounded: a child that overruns is
//! killed, and output that a descendant still holds open after the child's exit fails the caller at the bound. What
//! this does not prove: that a killed child's descendants are gone; a kill whose exit is not seen within 5 s is
//! reported as unconfirmed.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
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
        assert_eq!(
            record,
            format!("{} {pid}\n", self.name),
            "isolated body entered other than once: {}",
            self.name
        );
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
    let path = std::env::temp_dir().join(format!(
        "sot-entered-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&path);
    let mut command = Command::new(std::env::current_exe().expect("current_exe"));
    command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env_remove(CHILD)
        .env(ENTERED, &path);
    (
        command,
        Entry {
            name: test_name.to_string(),
            path,
            checked: false,
        },
    )
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

/// Why a child work wait failed; cleanup has its separate five-second confirmation bound.
#[derive(Debug)]
pub enum ChildWaitKind {
    Expired,
    Poll(std::io::Error),
}

#[derive(Debug)]
pub struct ChildWaitError {
    pub kind: ChildWaitKind,
    pub child: u32,
    pub termination_confirmed: bool,
}

impl std::fmt::Display for ChildWaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ChildWaitKind::Expired => write!(
                f,
                "the child test did not complete within its absolute work deadline"
            )?,
            ChildWaitKind::Poll(error) => write!(f, "the child test could not be polled: {error}")?,
        }
        write!(
            f,
            "; child={} cleanup={}",
            self.child,
            if self.termination_confirmed {
                "confirmed"
            } else {
                "unconfirmed"
            }
        )
    }
}
impl std::error::Error for ChildWaitError {}

/// Waits against the caller's existing work deadline. Only this owner ends/reaps its recorded child.
pub fn wait_until(child: &mut Child, deadline: Instant) -> Result<ExitStatus, ChildWaitError> {
    let kind = loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(50)),
                );
            }
            Ok(None) => break ChildWaitKind::Expired,
            Err(error) => break ChildWaitKind::Poll(error),
        }
    };
    let _ = child.kill();
    // Cleanup confirmation is separate from the spent work deadline.
    let cleanup = Instant::now() + Duration::from_secs(5);
    let termination_confirmed = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if Instant::now() < cleanup => std::thread::sleep(Duration::from_millis(50)),
            _ => break false,
        }
    };
    Err(ChildWaitError {
        kind,
        child: child.id(),
        termination_confirmed,
    })
}

/// Compatible duration wrapper; computes its work deadline once.
pub fn wait_within(child: &mut Child, bound: Duration) -> ExitStatus {
    wait_until(child, Instant::now() + bound).unwrap_or_else(|error| panic!("{error}"))
}

/// A child whose piped stdout and stderr are read on threads of their own, so a child that writes more than a pipe
/// holds is never stalled by its parent. Each reader sends what it read once its pipe ends.
pub struct Draining {
    child: Child,
    out: Option<Receiver<std::io::Result<String>>>,
    err: Option<Receiver<std::io::Result<String>>>,
}

/// Starts reading `child`'s piped stdout and stderr (either may be absent) on threads of their own.
pub fn drain(mut child: Child) -> Draining {
    fn reader(
        pipe: Option<impl Read + Send + 'static>,
    ) -> Option<Receiver<std::io::Result<String>>> {
        pipe.map(|mut pipe| {
            let (sent, read) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = sent.send(pipe.read_to_string(&mut text).map(|_| text));
            });
            read
        })
    }
    let out = reader(child.stdout.take());
    let err = reader(child.stderr.take());
    Draining { child, out, err }
}

impl Draining {
    /// Waits for the child within `bound` ([`wait_within`]), then for its output to end within what remains of that
    /// bound (and at least 1 s, so a child that exits at the bound's edge is not failed for its readers' last step).
    /// It returns the child's status, stdout and stderr. A failed read fails the caller. So does output still open at
    /// the deadline, which a descendant that inherited the pipe can hold after the child's exit; that reader is left to
    /// end when the descendant does.
    pub fn wait_within(mut self, bound: Duration) -> (ExitStatus, String, String) {
        let deadline = Instant::now() + bound;
        let status =
            wait_until(&mut self.child, deadline).unwrap_or_else(|error| panic!("{error}"));
        let text = |reader: Option<Receiver<std::io::Result<String>>>, what: &str| {
            reader.map_or_else(String::new, |reader| {
                let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1));
                match reader.recv_timeout(left) {
                    Ok(read) => read.unwrap_or_else(|e| panic!("reading the child's {what}: {e}")),
                    Err(RecvTimeoutError::Timeout) => panic!(
                        "the child exited but its {what} did not end within {bound:?} (a descendant may hold it)"
                    ),
                    Err(RecvTimeoutError::Disconnected) => panic!("the reader of the child's {what} ended without a result"),
                }
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
    run_isolated_until(test_name, Instant::now() + ISOLATION_TIMEOUT)
        .unwrap_or_else(|error| panic!("{error}"))
}

/// Exact-body isolation spending an existing absolute deadline. A successful parent returns `Ok(false)`.
pub fn run_isolated_until(test_name: &str, deadline: Instant) -> Result<bool, ChildWaitError> {
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        enter(test_name);
        return Ok(true);
    }
    let (mut command, entry) = test_command(test_name);
    let mut child = command
        .env(CHILD, test_name)
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn the isolated test");
    let result = wait_until(&mut child, deadline);
    entry.assert_once(child.id());
    let status = result?;
    assert!(
        status.success(),
        "isolated test {test_name} failed in its child process: {status}"
    );
    Ok(false)
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
        assert!(
            wait_within(&mut child, ISOLATION_TIMEOUT).success(),
            "a zero-test child exits 0"
        );
        let pid = child.id();
        assert_fails_with("isolated body did not enter", || entry.assert_once(pid));
    }

    /// The check is not optional: an entry record dropped unchecked fails.
    #[test]
    fn an_entry_dropped_unchecked_fails() {
        let (_command, entry) =
            test_command("test_isolated::tests::a_real_body_enters_exactly_once");
        assert_fails_with("dropped without assert_once", || drop(entry));
    }

    /// The child the drain and bound tests re-run; in an ordinary run, nothing.
    #[test]
    fn a_role() {
        let Ok(role) = std::env::var(ROLE) else {
            return;
        };
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
            // Leaves a descendant that holds this child's stdin and stdout, and exits at once.
            "leave" => {
                Command::new(std::env::current_exe().expect("current_exe"))
                    .args([
                        "--exact",
                        "test_isolated::tests::a_role",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(ROLE, "hold")
                    .env_remove(ENTERED)
                    .spawn()
                    .expect("spawn the descendant");
            }
            // The descendant: holds its inherited stdout until its inherited stdin ends.
            "hold" => {
                let _ = std::io::stdin().read_to_end(&mut Vec::new());
            }
            other => panic!("unknown role {other}"),
        }
    }

    /// A child that writes more than its pipe holds is drained while it runs, so it finishes and is read whole.
    #[test]
    fn a_child_that_fills_its_pipe_is_drained_and_finishes() {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let child = command
            .env(ROLE, "flood")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let (status, stdout, _) = drain(child).wait_within(ISOLATION_TIMEOUT);
        assert!(status.success(), "{status}");
        assert!(
            stdout.len() >= 64 * 4096,
            "the flood was not read whole: {} bytes",
            stdout.len()
        );
        entry.assert_once(pid);
    }

    /// A child that never ends fails its parent at the bound, by name, and is killed.
    #[test]
    fn a_stalled_child_fails_at_its_bound() {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let child = command
            .env(ROLE, "stall")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        let draining = drain(child);
        // The bound is the stall's: a child scheduled late is not killed before it enters.
        wait_for_entry(&entry);
        let started = Instant::now();
        assert_fails_with("did not complete within", move || {
            draining.wait_within(Duration::from_secs(5));
        });
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the bound was not kept: {:?}",
            started.elapsed()
        );
        entry.assert_once(pid);
    }

    /// A child that exits but leaves a descendant holding its stdout fails at the bound, by name, instead of hanging.
    #[test]
    fn output_a_descendant_holds_fails_at_the_bound() {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let mut child = command
            .env(ROLE, "leave")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        // The descendant holds stdout until this input ends.
        let input = child.stdin.take().expect("piped stdin");
        let draining = drain(child);
        wait_for_entry(&entry);
        let started = Instant::now();
        assert_fails_with("did not end within", move || {
            draining.wait_within(Duration::from_secs(5));
        });
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the bound was not kept: {:?}",
            started.elapsed()
        );
        drop(input);
        entry.assert_once(pid);
    }

    fn assert_fails_with(text: &str, check: impl FnOnce()) {
        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check))
            .expect_err("a check that must fail passed");
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .unwrap_or("");
        assert!(message.contains(text), "{message}");
    }

    /// Waits, within `ISOLATION_TIMEOUT`, until the child's body has entered, so the bound a test then times is its own.
    fn wait_for_entry(entry: &Entry) {
        let deadline = Instant::now() + ISOLATION_TIMEOUT;
        while std::fs::read_to_string(&entry.path)
            .unwrap_or_default()
            .is_empty()
        {
            assert!(
                Instant::now() < deadline,
                "{} did not enter within {ISOLATION_TIMEOUT:?}",
                entry.name
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // Body entry and child cleanup are behavioral proofs; the rerun spelling catalog is retired.

    #[test]
    fn wait_until_retains_a_spent_deadline_and_confirms_expiry() {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let mut child = command
            .env(ROLE, "stall")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        wait_for_entry(&entry);
        let started = Instant::now();
        let deadline = started + Duration::from_millis(600);
        std::thread::sleep(Duration::from_millis(400));
        let error = wait_until(&mut child, deadline).expect_err("stalled child must expire");
        entry.assert_once(pid);
        assert!(matches!(error.kind, ChildWaitKind::Expired));
        assert!(
            error.termination_confirmed,
            "owned child cleanup unconfirmed"
        );
        eprintln!("body-proof test=test_isolated::tests::a_role child={pid} bodies=1 completed=false cleanup=confirmed");
        assert!(
            started.elapsed() < Duration::from_millis(950),
            "child deadline was recomputed"
        );
    }

    #[test]
    fn run_isolated_until_reports_successful_parent_false() {
        let child_body = run_isolated_until(
            "test_isolated::tests::run_isolated_until_reports_successful_parent_false",
            Instant::now() + ISOLATION_TIMEOUT,
        )
        .unwrap();
        if child_body {
            return;
        }
        assert!(!child_body, "successful ISO parent must be false");
    }
}
