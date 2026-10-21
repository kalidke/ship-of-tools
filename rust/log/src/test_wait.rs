//! Test-only (feature `test-support`): the bounded form of a test's wait on another party. [`run_within`] runs a child
//! to its end and returns its whole account, [`Ran`]; [`try_run_within`] is its form that never panics, for a caller
//! that takes the failure as a value (a cleanup in `Drop`, a probe whose failure a skip names); [`within`] bounds any
//! other blocking wait (a join, a receive, an accept, a read). A wait that overruns fails its test by name instead of
//! holding the job, and a child that overruns is killed. A failure about a child carries its command, its exit status
//! and the last [`FAILURE_STREAM_BYTES`] of each stream ([`tail`]), the form `test_isolated`'s drained waits use too.

use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

/// The most of each stream a failure carries: its last bytes, where a hang or a fault usually shows, so a child that
/// wrote megabytes cannot flood the log.
pub const FAILURE_STREAM_BYTES: usize = 8 * 1024;

/// A child a test ran to its end: its exit status and both streams. Its `Display` is the account every failure about
/// it carries: the command, the status and the last bytes of each stream.
pub struct Ran {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    command: String,
}

/// Runs `command` to its end within `bound`, its input closed and both streams read as they come, and returns its
/// account; or, without panicking, why there is none: it did not start, it was still running at the bound (it is
/// killed), or a descendant still held its output open then. The error names the command and carries what it said.
pub fn try_run_within(command: &mut Command, bound: Duration) -> Result<Ran, String> {
    let words: Vec<String> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    let label = words.join(" ");
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = command.spawn().map_err(|error| format!("start {label}: {error}"))?;
    match crate::test_isolated::drain(child).try_wait_within(bound) {
        Ok((status, stdout, stderr)) => Ok(Ran { status, stdout, stderr, command: label }),
        Err(failure) => Err(format!("{label}: {failure}")),
    }
}

/// [`try_run_within`], failing the caller with its reason when the child has no account.
pub fn run_within(command: &mut Command, bound: Duration) -> Ran {
    try_run_within(command, bound).unwrap_or_else(|failure| panic!("{failure}"))
}

impl Ran {
    /// This account, when the child exited 0; any other end fails the caller with the whole account.
    pub fn ok(self) -> Self {
        assert!(self.status.success(), "{self}");
        self
    }
}

impl std::fmt::Display for Ran {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} exited: {}", self.command, self.status)?;
        for (name, body) in [("stdout", &self.stdout), ("stderr", &self.stderr)] {
            let (cut, body) = tail(body);
            write!(f, "\n--- {name}{cut} ---\n{body}")?;
        }
        Ok(())
    }
}

/// The last [`FAILURE_STREAM_BYTES`] of `body`, and a note saying how much was cut when that is not all of it.
pub(crate) fn tail(body: &str) -> (String, &str) {
    let mut start = body.len().saturating_sub(FAILURE_STREAM_BYTES);
    while !body.is_char_boundary(start) {
        start += 1;
    }
    let cut = if start > 0 { format!(", last {} of {} bytes", body.len() - start, body.len()) } else { String::new() };
    (cut, &body[start..])
}

/// Runs `wait` on a thread of its own and returns what it returns, within `bound`. A wait still blocked at the bound
/// fails the caller, naming `what`; its thread stays blocked and ends with the process. A panic in `wait` is the
/// caller's.
pub fn within<T: Send + 'static>(bound: Duration, what: &str, wait: impl FnOnce() -> T + Send + 'static) -> T {
    let (sent, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sent.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(wait)));
    });
    match done.recv_timeout(bound) {
        Ok(Ok(value)) => value,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => panic!("{what} did not end within {bound:?}"),
    }
}

#[cfg(test)]
#[path = "test_wait_tests.rs"]
mod tests;
