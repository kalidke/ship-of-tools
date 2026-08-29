//! `<state-dir>/supervisor.lock`: the public facade over
//! `fsutil::lock_supervisor` (ADR 0041 Lifecycle "one authority, one
//! fence"). `fsutil` itself is a private module — invisible from ANY
//! other crate, including a future `sot-capsule` binary target, which
//! Cargo treats as a SEPARATE crate from this package's library even
//! though they share one `Cargo.toml` (ADR 0041 U0 round-1 blocker 3: a
//! `pub fn` inside a private module is unreachable from outside the
//! defining crate no matter how public the function itself is) — so this
//! is the one place outside `fsutil` that ANY external caller may take
//! the supervisor fence from. Mirrors `pointer::pointer_path`'s
//! discipline: callers pass the STATE DIR; the fixed file name is pinned
//! here, never re-derived by the caller, so two callers can never
//! accidentally mint different-named authority fences for the same
//! drawer.

use crate::fsutil;
use crate::Result;
use std::path::{Path, PathBuf};

const SUPERVISOR_LOCK_FILE_NAME: &str = "supervisor.lock";

/// `<state_dir>/supervisor.lock`.
pub fn supervisor_lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SUPERVISOR_LOCK_FILE_NAME)
}

/// The held supervisor fence — kernel-released on drop (including hard
/// kills), exactly like the voyage writer fence's own guard. A DISTINCT
/// type (not a re-export of `fsutil::WriterLock`), so external callers'
/// code reads as holding THE authority fence, not an unrelated per-voyage
/// writer lock that merely happens to share its kernel mechanics — and so
/// this crate never has to expose a private-module type through a public
/// signature to make the facade work.
pub struct SupervisorLock(#[allow(dead_code)] fsutil::WriterLock); // held for its Drop

/// Acquire the ONE-AUTHORITY fence under `state_dir` — bootstraps it
/// (`CREATE_NEW`) if absent, then takes it with the same kernel-lock
/// mechanics the writer fence uses. See `fsutil::lock_supervisor`'s own
/// doc for the mechanism; this is its only public entry point. Mandatory
/// and cross-process on both platforms (see `fsutil::WriterLock`'s own
/// doc) — the real topology is CROSS-PROCESS (two separate
/// `sot-capsule supervise` instances), and that is exactly what the
/// kernel arbitrates.
pub fn lock_supervisor(state_dir: &Path) -> Result<SupervisorLock> {
    fsutil::lock_supervisor(&supervisor_lock_path(state_dir)).map(SupervisorLock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::time::{Duration, Instant};

    #[test]
    fn lock_supervisor_pins_the_file_name_under_the_given_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let guard = lock_supervisor(dir.path()).unwrap();
        assert!(supervisor_lock_path(dir.path()).is_file());
        drop(guard);
    }

    /// The thread-level guarantee this fence ACTUALLY provides, on both
    /// platforms: a lock already held (sequentially, before a second call
    /// is even attempted) correctly refuses a second acquisition.
    #[test]
    fn lock_supervisor_refuses_a_second_concurrent_holder_through_the_facade() {
        let dir = tempfile::tempdir().unwrap();
        let _held = lock_supervisor(dir.path()).unwrap();
        match lock_supervisor(dir.path()) {
            Err(e) => assert!(format!("{e}").contains("held by another process"), "{e}"),
            Ok(_) => panic!("expected a second lock_supervisor call to fail while the first is held"),
        }
    }

    /// The real concurrent SAME-PROCESS race (round-1 required test;
    /// restored in round 3 with a sound observation after a round-2
    /// false positive).
    ///
    /// ROUND-3 CORRECTED DIAGNOSIS: the round-2 windows-latest CI failure
    /// was NOT a Windows primitive gap (that claim is refuted — Microsoft's
    /// own `LockFileEx` documentation, the normative MS-FSA conflict
    /// algorithm, and Rust std's own Windows implementation all describe
    /// unconditional conflict checking, no same-process exception; an
    /// independent `fsutil` audit found no false-`Ok` path either). It was
    /// a genuine bug in THAT test's own observation: it moved the holder
    /// COUNT's decrement to after `drop(guard)`, so a fully correct
    /// handoff — thread A releases, thread B legitimately acquires and
    /// increments while A's decrement hasn't run yet — read as "2
    /// concurrent holders" even though the kernel lock was NEVER granted
    /// to two callers at once. A lifetime counter samples an interval that
    /// can outlive the real ownership window; it is not a valid witness.
    ///
    /// This version uses NO counter at all: every racing thread's
    /// `Result<SupervisorLock, _>` is collected (not merely its success
    /// bit) and every successful guard is kept ALIVE inside `outcomes`
    /// until every thread has reported. A live guard IS the ownership —
    /// if two threads had genuinely both won, `outcomes` would hold two
    /// live `SupervisorLock`s simultaneously at the point this asserts,
    /// with no window where counting could over- or under-report it.
    #[test]
    fn lock_supervisor_same_process_race_grants_exactly_one_holder_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        const RACERS: usize = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(RACERS));

        let handles: Vec<_> = (0..RACERS)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait(); // force genuinely simultaneous first attempts
                    lock_supervisor(&path)
                })
            })
            .collect();

        // Collect every outcome BEFORE inspecting any of them -- any
        // winning guard stays alive, owned inside `outcomes`, until this
        // point and beyond (it drops only when `outcomes` itself does, at
        // the end of this function).
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let winners = outcomes.iter().filter(|r| r.is_ok()).count();
        if winners != 1 {
            let errors: Vec<String> = outcomes.iter().filter_map(|r| r.as_ref().err().map(ToString::to_string)).collect();
            panic!("exactly one thread must win a concurrent same-process race, got {winners} winners (errors seen: {errors:?})");
        }
    }

    /// Bounded poll for `predicate` — shared by the cross-process race's
    /// ready/go/report coordination below, each with its OWN deadline and
    /// failure message (never a silent infinite wait).
    fn poll_until(mut predicate: impl FnMut() -> bool, timeout: Duration, what: &str) {
        let deadline = Instant::now() + timeout;
        while !predicate() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// The "child" role for the cross-process race below. Env vars (all
    /// required together): `FENCE_XPROC_STATE_DIR` (the fence's own state
    /// dir), `FENCE_XPROC_READY_DIR`/`FENCE_XPROC_GO_FILE` (the start
    /// barrier), `FENCE_XPROC_REPORT_DIR` (per-child outcome files),
    /// `FENCE_XPROC_MARKER` (the sole-ownership witness),
    /// `FENCE_XPROC_CHILD_ID`/`FENCE_XPROC_RACERS` (this child's index and
    /// the total count). A normal test pass never sets these, so this is
    /// a silent no-op then — only the parent test invokes it BY NAME with
    /// them set, in a dedicated child process (the same self-re-exec
    /// shape `tests/pipe_win.rs`'s own cross-process challenge test
    /// uses).
    ///
    /// Protocol, closing the round-3 review's two gaps in the prior
    /// version: (1) NO-BARRIER — signal ready, then wait for the parent's
    /// `go` file, so all children attempt their FIRST acquisition at
    /// genuinely the same time instead of racing sequential `CreateProcess`
    /// scheduling; (2) MARKER-REMOVED-BEFORE-DROP — the winner does not
    /// remove its marker (or drop its guard) until it has independently
    /// confirmed every OTHER child has already written its own report, so
    /// a slow competitor can never be descheduled past the point where the
    /// lock was released and pass without ever having contended for real.
    #[test]
    fn lock_supervisor_cross_process_race_child_role() {
        let Ok(state_dir) = std::env::var("FENCE_XPROC_STATE_DIR") else {
            return;
        };
        let ready_dir = PathBuf::from(std::env::var("FENCE_XPROC_READY_DIR").unwrap());
        let go_file = PathBuf::from(std::env::var("FENCE_XPROC_GO_FILE").unwrap());
        let report_dir = PathBuf::from(std::env::var("FENCE_XPROC_REPORT_DIR").unwrap());
        let marker = PathBuf::from(std::env::var("FENCE_XPROC_MARKER").unwrap());
        let child_id: usize = std::env::var("FENCE_XPROC_CHILD_ID").unwrap().parse().unwrap();
        let racers: usize = std::env::var("FENCE_XPROC_RACERS").unwrap().parse().unwrap();

        // Signal ready, then wait for the parent's start signal.
        std::fs::write(ready_dir.join(format!("ready-{child_id}")), b"").unwrap();
        poll_until(|| go_file.exists(), Duration::from_secs(10), "the parent's go signal");

        match lock_supervisor(Path::new(&state_dir)) {
            Ok(guard) => {
                std::fs::write(report_dir.join(format!("report-{child_id}")), b"ok").unwrap();
                // Sole-ownership witness: if another child ALSO believes
                // it won, this fails loudly rather than deadlocking --
                // both sides of a genuine double-grant race for the
                // marker, and the loser panics instead of proceeding.
                match std::fs::OpenOptions::new().write(true).create_new(true).open(&marker) {
                    Ok(_) => {}
                    Err(e) => panic!("child {child_id}: overlapping holder detected creating the marker: {e}"),
                }
                // Hold until every OTHER child has reported its OWN
                // outcome: by the time this releases, every competitor
                // has already contended against a lock provably still
                // held by this child.
                poll_until(
                    || (0..racers).filter(|&i| i != child_id).all(|i| report_dir.join(format!("report-{i}")).exists()),
                    Duration::from_secs(10),
                    "every competitor to report its outcome",
                );
                std::fs::remove_file(&marker).unwrap();
                drop(guard);
            }
            Err(e) => {
                assert!(format!("{e}").contains("held by another process"), "child {child_id}: unexpected error: {e}");
                std::fs::write(report_dir.join(format!("report-{child_id}")), format!("err:{e}")).unwrap();
            }
        }
    }

    /// The real cross-process race (round-1 required test; hardened in
    /// round 3 per review): production-topology coverage for the ONE
    /// guarantee this fence actually needs to provide (see
    /// `lock_supervisor`'s own doc) -- two separate OS processes, not
    /// threads. Forces genuine contention with a parent-issued start
    /// signal (never hopes sequential `CreateProcess` scheduling happens
    /// to overlap), and independently verifies BOTH that exactly one
    /// child observed `Ok` (via each child's own report file, read after
    /// every child has exited) AND that each child actually ran its ONE
    /// intended test (guarding the "hard-coded test name goes stale, zero
    /// tests matched, cargo test still exits 0" hazard, since exit status
    /// alone cannot catch that).
    #[test]
    fn lock_supervisor_cross_process_race_grants_exactly_one_holder_at_a_time() {
        // Guard against a nested re-exec somehow reaching this branch
        // instead of the dedicated child-role test above.
        if std::env::var("FENCE_XPROC_STATE_DIR").is_ok() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().to_path_buf();
        let ready_dir = dir.path().join("ready");
        let report_dir = dir.path().join("report");
        let go_file = dir.path().join("go");
        let marker = dir.path().join("holder.marker");
        std::fs::create_dir(&ready_dir).unwrap();
        std::fs::create_dir(&report_dir).unwrap();
        let exe = std::env::current_exe().expect("current_exe");

        const RACERS: usize = 4;
        let mut children: Vec<std::process::Child> = (0..RACERS)
            .map(|child_id| {
                std::process::Command::new(&exe)
                    .arg("--exact")
                    .arg("fence::tests::lock_supervisor_cross_process_race_child_role")
                    .arg("--nocapture")
                    .arg("--test-threads=1")
                    .env("FENCE_XPROC_STATE_DIR", &state_dir)
                    .env("FENCE_XPROC_READY_DIR", &ready_dir)
                    .env("FENCE_XPROC_GO_FILE", &go_file)
                    .env("FENCE_XPROC_REPORT_DIR", &report_dir)
                    .env("FENCE_XPROC_MARKER", &marker)
                    .env("FENCE_XPROC_CHILD_ID", child_id.to_string())
                    .env("FENCE_XPROC_RACERS", RACERS.to_string())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .expect("failed to spawn cross-process race child")
            })
            .collect();

        // Parent-issued start signal: only once EVERY child has signaled
        // ready do we let any of them attempt their first acquisition --
        // this is what forces contention instead of hoping for it.
        poll_until(
            || (0..RACERS).all(|i| ready_dir.join(format!("ready-{i}")).exists()),
            Duration::from_secs(10),
            "every child to signal ready",
        );
        std::fs::write(&go_file, b"go").unwrap();

        let outputs: Vec<_> = children.drain(..).map(|c| c.wait_with_output().expect("wait on race child")).collect();

        for (i, out) in outputs.iter().enumerate() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success(),
                "child {i} failed: status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                out.status
            );
            // Guards against the zero-tests-matched hazard: a stale
            // hard-coded test name would make `cargo test` report "0
            // passed" and STILL exit 0, silently turning this whole race
            // into a no-op that always "passes".
            assert!(
                stdout.contains("1 passed"),
                "child {i} did not report exactly one test run (zero-tests-matched hazard?):\n{stdout}"
            );
        }

        // Independent ground truth: read every report file directly,
        // rather than trusting each child's own internal bookkeeping.
        let mut winners = 0usize;
        let mut reports = Vec::new();
        for i in 0..RACERS {
            let mut text = String::new();
            std::fs::File::open(report_dir.join(format!("report-{i}")))
                .unwrap_or_else(|e| panic!("child {i} left no report file: {e}"))
                .read_to_string(&mut text)
                .unwrap();
            if text.trim() == "ok" {
                winners += 1;
            }
            reports.push(text);
        }
        assert_eq!(winners, 1, "exactly one child must observe Ok across the whole race: {reports:?}");
    }
}
