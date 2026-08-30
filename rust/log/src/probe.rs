//! Fault-injection scaffolding for the probe classifier's own model test
//! (a LATER unit — ADR 0041 "The probe", the Stage A/B transition table
//! under "Lifecycle"). This module ships NO decision logic and NO
//! classifier: only a seam, behind one trait, over every kind of
//! OS-facing operation that table consults — spawn, wait/kill on the
//! OWNED pre-proof child, connect, challenge, a writer-fence probe, and
//! an injectable clock — so a model test can drive every row (A1-A5,
//! B0-B9) deterministically with a SCRIPTED implementation that touches
//! NO real OS object, while the shipped classifier drives the SAME trait
//! with a REAL one.
//!
//! U0 SCOPE: the seam and both implementations below. Which observation
//! MEANS `READY`/`ABSENT`/`FOREIGN`/`PENDING`/... is the classifier's own
//! call (a later unit) and is deliberately absent here.
//!
//! # Round-1 review: associated types, not concrete OS objects
//!
//! `ProbeOps` is generic over three associated types (`Conn`,
//! `SpawnedChild`, `Process`) rather than hard-wiring `PipeClient` /
//! `ChallengedProcess`. `RealProbeOps` binds them to the real Windows
//! types; `ScriptedProbeOps` binds them to zero-sized dummy types that
//! never touch the OS at all — so a model test can construct every row
//! without a real pipe, a real spawned process, or a real challenge.

#![cfg(windows)]

use crate::challenge::{self, ChallengeOutcome};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::time::{Duration, Instant};

/// One connect attempt's outcome, categorized only as far as ADR 0041's
/// probe table (Stage B) needs — MECHANICAL categories; what each one
/// MEANS is the classifier's call, not this seam's.
#[derive(Debug)]
pub enum ConnectOutcome<C> {
    Connected(C),
    PipeBusy,
    FileNotFound,
    AccessDenied,
    OtherIo(std::io::Error),
}

/// One spawn attempt's outcome (Stage A's A1 row).
#[derive(Debug)]
pub enum SpawnOutcome<Child> {
    Spawned(Child),
    Failed(std::io::Error),
}

/// A process-wait attempt's outcome — shared by the OWNED pre-proof
/// child (Stage A's A2/A3) and an already-proven `Process` (the death
/// signal / KILL+WAIT's wait half): the outcome shape doesn't depend on
/// WHOSE identity the handle carries, only on what `WaitForSingleObject`
/// reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    Exited,
    StillRunning,
    WaitFailed,
}

/// A writer-fence probe's outcome (Stage B's B7/B8: a `FILE_NOT_FOUND`
/// connect is ambiguous between ABSENT and PENDING until the voyage's own
/// `writer.lock` is separately probed).
#[derive(Debug)]
pub enum FenceProbe {
    Free,
    Held,
    Error(std::io::Error),
}

/// Every OS-facing operation the probe classifier's transition table
/// consults, behind one seam. NO method here decides what an outcome
/// MEANS — each just performs (or, for `ScriptedProbeOps`, replays) one
/// mechanical observation.
pub trait ProbeOps {
    /// A live connection — `PipeClient` for the real impl, a cheap dummy
    /// marker for the scripted impl. No trait bound here (round-2 finding
    /// 7): only `ProbeOps::challenge`'s REAL implementation needs
    /// `crate::challenge::ChallengeableConnection`, and it already knows its own concrete
    /// `Conn` type, so the bound would only ever force a placeholder
    /// implementation on every OTHER impl for no reason.
    type Conn;
    /// The OWNED, NOT YET CHALLENGED child this probe episode itself
    /// spawned (Stage A). Deliberately distinct from `Process`: nothing
    /// has proven this handle's identity — it's ours because we just
    /// created it.
    type SpawnedChild;
    /// A proven server's retained identity (Stage A4/B1's READY/ADOPTED
    /// evidence) — `challenge::ChallengedProcess` for the real impl.
    type Process;

    /// A1: attempt to spawn the capsule child. The CALLER supplies what
    /// to run (this seam has no opinion on that); `Err` from
    /// `Command::spawn` is SPAWN-FAILED.
    fn spawn(&self, command: &mut std::process::Command) -> SpawnOutcome<Self::SpawnedChild>;
    /// A2/A3: has the owned child exited, or is its handle wait outcome
    /// otherwise observable, within `timeout`?
    fn wait_child(&self, child: &Self::SpawnedChild, timeout: Duration) -> WaitOutcome;
    /// A3's KILL half: terminate the owned child.
    fn kill_child(&self, child: &Self::SpawnedChild) -> std::io::Result<()>;

    /// B1-B9 (and A4/A5, which reuse this SAME operation on the owned
    /// child's own pipe): attempt one connect to a voyage pipe.
    fn connect(&self, voyage_id: &str) -> ConnectOutcome<Self::Conn>;
    /// The post-connect challenge (B1-B6, and A4/A5).
    fn challenge(&self, conn: &Self::Conn, deadline: Instant) -> ChallengeOutcome<Self::Process>;
    /// B7/B8: is the voyage's own writer fence free, held, or unprobeable?
    fn writer_fence_probe(&self, voyage_root: &Path) -> FenceProbe;

    /// The death signal on an ALREADY-PROVEN process (adopted, not
    /// owned).
    fn wait_exit(&self, process: &Self::Process, timeout: Duration) -> WaitOutcome;
    /// The invalid-mgmt fallback's kill authority on an ALREADY-PROVEN
    /// process.
    fn terminate(&self, process: &Self::Process) -> std::io::Result<()>;

    /// An injectable clock. B0/readiness cutoffs are measured against
    /// THIS, never `Instant::now()` directly, in any code that consumes
    /// `ProbeOps` — so a model test can deterministically reach any
    /// elapsed-time observation (an expired episode deadline, a blown
    /// readiness cutoff) without a real sleep.
    fn now(&self) -> Instant;
}

/// A just-spawned, NOT YET CHALLENGED child process handle. Stage A's
/// A1-A3 observations are about THIS type, never `ChallengedProcess`:
/// nothing has proven this handle's identity (it's ours only because we
/// just created it), so it carries none of the SID/creation-time
/// provenance a `ChallengedProcess` does — just enough to wait on it or
/// kill it. Dropping this closes the handle.
#[derive(Debug)]
pub struct SpawnedChild {
    handle: OwnedHandle,
}

impl SpawnedChild {
    // U1a Codex round-1, Blocker 2: `RealProbeOps` (the only caller of
    // this constructor) is now `pub(crate)` with no in-crate consumer
    // yet either -- deliberate scaffolding for U2's classifier, not dead
    // code to delete (see `RealProbeOps`'s own doc).
    #[allow(dead_code)]
    fn from_child(child: std::process::Child) -> Self {
        use std::os::windows::io::IntoRawHandle;
        // `into_raw_handle` consumes `child`, transferring ownership of
        // the PROCESS handle to us; any piped stdio the caller requested
        // are separate handles, dropped normally as `child`'s other
        // fields go out of scope here — untouched by this handoff.
        let raw = child.into_raw_handle();
        // SAFETY: `raw` came from `IntoRawHandle::into_raw_handle`,
        // which transfers unique ownership.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        Self { handle }
    }

    fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.handle.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE
    }

    /// See [`challenge::wait_handle`]'s doc for the bound.
    pub fn wait(&self, timeout: Duration) -> std::io::Result<bool> {
        challenge::wait_handle(self.raw(), timeout)
    }

    pub fn terminate(&self) -> std::io::Result<()> {
        challenge::terminate_handle(self.raw())
    }
}

/// The real implementation: `connect_voyage_pipe_unchallenged`,
/// `challenge::challenge` (with the voyage mgmt lane's
/// `VoyageMgmtExchange`), `std::process` spawn, and the bounded
/// wait/terminate helpers, unmediated. No decisions — just the mechanical
/// OS calls the classifier drives through [`ProbeOps`]. `connect` uses the
/// UNCHALLENGED connect (`pipe_win::connect_voyage_pipe` itself runs SID
/// authentication internally, which would collapse Stage B's own
/// connect-then-challenge rows — see that function's doc) — it currently
/// carries that raw connect's existing ~2s internal retry on
/// `PIPE_BUSY`/`FILE_NOT_FOUND`; U0 does not change that function's
/// behavior, so this is what's available today (a later unit may want a
/// non-retrying variant for the classifier's own 500ms-spaced probe loop).
///
/// # `pub(crate)`, not `pub` (U1a Codex round-1, Blocker 2)
///
/// An earlier version of this type was `pub`, which made the UNCHALLENGED
/// connection it hands back through `ConnectOutcome::Connected` a public
/// path to raw pipe I/O: external code could call
/// `RealProbeOps.connect(id)`, extract the `PipeClient`, and read/write it
/// directly without ever running `challenge`/`authenticate_server` — the
/// exact leak `connect_voyage_pipe`'s own enforcement exists to close,
/// reopened one layer up. `RealProbeOps` has no production consumer today
/// (this whole module is scaffolding — see the module doc), so nothing
/// depends on it being public, and `sot-capsule` (a separate bin CRATE
/// TARGET, `src/bin/sot-capsule.rs`) cannot see `pub(crate)` items
/// regardless: it only ever reaches this library through its `pub` API.
/// That is the intended shape once U2's classifier lands — a `pub`
/// function/type living IN THIS CRATE, wrapping `RealProbeOps` internally,
/// which `sot-capsule` calls — never `RealProbeOps` directly. Keeping the
/// trait (`ProbeOps`) and the scripted implementation (`ScriptedProbeOps`,
/// already gated behind `cfg(test)`/`test-support`) at their current
/// visibility is unaffected: neither one hands back a real, unauthenticated
/// `PipeClient` to anything outside this crate.
#[allow(dead_code)] // deliberate scaffolding, no consumer until U2 -- see this type's own doc
pub(crate) struct RealProbeOps;

impl ProbeOps for RealProbeOps {
    type Conn = crate::pipe_win::PipeClient;
    type SpawnedChild = SpawnedChild;
    type Process = challenge::ChallengedProcess;

    fn spawn(&self, command: &mut std::process::Command) -> SpawnOutcome<Self::SpawnedChild> {
        match command.spawn() {
            Ok(child) => SpawnOutcome::Spawned(SpawnedChild::from_child(child)),
            Err(e) => SpawnOutcome::Failed(e),
        }
    }

    fn wait_child(&self, child: &Self::SpawnedChild, timeout: Duration) -> WaitOutcome {
        match child.wait(timeout) {
            Ok(true) => WaitOutcome::Exited,
            Ok(false) => WaitOutcome::StillRunning,
            Err(_) => WaitOutcome::WaitFailed,
        }
    }

    fn kill_child(&self, child: &Self::SpawnedChild) -> std::io::Result<()> {
        child.terminate()
    }

    fn connect(&self, voyage_id: &str) -> ConnectOutcome<Self::Conn> {
        match crate::pipe_win::connect_voyage_pipe_unchallenged(voyage_id) {
            Ok(client) => ConnectOutcome::Connected(client),
            Err(crate::pipe_win::PipeError::Io { source, .. }) => {
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

    fn challenge(&self, conn: &Self::Conn, deadline: Instant) -> ChallengeOutcome<Self::Process> {
        let mut exchange = crate::exchange::VoyageMgmtExchange::default();
        challenge::challenge(conn, &mut exchange, deadline)
    }

    fn writer_fence_probe(&self, voyage_root: &Path) -> FenceProbe {
        let lock_path = voyage_root.join("writer.lock");
        match crate::fsutil::lock_writer(&lock_path) {
            // The guard drops here, releasing the fence immediately --
            // this is a PROBE, never a hold.
            Ok(_guard) => FenceProbe::Free,
            Err(crate::Error::State(_)) => FenceProbe::Held,
            Err(crate::Error::Io(e)) => FenceProbe::Error(e),
            Err(other) => FenceProbe::Error(std::io::Error::other(other)),
        }
    }

    fn wait_exit(&self, process: &Self::Process, timeout: Duration) -> WaitOutcome {
        match process.wait(timeout) {
            Ok(true) => WaitOutcome::Exited,
            Ok(false) => WaitOutcome::StillRunning,
            Err(_) => WaitOutcome::WaitFailed,
        }
    }

    fn terminate(&self, process: &Self::Process) -> std::io::Result<()> {
        process.terminate()
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

// ---------------------------------------------------------------------
// Test-only scaffolding: gated out of the production API (round-1 minor
// finding 9). `cfg(test)` covers this crate's OWN unit tests; `feature =
// "test-support"` additionally covers a SEPARATE integration-test crate
// in `tests/` within this same package, which can enable it via a
// self-referential `[dev-dependencies]` entry (see Cargo.toml) — the
// mechanism a later unit's own model test (`tests/supervisor_win.rs` or
// similar) is expected to use.
// ---------------------------------------------------------------------

/// A cheap placeholder connection for [`ScriptedProbeOps`] — never
/// touches the OS, and (round-2 finding 7) never implements
/// [`crate::challenge::ChallengeableConnection`] at all: only `ProbeOps::challenge`
/// consumes that trait, and only the REAL implementation (over a real
/// `PipeClient`) ever needs it, so requiring it of every `ProbeOps::Conn`
/// forced this placeholder to carry a fake null handle and three
/// panicking I/O methods nothing ever called. Deleted; this is now a
/// bare marker type `ScriptedProbeOps::challenge` never inspects.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct DummyConn;

/// A cheap placeholder for [`ProbeOps::SpawnedChild`] under
/// [`ScriptedProbeOps`] — carries no real handle at all.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct DummySpawnedChild;

/// A cheap placeholder for [`ProbeOps::Process`] under
/// [`ScriptedProbeOps`] — carries no real handle at all.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct DummyProcess;

/// Scripted `ProbeOps` for a model test to drive one row of the probe
/// classifier's transition table at a time, touching NO real OS object —
/// EXISTS so that test (a later unit) can force each Stage A/B
/// observation deterministically instead of arranging real OS races.
/// Each queue drains in order; a call past the end of its queue panics
/// loudly — a test that runs out of script under-specified its own
/// scenario, rather than a caller that should silently see something
/// arbitrary. The clock starts at a real `Instant::now()` (so arithmetic
/// against it is always valid) but only ever advances when the test
/// tells it to.
#[cfg(any(test, feature = "test-support"))]
pub struct ScriptedProbeOps {
    spawn: std::sync::Mutex<std::collections::VecDeque<SpawnOutcome<DummySpawnedChild>>>,
    wait_child: std::sync::Mutex<std::collections::VecDeque<WaitOutcome>>,
    kill_child: std::sync::Mutex<std::collections::VecDeque<std::io::Result<()>>>,
    connect: std::sync::Mutex<std::collections::VecDeque<ConnectOutcome<DummyConn>>>,
    challenge: std::sync::Mutex<std::collections::VecDeque<ChallengeOutcome<DummyProcess>>>,
    writer_fence_probe: std::sync::Mutex<std::collections::VecDeque<FenceProbe>>,
    wait_exit: std::sync::Mutex<std::collections::VecDeque<WaitOutcome>>,
    terminate: std::sync::Mutex<std::collections::VecDeque<std::io::Result<()>>>,
    now: std::sync::Mutex<Instant>,
}

#[cfg(any(test, feature = "test-support"))]
impl Default for ScriptedProbeOps {
    fn default() -> Self {
        Self {
            spawn: Default::default(),
            wait_child: Default::default(),
            kill_child: Default::default(),
            connect: Default::default(),
            challenge: Default::default(),
            writer_fence_probe: Default::default(),
            wait_exit: Default::default(),
            terminate: Default::default(),
            now: std::sync::Mutex::new(Instant::now()),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedProbeOps {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_spawn(&self, outcome: SpawnOutcome<DummySpawnedChild>) {
        self.spawn.lock().unwrap().push_back(outcome);
    }
    pub fn push_wait_child(&self, outcome: WaitOutcome) {
        self.wait_child.lock().unwrap().push_back(outcome);
    }
    pub fn push_kill_child(&self, outcome: std::io::Result<()>) {
        self.kill_child.lock().unwrap().push_back(outcome);
    }
    pub fn push_connect(&self, outcome: ConnectOutcome<DummyConn>) {
        self.connect.lock().unwrap().push_back(outcome);
    }
    pub fn push_challenge(&self, outcome: ChallengeOutcome<DummyProcess>) {
        self.challenge.lock().unwrap().push_back(outcome);
    }
    pub fn push_writer_fence_probe(&self, outcome: FenceProbe) {
        self.writer_fence_probe.lock().unwrap().push_back(outcome);
    }
    pub fn push_wait_exit(&self, outcome: WaitOutcome) {
        self.wait_exit.lock().unwrap().push_back(outcome);
    }
    pub fn push_terminate(&self, outcome: std::io::Result<()>) {
        self.terminate.lock().unwrap().push_back(outcome);
    }

    /// Set the injected clock to an absolute `Instant` (typically derived
    /// from `self.now()` plus/minus a `Duration`, since `Instant` has no
    /// public constructor of its own).
    pub fn set_now(&self, t: Instant) {
        *self.now.lock().unwrap() = t;
    }
    /// Advance the injected clock by `by`, without a real sleep.
    pub fn advance(&self, by: Duration) {
        let mut n = self.now.lock().unwrap();
        *n += by;
    }

    /// `true` iff every queue is empty — the model test's own final
    /// assertion that its whole script was consumed, not merely that the
    /// rows it happened to check passed.
    pub fn all_exhausted(&self) -> bool {
        self.spawn.lock().unwrap().is_empty()
            && self.wait_child.lock().unwrap().is_empty()
            && self.kill_child.lock().unwrap().is_empty()
            && self.connect.lock().unwrap().is_empty()
            && self.challenge.lock().unwrap().is_empty()
            && self.writer_fence_probe.lock().unwrap().is_empty()
            && self.wait_exit.lock().unwrap().is_empty()
            && self.terminate.lock().unwrap().is_empty()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ProbeOps for ScriptedProbeOps {
    type Conn = DummyConn;
    type SpawnedChild = DummySpawnedChild;
    type Process = DummyProcess;

    fn spawn(&self, _command: &mut std::process::Command) -> SpawnOutcome<DummySpawnedChild> {
        self.spawn.lock().unwrap().pop_front().expect("scripted spawn outcome exhausted")
    }
    fn wait_child(&self, _child: &DummySpawnedChild, _timeout: Duration) -> WaitOutcome {
        self.wait_child.lock().unwrap().pop_front().expect("scripted wait_child outcome exhausted")
    }
    fn kill_child(&self, _child: &DummySpawnedChild) -> std::io::Result<()> {
        self.kill_child.lock().unwrap().pop_front().expect("scripted kill_child outcome exhausted")
    }
    fn connect(&self, _voyage_id: &str) -> ConnectOutcome<DummyConn> {
        self.connect.lock().unwrap().pop_front().expect("scripted connect outcome exhausted")
    }
    fn challenge(&self, _conn: &DummyConn, _deadline: Instant) -> ChallengeOutcome<DummyProcess> {
        self.challenge.lock().unwrap().pop_front().expect("scripted challenge outcome exhausted")
    }
    fn writer_fence_probe(&self, _voyage_root: &Path) -> FenceProbe {
        self.writer_fence_probe.lock().unwrap().pop_front().expect("scripted writer_fence_probe outcome exhausted")
    }
    fn wait_exit(&self, _process: &DummyProcess, _timeout: Duration) -> WaitOutcome {
        self.wait_exit.lock().unwrap().pop_front().expect("scripted wait_exit outcome exhausted")
    }
    fn terminate(&self, _process: &DummyProcess) -> std::io::Result<()> {
        self.terminate.lock().unwrap().pop_front().expect("scripted terminate outcome exhausted")
    }
    fn now(&self) -> Instant {
        *self.now.lock().unwrap()
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
    fn scripted_wait_and_terminate_outcomes_drain_in_order_with_no_real_process() {
        let ops = ScriptedProbeOps::new();
        ops.push_wait_exit(WaitOutcome::StillRunning);
        ops.push_wait_exit(WaitOutcome::Exited);
        ops.push_terminate(Ok(()));
        ops.push_terminate(Err(std::io::Error::other("boom")));

        // A trivial zero-sized DummyProcess -- no pipe, no challenge, no
        // real Windows handle anywhere.
        let process = DummyProcess;
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

    #[test]
    fn injected_clock_advances_only_when_told() {
        let ops = ScriptedProbeOps::new();
        let t0 = ops.now();
        assert_eq!(ops.now(), t0, "the clock must never move on its own");
        ops.advance(Duration::from_secs(120));
        assert_eq!(ops.now(), t0 + Duration::from_secs(120));
    }

    /// The model-test scaffolding proof (ADR 0041 U0 round-1 required
    /// test): every A1-A5/B0-B9 row is drivable through `ScriptedProbeOps`
    /// alone, with NO real pipe, process, or challenge anywhere, and
    /// every queue is exhausted by the end — proving the seam is total
    /// over the classifier's own transition table without shipping any
    /// of that table's decision logic here.
    #[test]
    fn every_probe_table_row_is_drivable_scripted_only() {
        let ops = ScriptedProbeOps::new();
        let unused_cmd = || std::process::Command::new("unused");

        // A1: CreateProcess failed -> SPAWN-FAILED.
        ops.push_spawn(SpawnOutcome::Failed(std::io::Error::other("spawn failed")));
        assert!(matches!(ops.spawn(&mut unused_cmd()), SpawnOutcome::Failed(_)));

        // A2: the child has exited -> LEG ENDED.
        ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
        let SpawnOutcome::Spawned(child) = ops.spawn(&mut unused_cmd()) else { panic!("expected Spawned") };
        ops.push_wait_child(WaitOutcome::Exited);
        assert_eq!(ops.wait_child(&child, Duration::from_secs(1)), WaitOutcome::Exited);

        // A3: WAIT_FAILED, or the readiness cutoff expired -> KILL+WAIT
        // (round-2 finding 5: BOTH halves of the row, not kill alone).
        // The wait-failure half:
        ops.push_wait_child(WaitOutcome::WaitFailed);
        assert_eq!(ops.wait_child(&child, Duration::from_secs(1)), WaitOutcome::WaitFailed);
        // The readiness-cutoff half, via the injected clock -- no sleep:
        let readiness_cutoff = ops.now() + Duration::from_secs(60);
        ops.advance(Duration::from_secs(61));
        assert!(ops.now() >= readiness_cutoff, "the injected clock must be able to reach a blown cutoff");
        // Either half ends in KILL, THEN the post-kill WAIT -- the row
        // is named KILL+WAIT, not KILL alone.
        ops.push_kill_child(Ok(()));
        assert!(ops.kill_child(&child).is_ok());
        ops.push_wait_child(WaitOutcome::Exited);
        assert_eq!(ops.wait_child(&child, Duration::from_secs(1)), WaitOutcome::Exited);

        // A4: alive, within cutoff -> challenge on its pipe -> well-formed
        // status_ok, identity matches -> READY (shares `connect`/
        // `challenge` with Stage B by design).
        ops.push_connect(ConnectOutcome::Connected(DummyConn));
        let ConnectOutcome::Connected(conn) = ops.connect("voy") else { panic!("expected Connected") };
        ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
        assert!(matches!(
            ops.challenge(&conn, ops.now() + Duration::from_secs(2)),
            ChallengeOutcome::Proven(_)
        ));

        // A5: alive, within cutoff -> any OTHER challenge/connect outcome
        // -> PENDING.
        ops.push_connect(ConnectOutcome::Connected(DummyConn));
        let ConnectOutcome::Connected(conn) = ops.connect("voy") else { panic!("expected Connected") };
        ops.push_challenge(ChallengeOutcome::Undetermined);
        assert!(matches!(
            ops.challenge(&conn, ops.now() + Duration::from_secs(2)),
            ChallengeOutcome::Undetermined
        ));

        // B0: episode deadline expired -> WEDGED. Purely the injected
        // clock again -- no real wall-clock wait.
        let episode_deadline = ops.now() + Duration::from_secs(60);
        ops.advance(Duration::from_secs(61));
        assert!(ops.now() >= episode_deadline);

        // B1: connect ok -> well-formed status_ok, identity matches ->
        // ADOPTED.
        ops.push_connect(ConnectOutcome::Connected(DummyConn));
        let ConnectOutcome::Connected(conn) = ops.connect("voy") else { panic!("expected Connected") };
        ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
        assert!(matches!(ops.challenge(&conn, ops.now()), ChallengeOutcome::Proven(_)));

        // B2/B3/B4: connect ok -> SID differs / wrong opcode / undecodable
        // -> all fold to Foreign at this seam's level of observation (the
        // WHY is the challenge's own job, already covered by
        // challenge.rs's own frame tests).
        for _ in 0..3 {
            ops.push_connect(ConnectOutcome::Connected(DummyConn));
            let ConnectOutcome::Connected(conn) = ops.connect("voy") else { panic!("expected Connected") };
            ops.push_challenge(ChallengeOutcome::Foreign);
            assert!(matches!(ops.challenge(&conn, ops.now()), ChallengeOutcome::Foreign));
        }

        // B5: connect ok -> EOF/timeout/read-write error, or an
        // authentication OS-call failure -> PENDING.
        ops.push_connect(ConnectOutcome::Connected(DummyConn));
        let ConnectOutcome::Connected(conn) = ops.connect("voy") else { panic!("expected Connected") };
        ops.push_challenge(ChallengeOutcome::Undetermined);
        assert!(matches!(ops.challenge(&conn, ops.now()), ChallengeOutcome::Undetermined));

        // B6: connect ERROR_ACCESS_DENIED -> FOREIGN.
        ops.push_connect(ConnectOutcome::AccessDenied);
        assert!(matches!(ops.connect("voy"), ConnectOutcome::AccessDenied));

        // B7: connect ERROR_FILE_NOT_FOUND -> writer fence FREE -> ABSENT.
        ops.push_connect(ConnectOutcome::FileNotFound);
        assert!(matches!(ops.connect("voy"), ConnectOutcome::FileNotFound));
        ops.push_writer_fence_probe(FenceProbe::Free);
        assert!(matches!(ops.writer_fence_probe(Path::new("voy")), FenceProbe::Free));

        // B8: connect ERROR_FILE_NOT_FOUND -> fence held, or probing it
        // errored -> PENDING.
        ops.push_connect(ConnectOutcome::FileNotFound);
        assert!(matches!(ops.connect("voy"), ConnectOutcome::FileNotFound));
        ops.push_writer_fence_probe(FenceProbe::Held);
        assert!(matches!(ops.writer_fence_probe(Path::new("voy")), FenceProbe::Held));
        ops.push_connect(ConnectOutcome::FileNotFound);
        assert!(matches!(ops.connect("voy"), ConnectOutcome::FileNotFound));
        ops.push_writer_fence_probe(FenceProbe::Error(std::io::Error::other("probe failed")));
        assert!(matches!(ops.writer_fence_probe(Path::new("voy")), FenceProbe::Error(_)));

        // B9: connect ERROR_PIPE_BUSY, or any other connect error ->
        // PENDING.
        ops.push_connect(ConnectOutcome::PipeBusy);
        assert!(matches!(ops.connect("voy"), ConnectOutcome::PipeBusy));
        ops.push_connect(ConnectOutcome::OtherIo(std::io::Error::other("other")));
        assert!(matches!(ops.connect("voy"), ConnectOutcome::OtherIo(_)));

        assert!(ops.all_exhausted(), "every scripted queue must be fully drained");
    }
}
