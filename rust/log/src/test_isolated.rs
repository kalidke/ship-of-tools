//! Test-only (feature `test-support`): the one way a test re-runs its own binary for one named test, and waits on it
//! within a bound. A child that did not enter the named test's body fails its parent, so a name that matches no test
//! cannot pass, and an entry record dropped unchecked fails too. Every wait here is bounded: a child that overruns is
//! killed, and output that a descendant still holds open after the child's exit fails the caller at the bound. What
//! this does not prove: that a killed child's descendants are gone; a kill whose exit is not seen within 5 s is
//! reported as unconfirmed. Direct fixtures retain child/entry and separate stream observations on failure.
//! Regression proofs validate observed role/pid prerequisites and exact causes before accepting a red.
//! Both output streams are captured as bytes; invalid UTF-8 is rendered as explicit uppercase byte escapes.
//! Wrapped native fixtures select their PID from a complete stdout witness, independently of exact entry.

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
    /// A private folder holding the record of a wrapped fixture (see [`Entry::prepare_wrapped_record`]).
    folder: Option<PathBuf>,
    checked: bool,
}

impl Entry {
    /// The explicitly named ISO assignment; wrapped recipes transfer only this entry path.
    pub fn environment_assignment(&self) -> (&'static str, &std::ffi::OsStr) {
        (ENTERED, self.path.as_os_str())
    }

    /// Create the test-owned record before elevation, in a private folder made for it. A wrapped fixture's native child
    /// may run as another account (root), and the kernel (`fs.protected_regular`) refuses `O_CREAT` of a file another
    /// account owns in a world-writable sticky folder such as the system temp folder. So the record is never there:
    /// its folder is not sticky and not writable by others, and the child's `enter` opens an existing file.
    pub fn prepare_wrapped_record(&mut self) -> std::io::Result<()> {
        let folder = std::env::temp_dir().join(format!(
            "sot-entered-dir-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&folder)?;
        self.folder = Some(folder.clone());
        self.path = folder.join("entered");
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)
            .map(|_| ())
    }

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
        if let Some(folder) = &self.folder {
            let _ = std::fs::remove_dir(folder);
        }
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
            folder: None,
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
    /// A wrapped native PID comes from stdout, independently of the entry file.
    pub native: Option<Result<u32, String>>,
    pub work: Result<T, FixtureFailure>,
    pub wait: Result<ExitStatus, ChildWaitError>,
    pub termination: Result<(), String>,
    pub entry: Result<(), String>,
    pub output: Result<(String, String), String>,
    /// Separate observations survive failure of the other stream.
    pub streams: FixtureOutput,
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
    deadline: Instant,
    work: impl FnOnce(&mut Child, &Entry, &mut Instant) -> Result<T, FixtureFailure>,
) -> FixtureOutcome<T> {
    supervise_selected(child, entry, deadline, None, work)
}

/// A wrapped spawn failure retains its original cause and attempted entry check.
#[derive(Debug)]
pub struct WrappedSpawnFailure {
    pub error: std::io::Error,
    pub entry: Result<(), String>,
}

/// The wrapped native fixture uses the direct owner's wait, independent streams and finalization.
pub fn supervise_wrapped_fixture_until(
    mut command: Command,
    entry: Entry,
    deadline: Instant,
    role: &str,
) -> Result<FixtureOutcome<()>, WrappedSpawnFailure> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    match command.spawn() {
        Ok(child) => Ok(supervise_selected(
            child,
            entry,
            deadline,
            Some(role),
            |child, _, _| {
                eprintln!("native-launcher test={role} launcher={}", child.id());
                std::io::stderr().flush().map_err(|error| {
                    FixtureFailure::Error(format!("flush launcher observation: {error}"))
                })
            },
        )),
        Err(error) => {
            let entry =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry.assert_once(0)))
                    .map_err(panic_text);
            Err(WrappedSpawnFailure { error, entry })
        }
    }
}

fn native_pid(streams: &FixtureOutput, role: &str) -> Result<u32, String> {
    let text = match &streams.stdout {
        Ok(text) => text.as_str(),
        Err(error) => error.captured.as_str(),
    };
    let records: Vec<_> = text
        .split_inclusive('\n')
        .filter_map(|line| line.find("native-start ").map(|at| &line[at..]))
        .collect();
    if records.len() != 1 {
        return Err(format!(
            "native start witness count {}, expected one",
            records.len()
        ));
    }
    let line = records[0];
    let prefix = format!("native-start test={role} native=");
    let pid = line
        .strip_prefix(&prefix)
        .and_then(|s| s.strip_suffix('\n'))
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|pid| *pid != 0)
        .ok_or_else(|| "native start witness partial, wrong role or invalid PID".to_string())?;
    Ok(pid)
}

fn supervise_selected<T>(
    child: Child,
    entry: Entry,
    mut deadline: Instant,
    role: Option<&str>,
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
    let streams = draining.observe_output_until(deadline);
    let native = role.map(|role| native_pid(&streams, role));
    let expected = native
        .as_ref()
        .map_or(pid, |native| *native.as_ref().unwrap_or(&0));
    let entry =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry.assert_once(expected)))
            .map_err(panic_text);
    let entry = match &native {
        Some(Err(error)) => Err(format!("{error}; exact entry: {entry:?}")),
        _ => entry,
    };
    let output = streams.combined();
    FixtureOutcome {
        child: pid,
        native,
        work,
        wait,
        termination,
        entry,
        output,
        streams,
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
    fn observe_output_until(&mut self, deadline: Instant) -> FixtureOutput {
        FixtureOutput {
            stdout: read_output(self.out.take(), deadline, "stdout"),
            stderr: read_output(self.err.take(), deadline, "stderr"),
        }
    }
    fn output_until(&mut self, deadline: Instant) -> Result<(String, String), String> {
        self.observe_output_until(deadline).combined()
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

/// An output failure retains its stream, original I/O cause and any captured diagnostic.
#[derive(Clone, Debug)]
pub struct OutputFailure {
    pub stream: &'static str,
    pub kind: Option<std::io::ErrorKind>,
    pub reason: String,
    pub captured: String,
}
impl std::fmt::Display for OutputFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "reading the child's {}: {}; captured={}",
            self.stream, self.reason, self.captured
        )
    }
}
/// Both output observations are retained independently before the compatibility tuple is formed.
pub struct FixtureOutput {
    pub stdout: Result<String, OutputFailure>,
    pub stderr: Result<String, OutputFailure>,
}
impl FixtureOutput {
    fn combined(&self) -> Result<(String, String), String> {
        match (&self.stdout, &self.stderr) {
            (Ok(out), Ok(err)) => Ok((out.clone(), err.clone())),
            (out, err) => Err(format!("stdout: {out:?}; stderr: {err:?}")),
        }
    }
}
fn read_output(
    reader: Option<Receiver<(Vec<u8>, std::io::Result<()>)>>,
    deadline: Instant,
    what: &'static str,
) -> Result<String, OutputFailure> {
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
            result.map(|()| text.clone()).map_err(|error| OutputFailure {
                stream: what, kind: Some(error.kind()), reason: error.to_string(), captured: text,
            })
        }
        Err(error) => Err(OutputFailure {
            stream: what, kind: None, captured: String::new(),
            reason: match error {
                RecvTimeoutError::Timeout => format!("the child exited but its {what} did not end within the output deadline (a descendant may hold it)"),
                RecvTimeoutError::Disconnected => format!("the reader of the child's {what} ended without a result"),
            },
        }),
    }
}

/// Exact selected readiness cause, shared by the proofs and their rejection controls.
#[derive(Clone, Copy, Debug)]
pub enum ReadinessCase {
    Timeout,
    Error,
    Panic,
}
impl ReadinessCase {
    pub fn expected(self) -> (&'static str, &'static str) {
        match self {
            Self::Timeout => ("timeout", "expected readiness marker missing"),
            Self::Error => ("error", "readiness read failed: NotFound"),
            Self::Panic => ("panic", "deliberate readiness panic"),
        }
    }
}
/// The same selected failure operation is used by ISO and socket fixture controllers.
pub fn readiness_failure(
    case: ReadinessCase,
    control: &str,
    missing: &std::path::Path,
    timeout: impl FnOnce() -> Result<(), FixtureFailure>,
) -> Result<(), FixtureFailure> {
    if control == "variant" {
        return Err(match case {
            ReadinessCase::Error => FixtureFailure::Timeout("another timeout phase".into()),
            _ => FixtureFailure::Error("unrelated readiness failure".into()),
        });
    }
    if control == "reason" {
        return match case {
            ReadinessCase::Timeout => Err(FixtureFailure::Timeout("another timeout phase".into())),
            ReadinessCase::Error => Err(FixtureFailure::Error("unrelated read failure".into())),
            ReadinessCase::Panic => panic!("unrelated readiness panic"),
        };
    }
    match case {
        ReadinessCase::Panic => panic!("deliberate readiness panic"),
        ReadinessCase::Error => {
            let error = std::fs::read(missing).unwrap_err();
            eprintln!("fixture-io kind={:?} reason={error}", error.kind());
            Err(FixtureFailure::Error(format!(
                "readiness read failed: {:?}",
                error.kind()
            )))
        }
        ReadinessCase::Timeout => timeout(),
    }
}
pub fn readiness_scenarios() -> [(ReadinessCase, bool, bool); 5] {
    [
        (ReadinessCase::Timeout, false, false),
        (ReadinessCase::Error, false, false),
        (ReadinessCase::Panic, false, false),
        (ReadinessCase::Timeout, true, false),
        (ReadinessCase::Timeout, false, true),
    ]
}
/// The complete start witness associates the actual child's role and spawned pid.
pub fn fixture_start_record(role: &str, pid: u32) -> String {
    format!("fixture-start test={role} child={pid}\n")
}
pub fn fixture_start_matches(text: &str, role: &str, pid: u32) -> bool {
    text.split_inclusive('\n')
        .any(|line| line == fixture_start_record(role, pid))
}
/// Report the cause separately from finalization; all observations have already been collected by ISO.
pub fn verify_readiness(
    outcome: &FixtureOutcome<()>,
    role: &str,
    start: bool,
    selected: ReadinessCase,
    held: bool,
    zero: bool,
) {
    outcome.report(role);
    let (kind, reason) = selected.expected();
    let actual = outcome.work.as_ref().err();
    let matched = start && actual.is_some_and(|f| f.kind() == kind && f.to_string() == reason);
    eprintln!("fixture-cause test={role} child={} start={} selected={kind} actual={} reason={} matched={matched}",
        outcome.child, if start { "observed" } else { "missing" },
        actual.map_or("ok", FixtureFailure::kind), actual.map_or(String::new(), ToString::to_string));
    assert!(start, "readiness proof did not observe fixture start");
    assert!(matched, "readiness proof observed the wrong failure");
    let ended = if held {
        outcome
            .wait
            .as_ref()
            .is_err_and(|e| matches!(e.kind, ChildWaitKind::Expired) && e.termination_confirmed)
    } else {
        outcome.wait.as_ref().is_ok_and(ExitStatus::success)
    };
    let entered = if zero {
        outcome
            .entry
            .as_ref()
            .is_err_and(|e| e.contains("isolated body did not enter"))
    } else {
        outcome.entry.is_ok()
    };
    assert!(
        ended && entered && outcome.termination.is_ok() && outcome.output.is_ok(),
        "readiness failure bypassed owned-child finalization"
    );
}
/// Real-fixture rejection controls use exactly the ordinary readiness verifier.
pub fn verify_readiness_controls(
    role: &str,
    mut fixture: impl FnMut(ReadinessCase, &str) -> (FixtureOutcome<()>, bool),
) {
    let mut checked = 0;
    for case in [
        ReadinessCase::Timeout,
        ReadinessCase::Error,
        ReadinessCase::Panic,
    ] {
        for control in [
            "before", "pid", "role", "partial", "variant", "reason", "valid",
        ] {
            if std::env::var("SOT_TEST_PROOF_CONTROL").is_ok_and(|wanted| wanted != control) {
                continue;
            }
            checked += 1;
            let (outcome, start) = fixture(case, control);
            outcome.report(role);
            assert!(
                outcome.wait.as_ref().unwrap().success()
                    && outcome.termination.is_ok()
                    && outcome.entry.is_ok()
            );
            let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                verify_readiness(&outcome, role, start, case, false, false)
            }));
            if control == "valid" {
                assert!(rejected.is_ok());
                continue;
            }
            let missing = matches!(control, "before" | "pid" | "role" | "partial");
            assert!(
                rejected.is_err(),
                "readiness proof accepted {}",
                if missing {
                    "an unobserved fixture start"
                } else {
                    "the wrong failure"
                }
            );
            let reason = panic_text(rejected.unwrap_err());
            assert!(
                reason.contains(if missing {
                    "readiness proof did not observe fixture start"
                } else {
                    "readiness proof observed the wrong failure"
                }),
                "{reason}"
            );
            eprintln!("readiness-rejection control={control} reason={reason}");
        }
    }
    assert!(checked > 0, "readiness controls selected no fixture");
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

    /// A wrapped fixture's record is where a root child can create it: not in the world-writable sticky system temp
    /// folder, whose `fs.protected_regular` refuses `O_CREAT` of a file another account owns. The record's folder is
    /// owned by the caller, private, not sticky, and goes away with the entry.
    #[cfg(unix)]
    #[test]
    fn a_wrapped_record_lives_in_a_private_non_sticky_folder() {
        use std::os::unix::fs::MetadataExt;
        let (_, mut entry) = test_command(
            "test_isolated::tests::a_wrapped_record_lives_in_a_private_non_sticky_folder",
        );
        entry.prepare_wrapped_record().unwrap();
        let (name, record) = entry.environment_assignment();
        assert_eq!(name, ENTERED);
        let record = PathBuf::from(record);
        let folder = record.parent().unwrap().to_path_buf();
        assert_ne!(
            folder,
            std::env::temp_dir(),
            "the record sits directly in the shared temp folder"
        );
        let meta = std::fs::metadata(&folder).unwrap();
        assert_eq!(
            meta.mode() & 0o7777,
            0o700,
            "the record's folder is sticky or open to others"
        );
        // SAFETY: geteuid has no preconditions.
        assert_eq!(meta.uid(), unsafe { libc::geteuid() });
        assert_eq!(std::fs::metadata(&record).unwrap().len(), 0);
        // What `enter` does in the child, in the child's own account: append to the existing record.
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&record)
            .unwrap();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entry.assert_once(0)))
                .is_err()
        );
        assert!(!folder.exists(), "the record's folder outlived its entry");
    }

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
            let mut record =
                fixture_start_record("test_isolated::tests::a_role", std::process::id());
            match std::env::var("SOT_TEST_FIXTURE_WITNESS").as_deref() {
                Ok("pid") => record = fixture_start_record("test_isolated::tests::a_role", 0),
                Ok("role") => {
                    record =
                        fixture_start_record("test_isolated::tests::wrong_role", std::process::id())
                }
                Ok("partial") => {
                    record = "fixture-start test=test_isolated::tests::a_role\n".into()
                }
                _ => {}
            }
            std::fs::write(path, record + "fixture-start-written\n").unwrap();
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
                let mut err = std::io::stderr().lock();
                err.write_all(b"intended child diagnostic \xFF\xFE\xC3(\n")
                    .unwrap();
                err.flush().unwrap();
                let mut out = std::io::stdout().lock();
                writeln!(out, "valid stdout control\nutf8-start test=test_isolated::tests::a_role child={} intended child failure", std::process::id()).unwrap();
                out.flush().unwrap();
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

    fn observe_record(
        path: &std::path::Path,
        deadline: Instant,
        marker: &str,
    ) -> Result<String, FixtureFailure> {
        loop {
            let text = std::fs::read_to_string(path).map_err(|e| {
                eprintln!("fixture-io kind={:?} reason={}", e.kind(), e);
                FixtureFailure::Error(format!("readiness read failed: {:?}", e.kind()))
            })?;
            if text
                .split_inclusive('\n')
                .any(|line| line.ends_with('\n') && line.starts_with(marker))
            {
                return Ok(text);
            }
            if Instant::now() >= deadline {
                return Err(FixtureFailure::Timeout(
                    "expected readiness marker missing".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn readiness_fixture(
        selected: ReadinessCase,
        control: &str,
        role: &str,
    ) -> (FixtureOutcome<()>, bool) {
        let started = tempfile::NamedTempFile::new().unwrap();
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let child = command
            .env(ROLE, role)
            .env("SOT_TEST_FIXTURE_STARTED", started.path())
            .env("SOT_TEST_FIXTURE_WITNESS", control)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut start = false;
        let outcome = supervise_fixture_until(
            child,
            entry,
            Instant::now() + ISOLATION_TIMEOUT,
            |_, _, end| {
                let path = if control == "before" {
                    started.path().with_extension("missing")
                } else {
                    started.path().to_path_buf()
                };
                let witness = observe_record(&path, *end, "fixture-start-written")?;
                start = fixture_start_matches(&witness, "test_isolated::tests::a_role", pid);
                eprintln!("fixture-witness child={pid} record={witness:?}");
                let readiness_end = Instant::now() + Duration::from_millis(150);
                *end = readiness_end + Duration::from_millis(600);
                readiness_failure(
                    selected,
                    control,
                    &started.path().with_extension("missing"),
                    || observe_record(started.path(), readiness_end, "withheld-ready").map(|_| ()),
                )
            },
        );
        (outcome, start)
    }
    #[test]
    fn readiness_failure_finishes_owned_child_checks() {
        for (case, held, zero) in readiness_scenarios() {
            let role = if held {
                "stall"
            } else if zero {
                "zero-release"
            } else {
                "release"
            };
            let (outcome, start) = readiness_fixture(case, "valid", role);
            verify_readiness(
                &outcome,
                "test_isolated::tests::a_role",
                start,
                case,
                role == "stall",
                role == "zero-release",
            );
        }
    }
    #[test]
    fn readiness_proof_rejects_wrong_failure() {
        verify_readiness_controls("test_isolated::tests::a_role", |case, control| {
            readiness_fixture(case, control, "release")
        });
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

    fn byte_fixture() -> FixtureOutcome<()> {
        let (mut command, entry) = test_command("test_isolated::tests::a_role");
        let child = command
            .env(ROLE, "bytes")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        supervise_fixture_until(
            child,
            entry,
            Instant::now() + ISOLATION_TIMEOUT,
            |_, _, _| Ok(()),
        )
    }
    // Independent child prerequisites precede decoder classification, including in every negative control.
    fn verify_utf8(
        outcome: &FixtureOutcome<()>,
        injected: Option<&OutputFailure>,
    ) -> Result<(), &'static str> {
        let role = "test_isolated::tests::a_role";
        outcome.report(role);
        let out = outcome
            .streams
            .stdout
            .as_ref()
            .map_err(|_| "UTF-8 fixture stdout failed")?;
        let witness = format!(
            "utf8-start test={role} child={} intended child failure\n",
            outcome.child
        );
        assert!(
            outcome.wait.as_ref().is_ok_and(|s| s.code() == Some(101))
                && outcome.termination.is_ok()
                && outcome.entry.is_ok()
                && out.contains("valid stdout control\n")
                && out.split_inclusive('\n').any(|line| line == witness),
            "UTF-8 fixture prerequisites failed"
        );
        eprintln!(
            "utf8-child child={} status={} cleanup=confirmed entry=once witness={witness:?}",
            outcome.child,
            outcome.wait.as_ref().unwrap()
        );
        let failure = injected.or_else(|| outcome.streams.stderr.as_ref().err());
        if let Some(failure) = failure {
            let decoder = failure.stream == "stderr"
                && failure.kind == Some(std::io::ErrorKind::InvalidData)
                && failure.reason == "stream did not contain valid UTF-8";
            eprintln!("utf8-cause test={role} child={} status={} cleanup=confirmed entry=once stream={} kind={:?} reason={} decoder={}",
                outcome.child, outcome.wait.as_ref().unwrap(), failure.stream, failure.kind, failure.reason, if decoder { "matched" } else { "rejected" });
            return Err(if decoder {
                "child output lost escaped invalid UTF-8 diagnostic"
            } else {
                "UTF-8 proof observed an unrelated capture failure"
            });
        }
        let err = outcome.streams.stderr.as_ref().unwrap();
        assert!(
            err.contains("intended child diagnostic \\xFF\\xFE\\xC3(")
                && err.contains("intended child failure"),
            "child output lost escaped invalid UTF-8 diagnostic: {err}"
        );
        eprintln!(
            "{err}child-status={} child={} bodies=1 cleanup=confirmed",
            outcome.wait.as_ref().unwrap(),
            outcome.child
        );
        Ok(())
    }
    #[test]
    fn invalid_utf8_stderr_preserves_the_child_diagnostic() {
        verify_utf8(&byte_fixture(), None).unwrap_or_else(|reason| panic!("{reason}"));
    }
    #[test]
    fn utf8_proof_rejects_wrong_capture_failure() {
        for (stream, kind, reason) in [
            (
                "stderr",
                std::io::ErrorKind::Other,
                "unrelated output observation failure",
            ),
            (
                "stdout",
                std::io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            ),
            (
                "stderr",
                std::io::ErrorKind::InvalidData,
                "another decoder reason",
            ),
        ] {
            let outcome = byte_fixture();
            let failure = OutputFailure {
                stream,
                kind: Some(kind),
                reason: reason.into(),
                captured: String::new(),
            };
            let rejected = verify_utf8(&outcome, Some(&failure));
            assert_eq!(
                rejected,
                Err("UTF-8 proof observed an unrelated capture failure"),
                "UTF-8 proof accepted an unrelated capture failure"
            );
            eprintln!("utf8-rejection stream={stream} kind={kind:?} reason={reason}");
        }
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
