#![cfg(any(target_os = "linux", target_os = "macos", windows))]
//! Integration tests for the capsule runtime (`src/capsule/`, ADR 0041
//! step 4; ADR 0043 "Decisions for LU2": ungated in LU2b once a real Unix
//! producer existed to drive it). Lives
//! in `tests/` for the same reason `tests/conpty.rs` does: one of these
//! (the flood test) needs `env!("CARGO_BIN_EXE_...")` to find its helper
//! binary, which Cargo only wires up for integration test binaries, and
//! the rest are kept here too for one home and one
//! `cargo test -p sot-log --test capsule` filter.
//!
//! Three named targets, not bare `unix`/unconditional: `capsule::run`
//! needs two things a platform either has or does not — a `self_status`
//! (ADR 0043 decision 16) and a store rename arm — and on any Unix that
//! is neither Linux nor macOS both still fail closed with
//! `Error::Unsupported`, so a real `PtyProducer`-driven `capsule::run`
//! call would panic there regardless of which test called it. Both of
//! the reasons this gate ONCE excluded macOS are gone: M1 gave `fsutil`
//! its `renamex_np` arm, and `self_status` has a macOS arm over the
//! `pidversion` the audit token carries. The same two facts widened
//! `capsule/`'s own internal `#[cfg(all(test, ...))] mod tests` gate,
//! which was gated for the identical reason and still is.
//!
//! Nothing in this file reads `/proc`, opens a pidfd or expects
//! PDEATHSIG: the Linux-kernel-specific mechanism lives in
//! `tests/supervisor/`, which keeps its own gate. `unix_only` below is
//! `cfg(unix)` because what it exercises — a real spawn failure, a real
//! signal death, pty geometry — is Unix mechanism, not Linux mechanism,
//! and it now runs on macOS too.
//!
//! The host-handshake byte state machine's own unit tests
//! (`host_handshake.rs`) are pure and run everywhere already; what these
//! tests add is proof the WIRING is correct on a real ConPTY — that a real
//! DA1 answer becomes a well-formed, exactly-once `request`/`response`/
//! `outcome` triple, that a real resize commits a real `outcome` and calls
//! `ResizePseudoConsole` exactly when it should, and that a real spawn
//! failure and a real requested kill both seal a verifiable voyage with
//! `producer_dead` as the last frame in it.
//!
//! Discharge round (Codex review, finding 7): the previous version's flood
//! test asserted byte-count equality across a lossy transform boundary
//! (`hOutput` is conhost's own rendered VT stream, not raw child stdout)
//! and ran `run` on the test's own thread, so a teardown deadlock consumed
//! the whole CI job's timeout instead of failing locally; the handshake
//! test checked membership, not a bijection; the resize test could pass
//! even if every outcome targeted the same request or `ResizePseudoConsole`
//! were never actually gated. All four are fixed below.

#[path = "../support/transports.rs"]
mod transports;

use sot_log::lane::attach_proto::ConnId;
use sot_log::capsule::{self, CapsuleConfig, Command, Ended, ExitKind};
use sot_log::capsule::producer::ExitStatus;
use sot_log::lane::wire::{self, Survival};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use transports::{no_transport, TestTransport};

/// The producer under test, selected in ONE place (ADR 0043 "Decisions for
/// LU2"): every `capsule::run` call in this file names `P`, never a
/// concrete producer directly, so a platform swap touches only this
/// module. `unix` here means Linux ONLY in practice (see the file-level
/// `cfg` gate's own doc): this whole file never compiles on any other
/// Unix.
mod producer_under_test {
    #[cfg(windows)]
    pub type P = sot_log::capsule::producer::conpty::producer::ConptyProducer;
    #[cfg(unix)]
    pub type P = sot_log::capsule::producer::pty::PtyProducer;

    /// argv[0] every test in this file spawns as its shell. A bare
    /// interactive `/bin/sh` stays alive until killed, exactly like
    /// `cmd.exe` — both are read from the pty's own controlling terminal
    /// and simply wait at their own prompt with nothing further to do.
    #[cfg(windows)]
    pub const SHELL_ARGV: &str = "cmd.exe";
    #[cfg(unix)]
    pub const SHELL_ARGV: &str = "/bin/sh";

    /// The helper binary the flood/fidelity tests need —
    /// `env!("CARGO_BIN_EXE_...")` only resolves inside an integration
    /// test binary, which is why this lives here rather than in the
    /// helper's own `src/bin/*.rs`.
    #[cfg(windows)]
    pub const HELPER_EXE: &str = env!("CARGO_BIN_EXE_sot-conpty-helper");
    #[cfg(unix)]
    pub const HELPER_EXE: &str = env!("CARGO_BIN_EXE_sot-pty-helper");
}
use producer_under_test::{HELPER_EXE, SHELL_ARGV, P};

/// A one-shot shell command's own argv, per platform — `cmd.exe /d /c
/// <cmd>` on Windows, `/bin/sh -c <cmd>` on Unix — kept in ONE place so a
/// test that just needs "run this command and exit" (as opposed to a bare
/// interactive shell that stays alive until killed, which every OTHER
/// `SHELL_ARGV`-only call site in this file already is) doesn't hardcode
/// either shell's own flag shape.
fn shell_command(cmd: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        vec![SHELL_ARGV.to_string(), "/d".to_string(), "/c".to_string(), cmd.to_string()]
    }
    #[cfg(unix)]
    {
        vec![SHELL_ARGV.to_string(), "-c".to_string(), cmd.to_string()]
    }
}

/// Every test in this binary spawns a real ConPTY producer plus a capsule
/// writer loop and reader thread. Run CONCURRENTLY (cargo's default) on a
/// two-core CI runner, one test's 20 MiB flood can starve another test's
/// entire process group long enough to blow its pre-admission and wait
/// deadlines — observed as PreAdmissionTimeout on a hello that had been
/// sent, surviving two intra-loop pacing fixes because the contention was
/// never inside one capsule at all. One shared lock makes the heavy tests
/// additive instead of adversarial; poisoning is tolerated so one failing
/// test doesn't cascade.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}


fn config(dir: &std::path::Path, name: &str, argv: Vec<String>, cols: u16, rows: u16) -> CapsuleConfig {
    CapsuleConfig {
        voyage_root: dir.join(name),
        voyage_id: name.to_string(),
        screen_path: Some(dir.join(format!("{name}.screen"))),
        argv,
        cols,
        rows,
        survival: Survival::Normal,
        // No supervisor in this harness -- see `CapsuleConfig::parent_lease`'s own doc.
        parent_lease: None,
    }
}

/// Encode helpers for the attach lane's client frames and the mgmt lane's
/// requests — thin wrappers so tests read as protocol steps, not byte
/// plumbing.
mod frame {
    use super::wire;

    /// The MODERN client's own default (Codex round on #194): hellos at
    /// the current attach proto, which is what every test in this file
    /// that does not care about version negotiation itself should get,
    /// so a checkpoint it collects carries a scrollback ring like a real
    /// current client's would. [`hello_at`] is the explicit-version
    /// escape hatch for tests that DO care (e.g. an old client's v1).
    pub fn hello() -> Vec<u8> {
        hello_at(wire::ATTACH_PROTO_V2)
    }
    pub fn hello_at(proto: u32) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Hello { proto }).unwrap()
    }
    pub fn attach(controller_id: &str) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Attach { controller_id: controller_id.into() }).unwrap()
    }
    pub fn take(controller_id: &str) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Take { controller_id: controller_id.into() }).unwrap()
    }
    pub fn input(controller_id: &str, take_epoch: u64, idem_key: [u8; 16], payload: &[u8]) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Input {
            controller_id: controller_id.into(),
            take_epoch,
            idem_key,
            payload: payload.to_vec(),
        })
        .unwrap()
    }
    pub fn resize(cols: u16, rows: u16) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Resize { cols, rows }).unwrap()
    }
    pub fn mgmt_probe() -> Vec<u8> {
        wire::encode_mgmt_request(&wire::MgmtRequest::Probe).unwrap()
    }
    pub fn mgmt_status() -> Vec<u8> {
        wire::encode_mgmt_request(&wire::MgmtRequest::Status).unwrap()
    }
    pub fn mgmt_shutdown(reason: &str) -> Vec<u8> {
        wire::encode_mgmt_request(&wire::MgmtRequest::Shutdown { reason: reason.into() }).unwrap()
    }
}

/// Polls a `TestTransport`'s sent frames (cursor-based: never re-scans
/// already-seen entries) for one matching `pred`, bounded — a protocol
/// reply is asynchronous relative to the test's own thread, so this is the
/// same "poll with a bound, never a fixed sleep" discipline the existing
/// flood/resize tests already use for `run`'s own completion.
struct FrameWatcher<'a> {
    transport: &'a TestTransport,
    /// One cursor PER CONNECTION into the shared send log. A single
    /// shared cursor was the first version of this fix and regressed
    /// both windows legs deterministically: a wait for conn B that
    /// matches at log position N would advance the shared cursor past
    /// conn A's frames interleaved before N, so a later wait for A
    /// could never see them — the pre-fix full-rescan was
    /// order-tolerant, a single cursor is not. Per-connection cursors
    /// keep the O(new bytes) poll cost while preserving the rescan's
    /// semantics for interleaved streams.
    next_idx: std::collections::HashMap<ConnId, usize>,
}

impl<'a> FrameWatcher<'a> {
    fn new(transport: &'a TestTransport) -> Self {
        Self { transport, next_idx: std::collections::HashMap::new() }
    }

    /// `label` names WHICH expectation this call is waiting for, purely
    /// for the timeout panic -- every wait used to say the same generic
    /// "timed out waiting for an expected frame on conn N" regardless of
    /// which of the dozens of call sites in this file it was, which cost
    /// real time isolating the ground-gate bug (every failure looked
    /// identical until the CI timeline was cross-referenced by hand).
    fn wait_for<T>(
        &mut self,
        label: &'static str,
        conn: ConnId,
        timeout: Duration,
        mut pred: impl FnMut(&wire::DecodedFrame) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + timeout;
        loop {
            // `sent_frames_from`, not `sent_frames`: this is a hot 10ms
            // poll loop, and a full clone on every iteration turns into
            // O(total accumulated bytes) PER POLL once a connection stays
            // subscribed through a large volume (a flood's own driver,
            // once correctly exempt from queue-overflow eviction) --
            // exactly what turned this wait into a real CI timeout despite
            // the underlying protocol machine being correct (PR #139
            // discharge round; see `sent_frames_from`'s own doc).
            let start = *self.next_idx.get(&conn).unwrap_or(&0);
            let new_frames = self.transport.sent_frames_from(start);
            let mut pos = start;
            for (c, bytes) in &new_frames {
                pos += 1;
                if *c != conn {
                    continue;
                }
                let mut s = wire::FrameSplitter::new();
                let (decoded, err) = s.feed(bytes);
                assert_eq!(err, None, "unexpected wire error in a self-encoded frame");
                for f in &decoded {
                    if let Some(v) = pred(f) {
                        // Consume up to and including the matched entry
                        // for THIS connection only; other connections'
                        // cursors are untouched, so their interleaved
                        // earlier frames stay findable.
                        self.next_idx.insert(conn, pos);
                        return v;
                    }
                }
            }
            self.next_idx.insert(conn, pos);
            if Instant::now() >= deadline {
                panic!("timed out waiting for {label:?} on conn {conn}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Collects a full checkpoint transfer's bytes for `conn` (one or more
    /// `checkpoint_chunk` frames, concatenated through `last`). `label`
    /// passes straight through to `wait_for`'s own timeout message.
    fn collect_checkpoint(&mut self, label: &'static str, conn: ConnId, timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
            let (last, bytes) = self.wait_for(label, conn, remaining, |f| {
                if let wire::DecodedFrame::AttachServer(wire::AttachServer::CheckpointChunk { last, bytes }) = f {
                    Some((*last, bytes.clone()))
                } else {
                    None
                }
            });
            out.extend(bytes);
            if last {
                return out;
            }
        }
    }
}

/// The `ended` file the leg left in the folder at `root`.
fn ended_of(root: &std::path::Path) -> Ended {
    capsule::read_ended(root).unwrap().expect("the leg left no ended file")
}

/// Whether the leg's `ended` file says an end was asked for (written when the request is processed, while the leg runs).
fn end_asked_for(root: &std::path::Path) -> bool {
    capsule::read_ended(root).ok().flatten().is_some_and(|e| e.requested)
}

/// What the screen file the leg left for `name` shows, as text: the saved screen restored into a parser, rows joined
/// by newlines.
fn saved_screen_text(dir: &std::path::Path, name: &str) -> String {
    let bytes = std::fs::read(dir.join(format!("{name}.screen"))).expect("the leg left no screen file");
    let mut parser = vt100_ctt::Parser::new(25, 80, capsule::CAPSULE_SCROLLBACK_ROWS);
    parser.restore_screen(&bytes).expect("the screen file restores");
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    (0..rows)
        .map(|r| (0..cols).filter_map(|c| screen.cell(r, c).map(|cell| cell.contents().to_string())).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Bounded join: `run` blocks until the run ends, and a bug in the
/// teardown sequence's own ordering is exactly the class of bug that would
/// hang it forever — a test must fail loud within a bounded wait, never
/// hang the suite (or, worse, the whole CI job's own timeout — review
/// finding on the flood test specifically).
fn wait_for_join<T: Send + 'static>(handle: std::thread::JoinHandle<T>, timeout: Duration) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Some(handle.join().unwrap())
}

/// Test 1: E2E. A one-shot shell command (`cmd.exe /d /c echo <marker>` on Windows, `/bin/sh -c 'echo <marker>'` on
/// Unix — see `shell_command`'s own doc) runs to completion (a natural producer exit — no `Kill` ever sent); the leg
/// leaves a finished `ended` file that asked for nothing, and its screen file shows the marker.
#[test]
fn e2e_runs_and_leaves_its_ending_and_its_screen() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let marker = "SOT_CAPSULE_WIN_E2E_9f31";
    let argv = shell_command(&format!("echo {marker}"));
    let cfg = config(dir.path(), "e2e1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let mut transport = no_transport();
    let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    assert_eq!(summary.exit_code, Some(ExitStatus::Code(0)));

    let ended = ended_of(&root);
    assert!(ended.finished && !ended.requested, "a natural exit asks for nothing: {ended:?}");
    assert_eq!(ended.exit_code, Some(0));
    assert!(ended.uptime_ms.is_some());
    let text = saved_screen_text(dir.path(), "e2e1");
    assert!(text.contains(marker), "got: {text:?}");
}

/// Test 2b: an out-of-budget INITIAL geometry is treated like a spawn failure: "Initial geometry is validated by the
/// same rule" a resize is (ADR 0041). Nothing ran, and `ended` says so.
#[test]
fn spawn_failure_from_out_of_budget_initial_geometry() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = shell_command("exit 0");
    let cfg = config(dir.path(), "fail2", argv, 1, 25); // cols=1 < the 2-column floor
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let mut transport = no_transport();
    let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::SpawnFailed);
    let ended = ended_of(&root);
    assert!(ended.finished && !ended.requested);
    assert!(ended.reason.as_deref().is_some_and(|r| r.starts_with("spawn_failed")), "{ended:?}");
}

/// Test 3: a requested kill (`Command::Kill`) tears down a still-running producer through the ONE orchestrator, and
/// the leg's `ended` says the end was asked for, with the exit status `run` itself observed.
#[test]
fn requested_kill_tears_down_and_says_so() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // bare interactive shell — stays open until killed
    let cfg = config(dir.path(), "kill1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut transport = no_transport();
        capsule::run::<P>(cfg, rx, &mut transport)
    });
    std::thread::sleep(Duration::from_millis(500));
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);

    let ended = ended_of(&root);
    assert!(ended.finished && ended.requested, "{ended:?}");
    assert_eq!(ended.reason.as_deref(), Some("operator_kill"));
    // A job-imposed exit code on Windows, a `killpg(SIGKILL)`-imposed signal on Unix (ADR 0043 decision 13: the two
    // are mutually exclusive additive fields) -- either way, the file matches what `run` itself observed.
    match summary.exit_code.expect("a real exit status") {
        ExitStatus::Code(code) => assert_eq!(ended.exit_code, Some(code)),
        ExitStatus::Signal(n) => {
            assert_eq!(ended.signal, Some(n));
            assert_eq!(ended.exit_code, None, "a signal death must never also carry an exit code");
        }
    }
}

mod attach;
mod shutdown;
mod v3_order;
mod output_end;

// ---------------------------------------------------------------------
// ADR 0043 "Decisions for LU2": the five tests that exercise real
// Windows-only mechanism (ConPTY spawn failure/geometry, ResizePseudoConsole,
// the output budget's own blocking bound under a real flood, and a
// reader-chunk-boundary replay) rather than the portable
// writer-loop/AttachProto contract every other test in this file proves.
// Grouped under ONE module (LU2b, per the plan this module's own doc
// already stated): this module gains #[cfg(windows)], in place of the
// file-level #![cfg(windows)] the file dropped.
// ---------------------------------------------------------------------
#[cfg(windows)]
mod windows_only;

// ---------------------------------------------------------------------
// ADR 0043 "Decisions for LU2" LU2b: the five tests that exercise real
// Unix-only mechanism (a real spawn failure, `wait()` observing a natural
// exit rather than a fatal early EOF -- decision 12's own proof, a real
// signal death -- decision 13/14, the pty geometry actually moving on a
// wire resize, and the held-EOF gate's own release) -- the Unix twin of
// `windows_only` above, same count (five), same "portable contract vs
// real platform mechanism" split.
// ---------------------------------------------------------------------
#[cfg(unix)]
mod unix_only;

mod no_record;
mod screen_io;
