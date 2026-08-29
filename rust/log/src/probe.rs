//! Fault-injection scaffolding for the probe classifier's own model test
//! (a LATER unit — ADR 0041 "The probe", the Stage A/B transition table
//! under "Lifecycle"). This module ships NO decision logic and NO
//! classifier: only a seam, behind one trait, over the three kinds of
//! OS-facing operation that table consults — connect, challenge, and
//! process-wait/terminate — so a model test can drive every row (A1-A5,
//! B0-B9) deterministically with a SCRIPTED implementation, while the
//! shipped classifier drives the SAME trait with the REAL one.
//!
//! U0 SCOPE: the seam and both implementations below. Which observation
//! MEANS `READY`/`ABSENT`/`FOREIGN`/`PENDING`/... is the classifier's own
//! call (a later unit) and is deliberately absent here.

#![cfg(windows)]

use crate::challenge::{self, ChallengeOutcome, ChallengeableConnection, ChallengedProcess};
use crate::pipe_win::{self, PipeClient, PipeError};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One connect attempt's outcome, categorized only as far as ADR 0041's
/// probe table (Stage B) needs — MECHANICAL categories; what each one
/// MEANS is the classifier's call, not this seam's.
#[derive(Debug)]
pub enum ConnectOutcome {
    Connected(PipeClient),
    PipeBusy,
    FileNotFound,
    AccessDenied,
    OtherIo(std::io::Error),
}

/// A process-wait attempt's outcome (Stage A's A3/KILL+WAIT rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    Exited,
    StillRunning,
    WaitFailed,
}

/// Every OS-facing operation the probe classifier's transition table
/// consults, behind one seam.
pub trait ProbeOps {
    fn connect(&self, voyage_id: &str) -> ConnectOutcome;
    fn challenge(&self, conn: &dyn ChallengeableConnection, deadline: Instant) -> ChallengeOutcome;
    fn wait_exit(&self, process: &ChallengedProcess, timeout: Duration) -> WaitOutcome;
    fn terminate(&self, process: &ChallengedProcess) -> std::io::Result<()>;
}

/// The real implementation: `connect_voyage_pipe`, `challenge::challenge`,
/// and `ChallengedProcess`'s own `wait`/`terminate`, unmediated. No
/// decisions — just the mechanical OS calls the classifier drives through
/// [`ProbeOps`]. `connect` currently carries `connect_voyage_pipe`'s own
/// existing ~2s internal retry on `PIPE_BUSY`/`FILE_NOT_FOUND`; U0 does
/// not change that function's behavior, so this is what's available
/// today (a later unit may want a non-retrying variant for the
/// classifier's own 500ms-spaced probe loop).
pub struct RealProbeOps;

impl ProbeOps for RealProbeOps {
    fn connect(&self, voyage_id: &str) -> ConnectOutcome {
        match pipe_win::connect_voyage_pipe(voyage_id) {
            Ok(client) => ConnectOutcome::Connected(client),
            Err(PipeError::Io { source, .. }) => {
                use windows_sys::Win32::Foundation::{
                    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY,
                };
                match source.raw_os_error() {
                    Some(c) if c == ERROR_FILE_NOT_FOUND as i32 => ConnectOutcome::FileNotFound,
                    Some(c) if c == ERROR_PIPE_BUSY as i32 => ConnectOutcome::PipeBusy,
                    Some(c) if c == ERROR_ACCESS_DENIED as i32 => ConnectOutcome::AccessDenied,
                    _ => ConnectOutcome::OtherIo(source),
                }
            }
            Err(other) => ConnectOutcome::OtherIo(std::io::Error::other(other)),
        }
    }

    fn challenge(&self, conn: &dyn ChallengeableConnection, deadline: Instant) -> ChallengeOutcome {
        challenge::challenge(conn, deadline)
    }

    fn wait_exit(&self, process: &ChallengedProcess, timeout: Duration) -> WaitOutcome {
        match process.wait(timeout) {
            Ok(true) => WaitOutcome::Exited,
            Ok(false) => WaitOutcome::StillRunning,
            Err(_) => WaitOutcome::WaitFailed,
        }
    }

    fn terminate(&self, process: &ChallengedProcess) -> std::io::Result<()> {
        process.terminate()
    }
}

/// Scripted `ProbeOps` for a model test to drive one row of the probe
/// classifier's transition table at a time — EXISTS so that test (a later
/// unit) can force each Stage A/B observation deterministically instead
/// of arranging real OS races. Each queue drains in order; a call past
/// the end of its queue panics loudly — a test that runs out of script
/// under-specified its own scenario, rather than a caller that should
/// silently see something arbitrary.
#[derive(Default)]
pub struct ScriptedProbeOps {
    connect: Mutex<VecDeque<ConnectOutcome>>,
    challenge: Mutex<VecDeque<ChallengeOutcome>>,
    wait_exit: Mutex<VecDeque<WaitOutcome>>,
    terminate: Mutex<VecDeque<std::io::Result<()>>>,
}

impl ScriptedProbeOps {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_connect(&self, outcome: ConnectOutcome) {
        self.connect.lock().unwrap().push_back(outcome);
    }

    pub fn push_challenge(&self, outcome: ChallengeOutcome) {
        self.challenge.lock().unwrap().push_back(outcome);
    }

    pub fn push_wait_exit(&self, outcome: WaitOutcome) {
        self.wait_exit.lock().unwrap().push_back(outcome);
    }

    pub fn push_terminate(&self, outcome: std::io::Result<()>) {
        self.terminate.lock().unwrap().push_back(outcome);
    }
}

impl ProbeOps for ScriptedProbeOps {
    fn connect(&self, _voyage_id: &str) -> ConnectOutcome {
        self.connect
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted connect outcome exhausted")
    }

    fn challenge(&self, _conn: &dyn ChallengeableConnection, _deadline: Instant) -> ChallengeOutcome {
        self.challenge
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted challenge outcome exhausted")
    }

    fn wait_exit(&self, _process: &ChallengedProcess, _timeout: Duration) -> WaitOutcome {
        self.wait_exit
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted wait_exit outcome exhausted")
    }

    fn terminate(&self, _process: &ChallengedProcess) -> std::io::Result<()> {
        self.terminate
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted terminate outcome exhausted")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_connect_outcomes_drain_in_order_without_touching_the_os() {
        let ops = ScriptedProbeOps::new();
        ops.push_connect(ConnectOutcome::FileNotFound);
        ops.push_connect(ConnectOutcome::PipeBusy);
        assert!(matches!(ops.connect("unused"), ConnectOutcome::FileNotFound));
        assert!(matches!(ops.connect("unused"), ConnectOutcome::PipeBusy));
    }

    #[test]
    fn scripted_wait_and_terminate_outcomes_drain_in_order() {
        let ops = ScriptedProbeOps::new();
        ops.push_wait_exit(WaitOutcome::StillRunning);
        ops.push_wait_exit(WaitOutcome::Exited);
        ops.push_terminate(Ok(()));
        ops.push_terminate(Err(std::io::Error::other("boom")));

        // A REAL `ChallengedProcess` is required to type-check here even
        // though the scripted impl ignores it — it holds a handle to
        // THIS TEST PROCESS, so this test never calls `.terminate()` on
        // it directly (only `ops.terminate`, which is scripted and never
        // touches the handle at all).
        let process = crate::challenge::tests::self_proven_process();
        assert_eq!(ops.wait_exit(&process, Duration::from_millis(1)), WaitOutcome::StillRunning);
        assert_eq!(ops.wait_exit(&process, Duration::from_millis(1)), WaitOutcome::Exited);
        assert!(ops.terminate(&process).is_ok());
        assert!(ops.terminate(&process).is_err());
    }

    #[test]
    #[should_panic(expected = "exhausted")]
    fn scripted_ops_panics_loudly_past_its_script() {
        let ops = ScriptedProbeOps::new();
        let _ = ops.connect("unused");
    }
}
