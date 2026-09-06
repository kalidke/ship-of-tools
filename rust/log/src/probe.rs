//! Fault-injection scaffolding for the probe classifier's own model test
//! (a LATER unit — ADR 0041 "The probe", the Stage A/B transition table
//! under "Lifecycle"). This module ships NO decision logic and NO
//! classifier: only a seam, behind one trait, over every kind of
//! OS-facing operation that table consults — spawn, wait/kill on the
//! OWNED pre-proof child, connect, challenge, a writer-fence probe, and
//! an injectable clock — so a model test can drive every row (A1-A5,
//! B0-B9) deterministically with a SCRIPTED implementation that touches
//! NO real OS object, while the shipped classifier drives the SAME trait
//! with a REAL one. This module is the platform-neutral core: the
//! mechanical outcome enums, the [`ProbeOps`] trait itself, and the
//! scripted test support (`ScriptedProbeOps` and its dummy types) — the
//! REAL implementation over actual OS objects (`RealProbeOps`,
//! `SpawnedChild`) is necessarily platform-specific and lives in
//! `probe_win.rs` today; a `probe_unix.rs` counterpart is a later L1-unix
//! unit.
//!
//! U0 SCOPE: the seam and both implementations (real and scripted). Which
//! observation MEANS `READY`/`ABSENT`/`FOREIGN`/`PENDING`/... is the
//! classifier's own call (a later unit) and is deliberately absent here.
//!
//! # Round-1 review: associated types, not concrete OS objects
//!
//! `ProbeOps` is generic over three associated types (`Conn`,
//! `SpawnedChild`, `Process`) rather than hard-wiring `PipeClient` /
//! `ChallengedProcess`. `probe_win::RealProbeOps` binds them to the real
//! Windows types; `ScriptedProbeOps` binds them to zero-sized dummy types
//! that never touch the OS at all — so a model test can construct every
//! row without a real pipe, a real spawned process, or a real challenge.

use crate::challenge::ChallengeOutcome;
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
    /// evidence) — `crate::challenge_win::ChallengedProcess` for the real
    /// impl.
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

    /// A4's own identity check (Codex review round 1, finding 10): the
    /// OWNED child's own `(pid, creation time)`, read independently of
    /// whatever a challenge over its pipe observed. `Err` means the
    /// identity could not even be read (the handle itself failed an OS
    /// call) — `probe_owned_spawn` treats that identically to a proven
    /// mismatch: fail closed, never trust an unverifiable `Proven`.
    fn spawned_identity(&self, child: &Self::SpawnedChild) -> std::io::Result<(u32, u64)>;
    /// The SAME `(pid, creation time)` shape, read off an ALREADY-PROVEN
    /// process — what the challenge itself already verified (against the
    /// connection's own OS-level peer info), with no further OS call.
    /// `probe_owned_spawn`'s A4 arm compares this against
    /// `spawned_identity` above to tell "the pipe now answers, and it's
    /// OUR child" apart from "the pipe now answers, but with someone
    /// else's leftover process."
    fn proven_identity(&self, process: &Self::Process) -> (u32, u64);

    /// An injectable clock. B0/readiness cutoffs are measured against
    /// THIS, never `Instant::now()` directly, in any code that consumes
    /// `ProbeOps` — so a model test can deterministically reach any
    /// elapsed-time observation (an expired episode deadline, a blown
    /// readiness cutoff) without a real sleep.
    fn now(&self) -> Instant;
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
    spawned_identity: std::sync::Mutex<std::collections::VecDeque<std::io::Result<(u32, u64)>>>,
    proven_identity: std::sync::Mutex<std::collections::VecDeque<(u32, u64)>>,
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
            spawned_identity: Default::default(),
            proven_identity: Default::default(),
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
    pub fn push_spawned_identity(&self, outcome: std::io::Result<(u32, u64)>) {
        self.spawned_identity.lock().unwrap().push_back(outcome);
    }
    pub fn push_proven_identity(&self, identity: (u32, u64)) {
        self.proven_identity.lock().unwrap().push_back(identity);
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
            && self.spawned_identity.lock().unwrap().is_empty()
            && self.proven_identity.lock().unwrap().is_empty()
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
    fn spawned_identity(&self, _child: &DummySpawnedChild) -> std::io::Result<(u32, u64)> {
        self.spawned_identity.lock().unwrap().pop_front().expect("scripted spawned_identity outcome exhausted")
    }
    fn proven_identity(&self, _process: &DummyProcess) -> (u32, u64) {
        self.proven_identity.lock().unwrap().pop_front().expect("scripted proven_identity outcome exhausted")
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
