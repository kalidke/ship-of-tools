//! Test-only (feature `test-support`): the one way a test re-runs its own binary for one named test, and waits on it
//! within a bound. A child that did not enter the named test's body fails its parent, so a name that matches no test
//! cannot pass, and an entry record dropped unchecked fails too. Every wait here is bounded: a child that overruns is
//! killed, and output that a descendant still holds open after the child's exit fails the caller at the bound. What
//! this does not prove: that a killed child's descendants are gone; a kill whose exit is not seen within 5 s is
//! reported as unconfirmed. Direct fixtures retain readiness failures until owned-child/entry checks finish.
//! Both output streams are captured as bytes; invalid UTF-8 is rendered as explicit uppercase byte escapes.

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

/// A direct fixture retains its primary failure until child and entry checks finish.
#[derive(Debug)]
pub enum FixtureFailure {
    Timeout(String),
    Error(String),
    Panic(String),
}

impl FixtureFailure {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Timeout(_) => "timeout",
            Self::Error(_) => "error",
            Self::Panic(_) => "panic",
        }
    }
}

impl std::fmt::Display for FixtureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Timeout(text) | Self::Error(text) | Self::Panic(text)) = self;
        f.write_str(text)
    }
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        text.to_string()
    } else {
        "fixture panicked with a non-text payload".into()
    }
}

/// All mandatory observations, including secondary failures, survive a readiness/work failure.
pub struct FixtureOutcome<T> {
    pub child: u32,
    pub work: Result<T, FixtureFailure>,
    pub wait: Result<ExitStatus, ChildWaitError>,
    pub termination: Result<(), String>,
    pub entry: Result<(), String>,
    pub output: Result<(String, String), String>,
}

impl<T> FixtureOutcome<T> {
    /// Report actual outcomes; callers raise retained failures only after inspecting this result.
    pub fn report(&self, test: &str) {
        eprintln!(
            "fixture-finalization test={test} child={} readiness={} wait={} cleanup={} entry={}",
            self.child,
            self.work.as_ref().err().map_or("ok", FixtureFailure::kind),
            match &self.wait {
                Ok(_) => "exited",
                Err(e) if matches!(e.kind, ChildWaitKind::Expired) => "expired",
                Err(_) => "error",
            },
            if self.termination.is_ok() {
                "confirmed"
            } else {
                "unconfirmed"
            },
            if self.entry.is_ok() { "once" } else { "failed" }
        );
        if let Err(error) = &self.work {
            eprintln!("fixture-readiness error={error}");
        }
        if let Err(error) = &self.wait {
            eprintln!("fixture-wait error={error}");
        }
        for (phase, result) in [("termination", &self.termination), ("entry", &self.entry)] {
            if let Err(error) = result {
                eprintln!("fixture-{phase} error={error}");
            }
        }
        if let Err(error) = &self.output {
            eprintln!("fixture-output error={error}");
        }
    }
}

/// Own the child immediately after spawn. Catch work panics, close input on failure, then attempt waiting,
/// independent termination confirmation, exact entry and both output checks before returning any failure.
/// Work may establish a separately named work deadline after successful readiness; failure keeps the original
/// absolute deadline. This confirms only the recorded child, not its descendants or held output's owner.
pub fn supervise_fixture_until<T>(
    child: Child,
    entry: Entry,
    mut deadline: Instant,
    work: impl FnOnce(&mut Child, &Entry, &mut Instant) -> Result<T, FixtureFailure>,
) -> FixtureOutcome<T> {
    let mut draining = drain(child);
    let pid = draining.child.id();
    let work = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        work(&mut draining.child, &entry, &mut deadline)
    }))
    .unwrap_or_else(|panic| Err(FixtureFailure::Panic(panic_text(panic))));
    if work.is_err() {
        drop(draining.child.stdin.take());
    }
    let wait = wait_until(&mut draining.child, deadline);
    let termination = match draining.child.try_wait() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err("owned child termination unconfirmed".into()),
        Err(error) => Err(format!("confirming owned child termination: {error}")),
    };
    let entry = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry.assert_once(pid)))
        .map_err(panic_text);
    let output = draining.output_until(deadline);
    FixtureOutcome {
        child: pid,
        work,
        wait,
        termination,
        entry,
        output,
    }
}

/// A child whose piped stdout and stderr are read on threads of their own, so a child that writes more than a pipe
/// holds is never stalled by its parent. Each reader sends what it read once its pipe ends.
pub struct Draining {
    child: Child,
    out: Option<Receiver<(Vec<u8>, std::io::Result<()>)>>,
    err: Option<Receiver<(Vec<u8>, std::io::Result<()>)>>,
}

/// Starts reading `child`'s piped stdout and stderr (either may be absent) on threads of their own.
pub fn drain(mut child: Child) -> Draining {
    fn reader(
        pipe: Option<impl Read + Send + 'static>,
    ) -> Option<Receiver<(Vec<u8>, std::io::Result<()>)>> {
        pipe.map(|mut pipe| {
            let (sent, read) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = pipe.read_to_end(&mut bytes).map(|_| ());
                let _ = sent.send((bytes, result));
            });
            read
        })
    }
    let out = reader(child.stdout.take());
    let err = reader(child.stderr.take());
    Draining { child, out, err }
}

impl Draining {
    fn output_until(&mut self, deadline: Instant) -> Result<(String, String), String> {
        let out = read_output(self.out.take(), deadline, "stdout");
        let err = read_output(self.err.take(), deadline, "stderr");
        match (out, err) {
            (Ok(out), Ok(err)) => Ok((out, err)),
            (out, err) => Err(format!("stdout: {out:?}; stderr: {err:?}")),
        }
    }
    /// Waits for the child within `bound` ([`wait_within`]), then for its output to end within what remains of that
    /// bound (and at least 1 s, so a child that exits at the bound's edge is not failed for its readers' last step).
    /// It returns the child's status, stdout and stderr. A failed read fails the caller. So does output still open at
    /// the deadline, which a descendant that inherited the pipe can hold after the child's exit; that reader is left to
    /// end when the descendant does.
    pub fn wait_within(mut self, bound: Duration) -> (ExitStatus, String, String) {
        let deadline = Instant::now() + bound;
        let status =
            wait_until(&mut self.child, deadline).unwrap_or_else(|error| panic!("{error}"));
        let (out, err) = self
            .output_until(deadline)
            .unwrap_or_else(|error| panic!("{error}"));
        (status, out, err)
    }
}

fn read_output(
    reader: Option<Receiver<(Vec<u8>, std::io::Result<()>)>>,
    deadline: Instant,
    what: &str,
) -> Result<String, String> {
    let Some(reader) = reader else {
        return Ok(String::new());
    };
    // Preserve the existing separately bounded terminal-output allowance.
    let left = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_secs(1));
    match reader.recv_timeout(left) {
        Ok((bytes, result)) => {
            let text = render_bytes(&bytes);
            result.map(|()| text.clone()).map_err(|error| {
                format!("reading the child's {what}: {error}; captured={text}")
            })
        }
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "the child exited but its {what} did not end within the output deadline (a descendant may hold it)"
        )),
        Err(RecvTimeoutError::Disconnected) => Err(format!("the reader of the child's {what} ended without a result")),
    }
}

/// Keep valid UTF-8 spans and escape every invalid byte, including an incomplete final sequence.
fn render_bytes(mut bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(valid) => {
                text.push_str(valid);
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                text.push_str(std::str::from_utf8(&bytes[..valid]).expect("validated span"));
                let invalid = error.error_len().unwrap_or(bytes.len() - valid);
                for byte in &bytes[valid..valid + invalid] {
                    write!(text, "\\x{byte:02X}").expect("string write");
                }
                bytes = &bytes[valid + invalid..];
            }
        }
    }
    text
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
        if role != "zero-release" {
            enter("test_isolated::tests::a_role");
        }
        if let Some(path) = std::env::var_os("SOT_TEST_FIXTURE_STARTED") {
            std::fs::write(path, b"started\n").unwrap();
        }
        match role.as_str() {
            // More than any platform's pipe holds (64 KiB on Linux and macOS, about 4 KiB on Windows), then exit.
            "flood" => {
                let mut out = std::io::stdout().lock();
                for _ in 0..64 {
                    out.write_all(&[b'x'; 4096]).expect("write the flood");
                }
            }
            "stall" => std::thread::sleep(Duration::from_secs(120)),
            "bytes" => {
                let mut out = std::io::stdout().lock();
                out.write_all(b"valid stdout control\n").unwrap();
                out.flush().unwrap();
                let mut err = std::io::stderr().lock();
                err.write_all(b"intended child diagnostic \xFF\xFE\xC3(\n")
                    .unwrap();
                err.flush().unwrap();
                panic!("intended child failure");
            }
            "byte-controls" => {
                let mut out = std::io::stdout().lock();
                for bytes in [b"\xE2".as_slice(), b"\x82\xAC\n", b"\x80\xC3(\xE2\x82"] {
                    out.write_all(bytes).unwrap();
                    out.flush().unwrap();
                }
            }
            "release" | "zero-release" => {
                eprintln!("fixture-start observed");
                std::io::stderr().flush().unwrap();
                std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
            }
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
        let mut started = None;
        let outcome = supervise_fixture_until(
            child,
            entry,
            Instant::now() + ISOLATION_TIMEOUT,
            |child, entry, deadline| {
                observe_entry(entry, child.id(), *deadline)?;
                let origin = Instant::now();
                started = Some(origin);
                *deadline = origin + Duration::from_secs(5);
                Ok(())
            },
        );
        outcome.report("test_isolated::tests::a_role");
        assert!(outcome.work.is_ok() && outcome.entry.is_ok() && outcome.termination.is_ok());
        assert!(matches!(
            outcome.wait.unwrap_err().kind,
            ChildWaitKind::Expired
        ));
        assert!(
            started.unwrap().elapsed() < Duration::from_secs(15),
            "the bound was not kept"
        );
    }

    /// A held output stream fails after the child has exited; entry/end checks still finish.
    #[test]
    fn output_a_descendant_holds_fails_at_the_bound() {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let child = command
            .env(ROLE, "leave")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn");
        let mut input = None;
        let mut started = None;
        let outcome = supervise_fixture_until(
            child,
            entry,
            Instant::now() + ISOLATION_TIMEOUT,
            |child, entry, deadline| {
                observe_entry(entry, child.id(), *deadline)?;
                input = child.stdin.take();
                let origin = Instant::now();
                started = Some(origin);
                *deadline = origin + Duration::from_secs(5);
                Ok(())
            },
        );
        drop(input);
        outcome.report("test_isolated::tests::a_role");
        assert!(outcome.work.is_ok() && outcome.entry.is_ok() && outcome.termination.is_ok());
        assert!(outcome.wait.unwrap().success());
        assert!(outcome.output.unwrap_err().contains("did not end within"));
        assert!(
            started.unwrap().elapsed() < Duration::from_secs(15),
            "the bound was not kept"
        );
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

    fn observe_entry(entry: &Entry, pid: u32, deadline: Instant) -> Result<(), FixtureFailure> {
        loop {
            let record = match std::fs::read_to_string(&entry.path) {
                Ok(record) => record,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(FixtureFailure::Error(error.to_string())),
            };
            if record == format!("{} {pid}\n", entry.name) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(FixtureFailure::Timeout(
                    "complete entry readiness record missing".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Start is observable independently of the withheld readiness marker.
    fn observe_start(path: &std::path::Path, deadline: Instant) -> Result<(), FixtureFailure> {
        loop {
            let bytes = std::fs::read(path).map_err(|e| FixtureFailure::Error(e.to_string()))?;
            if bytes == b"started\n" {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(FixtureFailure::Timeout("fixture start missing".into()));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn readiness_failure_finishes_owned_child_checks() {
        for (failure, role) in [
            ("timeout", "release"),
            ("error", "release"),
            ("panic", "release"),
            ("timeout", "stall"),
            ("timeout", "zero-release"),
        ] {
            let started = tempfile::NamedTempFile::new().unwrap();
            let (mut command, entry) = test_command("test_isolated::tests::a_role");
            let child = command
                .env(ROLE, role)
                .env("SOT_TEST_FIXTURE_STARTED", started.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let pid = child.id();
            let deadline = Instant::now() + ISOLATION_TIMEOUT;
            let outcome: FixtureOutcome<()> =
                supervise_fixture_until(child, entry, deadline, |_, _, end| {
                    observe_start(started.path(), *end)?;
                    eprintln!("fixture-start child={pid} observed=true");
                    let readiness_end = Instant::now() + Duration::from_millis(150);
                    *end = readiness_end + Duration::from_millis(600);
                    match failure {
                        "panic" => panic!("deliberate readiness panic"),
                        "error" => {
                            let error = std::fs::read(started.path().with_extension("missing"))
                                .unwrap_err();
                            Err(FixtureFailure::Error(format!(
                                "readiness read failed: {}",
                                error.kind()
                            )))
                        }
                        _ => {
                            while Instant::now() < readiness_end {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(FixtureFailure::Timeout(
                                "expected readiness marker missing".into(),
                            ))
                        }
                    }
                });
            outcome.report("test_isolated::tests::a_role");
            assert!(outcome.work.is_err(), "readiness failure not observed");
            assert!(
                outcome.termination.is_ok(),
                "readiness failure bypassed owned-child finalization"
            );
            if role == "zero-release" {
                assert!(outcome
                    .entry
                    .as_ref()
                    .unwrap_err()
                    .contains("isolated body did not enter"));
            } else {
                assert!(
                    outcome.entry.is_ok(),
                    "readiness failure bypassed owned-child finalization"
                );
            }
            if role == "stall" {
                assert!(matches!(
                    outcome.wait.unwrap_err().kind,
                    ChildWaitKind::Expired
                ));
            } else {
                assert!(outcome.wait.unwrap().success());
            }
        }
    }

    #[test]
    fn byte_output_controls_preserve_valid_spans_and_invalid_stdout() {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let child = command
            .env(ROLE, "byte-controls")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let outcome = supervise_fixture_until(
            child,
            entry,
            Instant::now() + ISOLATION_TIMEOUT,
            |_, _, _| Ok(()),
        );
        outcome.report("test_isolated::tests::a_role");
        assert!(outcome.termination.is_ok() && outcome.entry.is_ok());
        assert!(outcome.wait.unwrap().success());
        let (out, _) = outcome.output.unwrap();
        assert!(out.contains("€\n\\x80\\xC3(\\xE2\\x82"), "{out}");
    }

    #[test]
    fn invalid_utf8_stderr_preserves_the_child_diagnostic() {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let child = command
            .env(ROLE, "bytes")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drain(child).wait_within(ISOLATION_TIMEOUT)
        }));
        entry.assert_once(pid);
        eprintln!("entry-proof test=test_isolated::tests::a_role child={pid} entry=once");
        assert!(
            captured.is_ok(),
            "child output lost escaped invalid UTF-8 diagnostic"
        );
        let (status, out, err) = captured.unwrap();
        assert!(!status.success(), "intended child failure was not observed");
        assert!(out.contains("valid stdout control"));
        assert!(
            err.contains("intended child diagnostic \\xFF\\xFE\\xC3("),
            "child output lost escaped invalid UTF-8 diagnostic: {err}"
        );
        assert!(err.contains("intended child failure"));
        eprintln!("{err}child-status={status} child={pid} bodies=1 cleanup=confirmed");
    }

    // Body entry and child cleanup are behavioral proofs; the rerun spelling catalog is retired.

    #[test]
    fn wait_until_retains_a_spent_deadline_and_confirms_expiry() {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let child = command
            .env(ROLE, "stall")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let mut started = None;
        let outcome = supervise_fixture_until(
            child,
            entry,
            Instant::now() + ISOLATION_TIMEOUT,
            |child, entry, deadline| {
                observe_entry(entry, child.id(), *deadline)?;
                let origin = Instant::now();
                started = Some(origin);
                *deadline = origin + Duration::from_millis(600);
                std::thread::sleep(Duration::from_millis(400));
                Ok(())
            },
        );
        outcome.report("test_isolated::tests::a_role");
        assert!(outcome.work.is_ok() && outcome.entry.is_ok() && outcome.termination.is_ok());
        let error = outcome.wait.unwrap_err();
        assert!(matches!(error.kind, ChildWaitKind::Expired) && error.termination_confirmed);
        eprintln!("body-proof test=test_isolated::tests::a_role child={} bodies=1 completed=false cleanup=confirmed", outcome.child);
        assert!(
            started.unwrap().elapsed() < Duration::from_millis(950),
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
