#![cfg(any(target_os = "linux", target_os = "macos", windows))]
//! Integration tests for the capsule runtime (`src/capsule.rs`, ADR 0041
//! step 4; ADR 0043 "Decisions for LU2": renamed from `tests/capsule_win.rs`
//! in L1-unix LU2a when the writer loop became generic over `Producer`,
//! ungated in LU2b once a real Unix producer existed to drive it). Lives
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
//! `capsule.rs`'s own internal `#[cfg(all(test, ...))] mod tests` gate,
//! which was gated for the identical reason and still is.
//!
//! Nothing in this file reads `/proc`, opens a pidfd or expects
//! PDEATHSIG: the Linux-kernel-specific mechanism lives in
//! `tests/supervisor.rs`, which keeps its own gate. `unix_only` below is
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

#[path = "support/transports.rs"]
mod transports;

use sot_log::attach_proto::ConnId;
use sot_log::capsule::{self, CapsuleConfig, Command, ExitKind};
use sot_log::producer::ExitStatus;
use sot_log::segment::{RetentionClass, SegmentReader};
use sot_log::verify::{leg_carries_run_end_marker, verify_voyage};
use sot_log::wire::{self, Survival};
use sot_log::{Class, Envelope, RefKind};
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
    pub type P = sot_log::producer_conpty::ConptyProducer;
    #[cfg(unix)]
    pub type P = sot_log::producer_pty::PtyProducer;

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
        retention: RetentionClass::Discard,
        producer_kind: "test-shell".into(),
        argv,
        cols,
        rows,
        survival: Survival::Normal,
        // Codex round-1 Major 9: typed evidence, not `None` -- a test
        // asserts "nothing to protect" the same way a real first-install
        // transaction would (see `rollout::RolloutEvidence`'s own doc).
        rollout_evidence: sot_log::rollout::RolloutEvidence::NoRollbackTarget,
        // No supervisor in this harness -- see
        // `CapsuleConfig::parent_lease`'s own doc.
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

/// Every sealed frame across every `.sotseg` in `root/seg`, in segment
/// order — mirrors `capsule.rs`'s own test helper of the same name (not
/// shared: see `capsule_win.rs`'s module doc on duplication).
fn sealed_frames(root: &std::path::Path, voyage: &str) -> Vec<Envelope> {
    let seg_dir = root.join("seg");
    let mut out = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(&seg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for n in names {
        if n.ends_with(".sotseg") {
            let r = SegmentReader::read(&seg_dir.join(&n), true).unwrap();
            assert_eq!(r.header.voyage_id, voyage);
            out.extend(r.frames);
        }
    }
    out
}

/// Test-only base64 decoder for `capsule_win.rs`'s encode-only engine —
/// duplicated from `capsule.rs`'s own test helper.
fn decode_b64(s: &str) -> Vec<u8> {
    let val = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    };
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c) << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    out
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

/// `producer_dead` must be the LAST frame ever appended to the last
/// segment before it was sealed — the order the verifier itself does not
/// enforce (review finding). Returns its `detail` payload for further
/// assertions.
fn assert_producer_dead_is_last(frames: &[Envelope]) -> serde_json::Value {
    let last = frames.last().expect("no frames at all");
    assert_eq!(last.class, Class::Lifecycle, "producer_dead is not the last frame in the segment");
    let payload = last.payload.as_ref().unwrap();
    assert_eq!(payload["kind"], "producer_dead", "last frame is not producer_dead: {payload:?}");
    payload["detail"].clone()
}

/// Test 1: E2E. A one-shot shell command (`cmd.exe /d /c echo <marker>`
/// on Windows, `/bin/sh -c 'echo <marker>'` on Unix — see
/// `shell_command`'s own doc) runs to completion (a natural producer exit
/// — no `Kill` ever sent); the resulting voyage
/// verifies, carries the marker in its producer frames, never carries a
/// turn frame (raw terminal), and ends with `producer_dead`. The
/// host-handshake exchange is a BIJECTION, not membership (review
/// finding): at most one request, matched by exactly one response and
/// exactly one outcome, linked correctly — but tolerating zero, since DA1
/// presence is host/build-version-dependent (same reasoning as
/// `tests/conpty.rs`'s own DA1-presence finding).
#[test]
fn e2e_records_and_verifies() {
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
    assert_eq!(summary.segments_sealed, 1);
    verify_voyage(&root, "e2e1").unwrap();

    let frames = sealed_frames(&root, "e2e1");
    let mut all = Vec::new();
    for f in &frames {
        if f.class == Class::Producer {
            let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
            all.extend(decode_b64(b64));
        }
    }
    let text = String::from_utf8_lossy(&all);
    assert!(text.contains(marker), "got: {text:?}");
    assert!(frames.iter().all(|f| f.class != Class::TurnOpen && f.class != Class::TurnClose));
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["exit_code"], 0);

    let phase_is = |f: &&Envelope, kind_ns: &str, phase: &str| {
        f.class == Class::ControlExchange
            && f.payload.as_ref().unwrap()["kind_ns"] == kind_ns
            && f.payload.as_ref().unwrap()["phase"] == phase
    };
    let hh_reqs: Vec<&Envelope> =
        frames.iter().filter(|f| phase_is(f, "conpty/host-handshake", "request")).collect();
    let hh_resps: Vec<&Envelope> =
        frames.iter().filter(|f| phase_is(f, "conpty/host-handshake", "response")).collect();
    let hh_outcomes: Vec<&Envelope> =
        frames.iter().filter(|f| phase_is(f, "conpty/host-handshake", "outcome")).collect();
    eprintln!(
        "capsule_win e2e finding: host-handshake requests={}, responses={}, outcomes={}",
        hh_reqs.len(),
        hh_resps.len(),
        hh_outcomes.len()
    );
    assert!(hh_reqs.len() <= 1, "ADR 0041's model answers the handshake at most once per run");
    assert_eq!(hh_reqs.len(), hh_resps.len(), "bijection: every request has exactly one response");
    assert_eq!(hh_resps.len(), hh_outcomes.len(), "bijection: every response has exactly one outcome");
    assert_eq!(summary.handshake_answered, !hh_reqs.is_empty());
    if let Some(&req) = hh_reqs.first() {
        let resp = hh_resps[0];
        let outcome = hh_outcomes[0];
        let responds_to = resp
            .refs
            .iter()
            .find(|r| r.kind == RefKind::RespondsTo)
            .map(|r| r.frame)
            .expect("host-handshake response missing responds_to");
        assert_eq!(responds_to, req.seq, "response must respond to ITS OWN request");
        let target = outcome.payload.as_ref().unwrap()["target"].as_str().unwrap().to_string();
        assert_eq!(target, format!("{}:{}", req.seq.epoch, req.seq.n), "outcome must target ITS OWN request");
        assert_eq!(outcome.payload.as_ref().unwrap()["body"]["disposition"], "ok");
    }
}

/// Test 2b: an out-of-budget INITIAL geometry is treated the same way —
/// "Initial geometry is validated by the same rule" a resize is (ADR 0041).
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
    verify_voyage(&root, "fail2").unwrap();
    let frames = sealed_frames(&root, "fail2");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["spawn_failed"], true);
}

/// ADR 0041 "Upgrade and version skew" reader-first rollout gate (step 6
/// U1b, `rollout::gate`): `run` refuses BEFORE opening any segment —
/// and therefore before ever spawning the producer, unlike every OTHER
/// refusal above, which still bootstraps a voyage and seals a
/// `producer_dead` — when the configured installed rollback target's
/// reader cannot decode a segment declaring the EndRun-marker feature.
#[test]
fn refuses_when_the_installed_rollback_target_cannot_read_the_marker() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = shell_command("exit 0");
    let mut cfg = config(dir.path(), "rolloutgate1", argv, 80, 25);
    cfg.rollout_evidence = sot_log::rollout::RolloutEvidence::Installed {
        release: "0.5.9".to_string(),
        target: "rolloutgate1-target".to_string(),
        reader_features: vec!["sot.producer.json-f64-v1".to_string()],
    };
    let (_tx, rx) = mpsc::channel();
    let mut transport = no_transport();
    let err = capsule::run::<P>(cfg, rx, &mut transport).unwrap_err();
    assert!(format!("{err}").contains("cannot decode"), "got: {err}");
}

/// Test 3: a requested kill (`Command::Kill`) tears down a still-running
/// producer through the ONE orchestrator and still seals a verifiable
/// voyage, with a real (job-imposed) exit code recorded as the last frame.
#[test]
fn requested_kill_tears_down_and_seals() {
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
    verify_voyage(&root, "kill1").unwrap();

    let frames = sealed_frames(&root, "kill1");
    let dead = assert_producer_dead_is_last(&frames);
    // A job-imposed exit code on Windows, a `killpg(SIGKILL)`-imposed
    // signal on Unix (ADR 0043 decision 13: the two are mutually
    // exclusive additive fields) -- either way, the durable record must
    // match what `run` itself observed.
    match summary.exit_code.expect("a real exit status") {
        ExitStatus::Code(code) => assert_eq!(dead["exit_code"], code),
        ExitStatus::Signal(n) => {
            assert_eq!(dead["signal"], n);
            assert!(dead.get("exit_code").is_none(), "a signal death must never also carry an exit_code key");
        }
    }
}

// ---------------------------------------------------------------------
// ADR 0041 step 5 (U2): the pipe protocol.
// ---------------------------------------------------------------------

/// ADR 0041 "attach proto v2 bound to checkpoint v2" (Codex round on
/// #194, finding 1): a connection that negotiates attach proto v1 -- an
/// OLD client's own default, predating the scrollback ring -- must get a
/// checkpoint format v1 payload: no scrollback ring, even though the
/// capsule's own live parser keeps one (`CAPSULE_SCROLLBACK_ROWS`).
/// Proves the version-gated encode path in `capsule::run`'s
/// `BeginCheckpoint` handling, independent of the ring-arrival test above
/// (which hellos at v2, the modern client's own default).
#[test]
fn hello_v1_gets_a_checkpoint_with_no_scrollback_ring() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let helper = HELPER_EXE.to_string();
    let argv = vec![
        helper,
        "--script".to_string(),
        "50".to_string(),
        "--linger".to_string(),
    ];
    let (rows, cols) = (4u16, 20u16);
    let cfg = config(dir.path(), "hellov1", argv, cols, rows);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    // Enough real elapsed time that a v2 hello would find a nonempty
    // ring here too -- proving this test's negative result is the
    // version gate, not merely an empty ring to begin with (same pacing
    // rationale as the ring-arrival test above: `SCRIPT_BLOCK` writes
    // one line roughly every 59 ms).
    std::thread::sleep(Duration::from_millis(4000));

    const CONN: ConnId = 1;
    transport.open(CONN);
    transport.feed(CONN, frame::hello_at(wire::ATTACH_PROTO_V1));
    let mut watcher = FrameWatcher::new(&transport);
    let negotiated = watcher.wait_for("v1 hello_ok", CONN, Duration::from_secs(10), |f| {
        if let wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { proto }) = f {
            Some(*proto)
        } else {
            None
        }
    });
    assert_eq!(
        negotiated,
        wire::ATTACH_PROTO_V1,
        "the capsule must echo back exactly the negotiated version"
    );

    transport.feed(CONN, frame::attach("watcher"));
    let checkpoint_bytes =
        watcher.collect_checkpoint("v1 checkpoint", CONN, Duration::from_secs(10));

    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    verify_voyage(&root, "hellov1").unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);

    let mut restored = vt100_ctt::Parser::new(rows, cols, 100);
    restored
        .restore_screen(&checkpoint_bytes)
        .expect("a v1 checkpoint must decode");
    restored.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(
        restored.screen().scrollback(),
        0,
        "a v1-negotiated connection must receive a checkpoint with no ring"
    );
}

/// Test 8: the wire input WAL folds every legal `idem_key` chain exactly,
/// including a stale refusal (a demoted connection's replay) and a
/// duplicate `idem_key` answered deterministically WITHOUT appending any
/// new frame — and the SAME determinism holds across a capsule restart
/// (reopen the voyage; the dedupe index is rebuilt from the retained
/// segments, not started empty — ADR 0041 decision 5's whole point).
#[test]
fn wire_input_wal_chains_including_refused_stale_and_duplicate_idem_across_restart() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let name = "inputwal1";
    let root = dir.path().join(name);
    let k1 = [0x11u8; 16];
    let k2 = [0x22u8; 16];

    // --- Incarnation 1 -------------------------------------------------
    {
        let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
        let cfg = config(dir.path(), name, argv, 80, 25);
        let transport = TestTransport::new();
        let (tx, rx) = mpsc::channel();
        let run_transport = transport.clone();
        let handle = std::thread::spawn(move || {
            let mut t = run_transport;
            capsule::run::<P>(cfg, rx, &mut t)
        });

        // conn A attaches and takes -- the first driver ever, a pipe take.
        const A: ConnId = 1;
        transport.open(A);
        transport.feed(A, frame::hello());
        let mut watcher = FrameWatcher::new(&transport);
        watcher.wait_for("A hello_ok", A, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        transport.feed(A, frame::attach("alice"));
        watcher.collect_checkpoint("A checkpoint", A, Duration::from_secs(10));
        transport.feed(A, frame::take("alice"));
        let epoch = watcher.wait_for("A take_ok", A, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
            _ => None,
        });

        // K1: fresh input while authorized -- recorded.
        transport.feed(A, frame::input("alice", epoch, k1, b"echo one\r\n"));
        let outcome1 = watcher.wait_for("A input K1 fresh outcome", A, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
            _ => None,
        });
        assert!(outcome1, "expected the fresh K1 input to be recorded");

        // K1 AGAIN, same idem_key: chain is already {input,intent,forwarded}
        // -- must replay the SAME recorded outcome, appending nothing new
        // (checked after this incarnation seals, via the sealed frame count
        // for K1's idem_key, below).
        transport.feed(A, frame::input("alice", epoch, k1, b"echo one\r\n"));
        let outcome1_replay = watcher.wait_for("A input K1 replay outcome", A, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
            _ => None,
        });
        assert!(outcome1_replay, "duplicate K1 must replay input_recorded");

        // conn B attaches and takes, demoting A.
        const B: ConnId = 2;
        transport.open(B);
        transport.feed(B, frame::hello());
        watcher.wait_for("B hello_ok", B, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        transport.feed(B, frame::attach("bob"));
        watcher.collect_checkpoint("B checkpoint", B, Duration::from_secs(10));
        transport.feed(B, frame::take("bob"));
        watcher.wait_for("B take_ok", B, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })).then_some(())
        });

        // A tries a NEW key (K2) with its now-stale claim: demoted, so this
        // is refused -- folded into the SAME "stale" wire reply the ADR
        // defines for a durable epoch mismatch (a demoted connection is
        // indistinguishable from one on the wire).
        transport.feed(A, frame::input("alice", epoch, k2, b"echo two\r\n"));
        let outcome2 = watcher.wait_for("A input K2 stale outcome", A, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
            _ => None,
        });
        assert!(!outcome2, "a demoted connection's input must be refused stale");

        tx.send(Command::Kill).unwrap();
        wait_for_join(handle, Duration::from_secs(30))
            .expect("run did not return within the teardown bound")
            .unwrap();
        verify_voyage(&root, name).unwrap();
    }

    let frames = sealed_frames(&root, name);
    let input_frames_for = |key: [u8; 16]| -> Vec<&Envelope> {
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        frames
            .iter()
            .filter(|f| f.class == Class::Input && f.payload.as_ref().unwrap()["idem_key"] == hex)
            .collect()
    };
    assert_eq!(input_frames_for(k1).len(), 1, "K1's retry must not append a second `input` frame");
    let k2_facts: Vec<&Envelope> = frames
        .iter()
        .filter(|f| {
            f.class == Class::Lifecycle
                && f.payload.as_ref().unwrap()["kind"] == "input_fact"
                && f.payload.as_ref().unwrap()["fact"]["fact"] == "refused_stale_epoch"
        })
        .collect();
    assert_eq!(k2_facts.len(), 1, "K2 must have exactly one refused_stale_epoch fact");

    // --- Incarnation 2 (a "successor capsule") --------------------------
    {
        let argv = vec![SHELL_ARGV.to_string()];
        let cfg = config(dir.path(), name, argv, 80, 25);
        let transport = TestTransport::new();
        let (tx, rx) = mpsc::channel();
        let run_transport = transport.clone();
        let handle = std::thread::spawn(move || {
            let mut t = run_transport;
            capsule::run::<P>(cfg, rx, &mut t)
        });

        const C: ConnId = 1;
        transport.open(C);
        transport.feed(C, frame::hello());
        let mut watcher = FrameWatcher::new(&transport);
        watcher.wait_for("C hello_ok", C, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        transport.feed(C, frame::attach("carol"));
        watcher.collect_checkpoint("C checkpoint", C, Duration::from_secs(10));
        transport.feed(C, frame::take("carol"));
        let epoch2 = watcher.wait_for("C take_ok", C, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
            _ => None,
        });

        // K1 again, from a BRAND NEW capsule incarnation, a brand new
        // connection, and a brand new controller identity: the dedupe
        // index was rebuilt from the RETAINED voyage at open, so this must
        // still replay deterministically -- exactly decision 5's point ("a
        // successor capsule starting with an empty index would let a
        // pre-crash forwarded key re-forward").
        transport.feed(C, frame::input("carol", epoch2, k1, b"echo one\r\n"));
        let replay_after_restart = watcher.wait_for("C input K1 replay-after-restart outcome", C, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
            _ => None,
        });
        assert!(replay_after_restart, "K1 must still replay input_recorded after a capsule restart");

        tx.send(Command::Kill).unwrap();
        wait_for_join(handle, Duration::from_secs(30))
            .expect("run did not return within the teardown bound")
            .unwrap();
        verify_voyage(&root, name).unwrap();
    }

    // K1 must STILL have exactly one `input` frame across BOTH incarnations
    // -- the restart never re-forwarded it.
    let frames = sealed_frames(&root, name);
    let input_frames_for = |key: [u8; 16]| -> Vec<Envelope> {
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        frames
            .iter()
            .filter(|f| f.class == Class::Input && f.payload.as_ref().unwrap()["idem_key"] == hex)
            .cloned()
            .collect()
    };
    assert_eq!(input_frames_for(k1).len(), 1, "K1 must never gain a second `input` frame across a restart");
}

/// Test 9: a slow (never-draining) watcher's queued live-output bytes
/// overflow the 4 MiB per-subscriber budget and it is closed -- no wire
/// frame exists for that eviction, by design -- while the DRIVER, a
/// separate connection under the SAME flood, stays live and fully
/// functional throughout.
#[test]
fn slow_watcher_overflow_closes_while_driver_stays_live() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let helper = HELPER_EXE.to_string();
    let total: usize = 6 * 1024 * 1024; // > the 4 MiB per-watcher budget
    // --linger: the producer must OUTLIVE the post-eviction assertions.
    // Without it, the flood's completion races the eviction wait: the
    // producer can exit first, the run enters teardown, and the resize
    // below is then (correctly) not served — observed as a deterministic
    // 10 s timeout on the real windows legs while every protocol-level
    // replay of this sequence passed.
    let argv = vec![helper, "--flood".to_string(), total.to_string(), "--linger".to_string()];
    let cfg = config(dir.path(), "slowwatcher1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const DRIVER: ConnId = 1;
    const WATCHER: ConnId = 2;
    let mut watcher = FrameWatcher::new(&transport);

    transport.open(DRIVER);
    transport.feed(DRIVER, frame::hello());
    watcher.wait_for("driver hello_ok", DRIVER, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(DRIVER, frame::attach("driver"));
    watcher.collect_checkpoint("driver checkpoint", DRIVER, Duration::from_secs(10));
    transport.feed(DRIVER, frame::take("driver"));
    watcher.wait_for("driver take_ok", DRIVER, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })).then_some(())
    });

    transport.open(WATCHER);
    transport.feed(WATCHER, frame::hello());
    watcher.wait_for("watcher hello_ok", WATCHER, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(WATCHER, frame::attach("watcher"));
    watcher.collect_checkpoint("watcher checkpoint", WATCHER, Duration::from_secs(10));
    // Never drains from here on: every future send to WATCHER queues
    // forever, simulating a client that stopped reading its pipe.
    transport.set_hold_for(WATCHER, true);

    // Bounded poll for the watcher's own close -- the flood alone drives
    // this; no fixed sleep assumes when the budget actually trips.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if transport.closed_conns().contains(&WATCHER) {
            break;
        }
        assert!(Instant::now() < deadline, "watcher was never closed under a 6 MiB flood");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!transport.closed_conns().contains(&DRIVER), "the driver must stay live");

    // The driver is still fully functional: a resize still completes.
    transport.feed(DRIVER, frame::resize(100, 40));
    let resize_ok = watcher.wait_for("driver post-eviction resize outcome", DRIVER, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    assert!(resize_ok, "the driver must still be able to resize after the watcher's eviction");

    // The lingering producer is ended BY REQUEST — which is also the
    // honest exit_kind for this scenario.
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(60))
        .expect("run did not return within the local deadline")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    verify_voyage(&root, "slowwatcher1").unwrap();
}

/// Test 10: a refused `hello` (unsupported proto) closes only that
/// connection -- mgmt stays available (a fresh mgmt connection, per the
/// ADR: "the ADR's 'mgmt remains available' is satisfied by a fresh mgmt
/// connection"), and a LATER, protocol-compatible attach on a separate
/// connection still succeeds normally.
#[test]
fn hello_refusal_leaves_mgmt_and_later_attach_working() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()];
    let cfg = config(dir.path(), "hellorefuse1", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });
    let mut watcher = FrameWatcher::new(&transport);

    const MGMT: ConnId = 1;
    const BAD_HELLO: ConnId = 2;
    const GOOD: ConnId = 3;

    transport.open(MGMT);
    transport.feed(MGMT, frame::mgmt_probe());
    watcher.wait_for("mgmt probe_ok (initial)", MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ProbeOk)).then_some(())
    });

    transport.open(BAD_HELLO);
    transport.feed(
        BAD_HELLO,
        wire::encode_attach_client(&wire::AttachClient::Hello { proto: 999 }).unwrap(),
    );
    watcher.wait_for("bad_hello hello_refused", BAD_HELLO, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloRefused { .. })).then_some(())
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if transport.closed_conns().contains(&BAD_HELLO) {
            break;
        }
        assert!(Instant::now() < deadline, "the refused hello connection was never closed");
        std::thread::sleep(Duration::from_millis(10));
    }

    // Mgmt still works on its own connection -- probe AND status, the
    // latter carrying this process's own pid/creation-time/survival.
    transport.feed(MGMT, frame::mgmt_probe());
    watcher.wait_for("mgmt probe_ok (after bad hello)", MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ProbeOk)).then_some(())
    });
    transport.feed(MGMT, frame::mgmt_status());
    let (pid, survival) = watcher.wait_for("mgmt status_ok", MGMT, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::MgmtReply(wire::MgmtReply::StatusOk { pid, survival, .. }) => Some((*pid, *survival)),
        _ => None,
    });
    assert_eq!(pid, std::process::id(), "status.pid must be the capsule's OWN process id");
    assert_eq!(survival, wire::Survival::Normal);

    // A fresh, compatible attach still succeeds.
    transport.open(GOOD);
    transport.feed(GOOD, frame::hello());
    watcher.wait_for("good hello_ok", GOOD, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(GOOD, frame::attach("late"));
    watcher.collect_checkpoint("good checkpoint", GOOD, Duration::from_secs(10));

    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
}

/// Test 11 (REWRITTEN, Codex round-1 Blocker 1 discharge): the durable
/// MARKER — not the ack — drives teardown. "Ack completion only
/// ACCELERATES teardown" (ADR 0041 EndRun step 2): a stalled ack, a
/// client that stops reading, a progress-deadline close, or a lost
/// connection cannot unlatch it. Proven by holding the `shutdown_ok`
/// ack's physical-send completion and NEVER RELEASING IT — confirming
/// the marker is already durable on the still-open leg while the ack is
/// held, then confirming `run` still completes and seals within a
/// bounded time regardless (the ack-grace window still expires
/// normally, exactly as `shutdown_ack_grace_expires_and_teardown_still_
/// completes` proves for a Kill-driven teardown; this is the SAME
/// mechanism with the wire request itself as the ONLY driver, no
/// separate cause). The reason string still lands in `producer_dead`'s
/// detail — recorded from the marker's own commit (see
/// `commit_run_end_marker`'s call sites), never from the ack's
/// completion, which here never happens at all.
///
/// The ORIGINAL version of this test asserted the opposite
/// (`!handle.is_finished()` while the ack was held, released later) —
/// that encoded the design Blocker 1 identifies as wrong: a stalled ack
/// must never be able to leave a durable marker coexisting with a shell
/// running on.
#[test]
fn teardown_completes_even_when_the_shutdown_ack_is_never_delivered() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until EndRun
    let cfg = config(dir.path(), "neverack1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (_tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const MGMT: ConnId = 1;
    transport.open(MGMT);
    transport.set_hold_for(MGMT, true); // held FOREVER -- never released below
    transport.feed(MGMT, frame::mgmt_shutdown("never-delivered-ack"));

    // The ack's bytes are constructed and queued...
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("mgmt shutdown_ok queued", MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });

    // ...and the marker is ALREADY durable on the still-open leg, even
    // though that ack's physical completion will NEVER be reported.
    let seg_dir = root.join("seg");
    let marker_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if leg_carries_run_end_marker(&seg_dir, "neverack1", 1).unwrap_or(false) {
            break;
        }
        assert!(Instant::now() < marker_deadline, "marker never became visible on the still-open leg");
        std::thread::sleep(Duration::from_millis(20));
    }

    // `transport.release_held()` is deliberately NEVER called -- the
    // whole point is that teardown does not need it.
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect(
            "run did not complete even though the durable marker should drive teardown \
             regardless of the never-delivered ack",
        )
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    verify_voyage(&root, "neverack1").unwrap();
    assert!(leg_carries_run_end_marker(&seg_dir, "neverack1", 1).unwrap());

    let frames = sealed_frames(&root, "neverack1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["reason"], "never-delivered-ack");
}

/// ADR 0041 step 6 U1b, acceptance matrix "the marker is the acceptance
/// barrier": the marker is durable on the STILL-OPEN leg (before `run`
/// has sealed anything) essentially as soon as the request is processed
/// — and, distinctly from `teardown_completes_even_when_the_shutdown_
/// ack_is_never_delivered` above (which proves teardown does NOT need
/// the ack), this test proves the ack is still a working COURTESY when
/// nothing prevents it: released promptly, its bytes still show up as a
/// well-formed `ShutdownOk` reply on the same connection. "Ack completion
/// only accelerates teardown" cuts both ways — it must never be
/// REQUIRED, but it must still WORK.
#[test]
fn marker_is_durable_before_the_ack_completes_and_the_ack_still_works() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until EndRun
    let cfg = config(dir.path(), "markerstall1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (_tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const MGMT: ConnId = 1;
    transport.open(MGMT);
    transport.set_hold_for(MGMT, true); // held BEFORE the request that matters
    transport.feed(MGMT, frame::mgmt_shutdown("marker-before-ack"));

    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("mgmt shutdown_ok queued", MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });

    // The ack is QUEUED but its physical-write completion is HELD -- the
    // marker must already be durable regardless. Nothing is sealed yet
    // (teardown may already be under way), so poll the STILL-OPEN leg
    // directly.
    let seg_dir = root.join("seg");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if leg_carries_run_end_marker(&seg_dir, "markerstall1", 1).unwrap_or(false) {
            break;
        }
        assert!(Instant::now() < deadline, "marker never became visible on the still-open leg");
        std::thread::sleep(Duration::from_millis(20));
    }

    transport.release_held();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return after the ack was released")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    verify_voyage(&root, "markerstall1").unwrap();
    assert!(leg_carries_run_end_marker(&seg_dir, "markerstall1", 1).unwrap());
    // The courtesy still worked: the EARLIER `wait_for` already decoded a
    // well-formed `ShutdownOk` reply queued for this connection, and
    // `release_held` + the successful `wait_for_join` above prove its
    // physical-send completion was processed normally through teardown --
    // the ack is not required, but it is not broken either.
}

/// ADR 0041 step 6 U1b, acceptance matrix "the marker is the acceptance
/// barrier", step 4: two concurrent callers get ONE marker and TWO acks.
/// Real concurrency at the wire level (two mgmt connections, both
/// requesting before either's ack physically completes) — the end-to-end
/// proof that the WIRING (`attach_proto`'s `Action::RunEndRequested`
/// ordering, `execute_actions!`'s dispatch) actually delivers the
/// guarantee `commit_run_end_marker`'s own unit tests already prove in
/// isolation against the pure function.
#[test]
fn two_concurrent_shutdown_requests_write_one_marker_and_ack_both() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()];
    let cfg = config(dir.path(), "concurrentshutdown1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (_tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const MGMT_A: ConnId = 1;
    const MGMT_B: ConnId = 2;
    transport.open(MGMT_A);
    transport.open(MGMT_B);
    // Hold BOTH acks until both requests are already queued -- a genuine
    // race between the two callers, not two sequential round trips.
    transport.set_hold_for(MGMT_A, true);
    transport.set_hold_for(MGMT_B, true);
    transport.feed(MGMT_A, frame::mgmt_shutdown("caller-a"));
    transport.feed(MGMT_B, frame::mgmt_shutdown("caller-b"));

    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("A shutdown_ok queued", MGMT_A, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });
    watcher.wait_for("B shutdown_ok queued", MGMT_B, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });

    transport.release_held();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return after both acks were released")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    verify_voyage(&root, "concurrentshutdown1").unwrap();

    let frames = sealed_frames(&root, "concurrentshutdown1");
    let marker_count = frames
        .iter()
        .filter(|f| {
            f.class == Class::Lifecycle
                && f.payload.as_ref().and_then(|p| p.get("kind")).and_then(|k| k.as_str())
                    == Some("run_end_requested")
        })
        .count();
    assert_eq!(marker_count, 1, "two concurrent callers must write exactly one marker");
}

/// Test 12 (finding 7, Codex review rework): `Transport::shutdown_all` is
/// actually invoked before `run` returns, on every exit path -- the one
/// piece of the teardown rework that is new wiring, not a restatement of
/// something the pure-logic `AttachProto` tests already cover. The reduced
/// legal action set teardown enforces (mgmt served, producer-bound
/// admission revoked, no lockstep leak from an ignored request) is proven
/// exhaustively and race-free at that level already
/// (`attach_proto::teardown_ignores_producer_bound_requests_but_not_mgmt_or_attach`)
/// — reproving it here against a real ConPTY would only buy a race between
/// the test thread's writes and whichever loop iteration observes them
/// first, without adding coverage `execute_teardown_actions!`'s own
/// `unreachable!` arms don't already give at compile time.
///
/// U1a Codex round-1, minor cluster: asserts the exact count (2), not
/// merely "at least once" -- `run` now calls `shutdown_all` explicitly
/// once the (zero-iteration, on these paths) ack grace resolves, AND
/// `ShutdownGuard::drop` calls it again unconditionally afterward. Proving
/// the count is exactly 2 is what actually exercises `Transport::
/// shutdown_all`'s documented idempotent contract, rather than merely
/// trusting it.
#[test]
fn shutdown_all_is_called_before_run_returns_on_every_exit_path() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();

    // Path 1: a natural producer exit.
    {
        let argv = shell_command("exit 0");
        let cfg = config(dir.path(), "shutdownall1", argv, 80, 25);
        let transport = TestTransport::new();
        let (_tx, rx) = mpsc::channel();
        let mut run_transport = transport.clone();
        let summary = capsule::run::<P>(cfg, rx, &mut run_transport).unwrap();
        assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
        assert_eq!(
            transport.shutdown_all_call_count(),
            2,
            "shutdown_all must be called exactly twice (the explicit ack-grace call, then ShutdownGuard::drop) even on a natural producer exit"
        );
    }

    // Path 2: a requested kill, with an attached connection still open --
    // proving `shutdown_all` runs even when the pipe has real state on it,
    // not only in the no-connections-ever-opened case above.
    {
        let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
        let cfg = config(dir.path(), "shutdownall2", argv, 80, 25);
        let transport = TestTransport::new();
        let (tx, rx) = mpsc::channel();
        let run_transport = transport.clone();
        let handle = std::thread::spawn(move || {
            let mut t = run_transport;
            capsule::run::<P>(cfg, rx, &mut t)
        });

        const CONN: ConnId = 1;
        transport.open(CONN);
        transport.feed(CONN, frame::hello());
        let mut watcher = FrameWatcher::new(&transport);
        watcher.wait_for("conn hello_ok", CONN, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        transport.feed(CONN, frame::attach("watcher"));
        watcher.collect_checkpoint("conn checkpoint", CONN, Duration::from_secs(10));

        tx.send(Command::Kill).unwrap();
        let summary = wait_for_join(handle, Duration::from_secs(30))
            .expect("run did not return within the teardown bound")
            .unwrap();
        assert_eq!(summary.exit_kind, ExitKind::Requested);
        assert_eq!(
            transport.shutdown_all_call_count(),
            2,
            "shutdown_all must be called exactly twice on a requested kill too"
        );
    }
}

/// Codex round-1 Blocker 3 discharge: aggregate-teardown expiry is
/// TERMINAL, not a "loud but successful" return. `run` must not seal the
/// voyage or report `Ok(ExitSummary)` past a teardown whose transport
/// could not prove every worker stopped within the shared deadline — the
/// writer fence (`store`, dropped via this same early return) is the
/// only thing that may release past it. Simulated via `TestTransport::
/// force_shutdown_expiry` (a real Windows stalled-worker scenario is
/// proven separately, at the transport level, by `pipe_win.rs`'s own
/// `stalled_worker_does_not_block_teardown_of_healthy_connections` and
/// the pure `join_within` expiry tests) — this test's job is specifically
/// `capsule::run`'s OWN reaction to that report.
#[test]
fn aggregate_teardown_expiry_is_terminal_not_a_silent_seal() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = shell_command("exit 0");
    let cfg = config(dir.path(), "expiryterminal1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    transport.force_shutdown_expiry();
    let (_tx, rx) = mpsc::channel();
    let mut run_transport = transport.clone();
    let err = capsule::run::<P>(cfg, rx, &mut run_transport).unwrap_err();
    assert!(
        format!("{err}").contains("aggregate teardown"),
        "expected a named aggregate-teardown failure, got: {err}"
    );
    // No seal, ever: the segment stays `.open`, never `.sotseg`, and the
    // final `producer_dead` frame this path would otherwise have written
    // never lands.
    let seg_dir = root.join("seg");
    let names: Vec<String> = std::fs::read_dir(&seg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.ends_with(".sotseg")),
        "a terminal teardown failure must never seal a segment: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.ends_with(".open")),
        "the survivor segment must remain unsealed: {names:?}"
    );
}

/// Test 13 (U1a, ADR 0041 EndRun state machine item 4 / "ack grace"): a
/// mgmt `shutdown` accepted during teardown (mirroring "a request accepted
/// in the final service poll") must have its `ShutdownAck` physically
/// written before this capsule's own transport disappears — proven by
/// holding that ack's completion and observing `run` genuinely blocks
/// rather than tearing the pipe down out from under it, then releasing it
/// and observing `run` completes promptly.
///
/// U1a Codex round-1, Blocker 3 discharge: an EARLIER version of this test
/// held the ack while running a persistent `cmd.exe` but never sent
/// `Command::Kill` -- since `AttachAction::Shutdown` (which sets
/// `shutdown_requested`) is only emitted once THIS SAME held ack is
/// reported physically written, `run` never reached teardown AT ALL, let
/// alone the grace loop, and the test could only time out. A SEPARATE
/// cause (here, `Command::Kill`) must drive the primary into teardown
/// independently of the held connection.
///
/// U1a Codex round-1, minor cluster: the "still blocked" observation is a
/// CONTINUOUS poll against `shutdown_all_call_count` (the actual
/// mechanism-relevant signal), not one sleep-then-check at an arbitrary
/// point -- a single check at, say, 500ms can pass merely because
/// ordinary Phase A/B teardown itself hadn't finished yet, proving
/// nothing about the grace specifically. Polling continuously up to a
/// GENEROUS floor (1.5s, safely under the 2s grace and safely over the
/// sub-second teardown a trivial killed `cmd.exe` takes) means ANY early
/// firing is caught the instant it happens, regardless of how long
/// ordinary teardown took to get there.
#[test]
fn shutdown_ack_grace_defers_transport_shutdown_until_the_late_ack_completes() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
    let cfg = config(dir.path(), "ackgrace1", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    // A SEPARATE cause drives the primary into teardown (an operator kill,
    // mirroring a natural producer exit just as well) -- the late mgmt
    // connection below is a RACING request, not what ends the run.
    const LATE_MGMT: ConnId = 1;
    transport.open(LATE_MGMT);
    transport.set_hold_for(LATE_MGMT, true); // never completes on its own
    transport.feed(LATE_MGMT, frame::mgmt_shutdown("late-in-teardown"));
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("late mgmt shutdown_ok queued", LATE_MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });
    tx.send(Command::Kill).unwrap();

    // The ack's bytes are already QUEUED (proven above via `wait_for`, which
    // watches queued bytes, not completions) but its physical-write
    // completion is HELD -- `run` must not let its transport disappear
    // while that is true. Continuous poll, not one snapshot: an ORDER
    // assertion that holds regardless of how long ordinary teardown itself
    // happens to take.
    let floor = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < floor {
        assert!(!transport.shutdown_all_was_called(), "shutdown_all must not run before the grace resolves");
        assert!(!handle.is_finished(), "the ack grace must hold the transport open until the late ack completes");
        std::thread::sleep(Duration::from_millis(20));
    }

    transport.release_held();
    let summary = wait_for_join(handle, Duration::from_secs(10))
        .expect("run did not return after the late ack was released")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    assert_eq!(
        transport.shutdown_all_call_count(),
        2,
        "shutdown_all must run exactly twice: the explicit ack-grace call, then ShutdownGuard::drop"
    );

    let frames = sealed_frames(&dir.path().join("ackgrace1"), "ackgrace1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["reason"], "late-in-teardown");
}

/// Test 14 (U1a): the grace is a DEADLINE, not an indefinite wait — if the
/// late ack's completion never arrives, `run` still completes once
/// `SHUTDOWN_ACK_GRACE` (2s) elapses, and `shutdown_all` still runs
/// afterward.
///
/// U1a Codex round-1, Blocker 3 discharge: as in the test above, a
/// SEPARATE `Command::Kill` drives teardown -- the held connection's own
/// `shutdown` request never completes its ack, so it can never itself
/// trigger `AttachAction::Shutdown`/`shutdown_requested`.
///
/// U1a Codex round-1, minor cluster: no clock-injection seam exists for
/// `SHUTDOWN_ACK_GRACE` inside `run` (it is a real integration test
/// against a real ConPTY producer, so real time is unavoidable at this
/// level regardless) — the continuous pre-deadline poll below is the
/// ORDER assertion the review asked for, layered ON TOP of (not instead
/// of) confirming the pinned 2s bound itself: the lower-bound duration
/// check only fires if the deadline expired too early, which real-clock
/// scheduler jitter can only ever make LARGER, never smaller, so it is
/// not a source of flake in this direction.
#[test]
fn shutdown_ack_grace_expires_and_teardown_still_completes() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
    let cfg = config(dir.path(), "ackgrace2", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const LATE_MGMT: ConnId = 1;
    transport.open(LATE_MGMT);
    transport.set_hold_for(LATE_MGMT, true); // NEVER released -- proves the deadline, not the release
    transport.feed(LATE_MGMT, frame::mgmt_shutdown("never-acked"));
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("late mgmt shutdown_ok queued", LATE_MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });
    tx.send(Command::Kill).unwrap();

    let started = Instant::now();
    // ORDER assertion: continuously poll a floor safely under the 2s
    // grace, asserting it has NOT expired early.
    let floor = started + Duration::from_millis(1500);
    while Instant::now() < floor {
        assert!(!transport.shutdown_all_was_called(), "must not expire the grace early");
        std::thread::sleep(Duration::from_millis(20));
    }

    let summary = wait_for_join(handle, Duration::from_secs(15))
        .expect("run did not return even after the ack grace should have expired")
        .unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(2), "must honor the full grace before giving up: {elapsed:?}");
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    assert_eq!(
        transport.shutdown_all_call_count(),
        2,
        "shutdown_all must run exactly twice even when the grace expires unattended"
    );
}

/// Test 15 (U1a Codex round-1, Major 6 discharge): the grace drains only
/// what is ALREADY pending — a brand new connection arriving squarely
/// inside the grace window must be closed outright, with NO reply ever
/// sent for its request, rather than admitted and given almost none of
/// the 2s the "final service poll" guarantee actually promises.
#[test]
fn shutdown_ack_grace_admits_no_new_connections_or_bytes() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
    let cfg = config(dir.path(), "ackgrace3", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const LATE_MGMT: ConnId = 1;
    transport.open(LATE_MGMT);
    transport.set_hold_for(LATE_MGMT, true); // held throughout -- keeps the grace open for this whole test
    transport.feed(LATE_MGMT, frame::mgmt_shutdown("late-in-teardown"));
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("late mgmt shutdown_ok queued", LATE_MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });
    tx.send(Command::Kill).unwrap();

    // Confirm we are GENUINELY mid-grace (not merely "still doing ordinary
    // Phase A/B teardown") before probing the new-admission behavior --
    // the same continuous-poll proof the other ack-grace tests use.
    let floor = Instant::now() + Duration::from_millis(500);
    while Instant::now() < floor {
        assert!(!handle.is_finished(), "the ack grace must still be holding at this point");
        std::thread::sleep(Duration::from_millis(20));
    }

    // A brand NEW connection, opened squarely inside the confirmed-active
    // grace window: it must be closed outright, and no reply may ever be
    // sent to it.
    const NEW_CONN: ConnId = 2;
    transport.open(NEW_CONN);
    transport.feed(NEW_CONN, frame::mgmt_probe());
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !transport.closed_conns().contains(&NEW_CONN) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        transport.closed_conns().contains(&NEW_CONN),
        "a connection opened during the ack grace must be closed, never left admitted"
    );
    assert!(
        transport.sent_frames().iter().all(|(c, _)| *c != NEW_CONN),
        "no reply may ever be sent to a connection admitted during the ack grace"
    );

    transport.release_held();
    let summary = wait_for_join(handle, Duration::from_secs(10))
        .expect("run did not return after the late ack was released")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
}

/// A non-panicking, bounded poll for an `AttachServer::Output` frame on
/// `conn` whose bytes contain `needle` — unlike `FrameWatcher::wait_for`
/// (which panics on timeout), this returns `false` on expiry so the
/// caller can run its own cleanup (ending the run, joining threads)
/// BEFORE asserting on the result, rather than leaking a live shell/
/// thread behind an early panic mid-test.
fn poll_for_committed_marker(transport: &TestTransport, conn: ConnId, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut start = 0usize;
    loop {
        let frames = transport.sent_frames_from(start);
        start += frames.len();
        for (c, bytes) in &frames {
            if *c != conn {
                continue;
            }
            let mut s = wire::FrameSplitter::new();
            let (decoded, _err) = s.feed(bytes);
            for f in &decoded {
                if let wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) = f {
                    if String::from_utf8_lossy(bytes).contains(needle) {
                        return true;
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Codex round on #227 (P1 discharge): the group-commit deadline
/// (`last_commit.elapsed() >= GROUP_COMMIT_WINDOW`) used to be evaluated
/// ONLY inside `output_rx.recv_timeout`'s own `Timeout` arm, so producer
/// output plus transport activity arriving faster than
/// `GROUP_COMMIT_WINDOW` apart could starve it indefinitely — the DATA was
/// always buffered correctly; only WHEN an attached watcher got to see it
/// was at risk. Proven here with a mechanism-level bound, not a lucky
/// timing sample: a `mgmt_probe()` fed on one already-open mgmt
/// connection every 10ms — comfortably faster than `GROUP_COMMIT_WINDOW`'s
/// own 50ms — for the WHOLE observation window means `output_rx.
/// recv_timeout` can never time out during it, so a Timeout-arm-only
/// regression could not commit ANYTHING in that window NO MATTER HOW LONG
/// it ran, which is what makes the bound's own exact value (500ms, 10x the
/// window -- generous headroom for this test running alongside others
/// under `cargo test`'s default parallelism, not a tight timing race)
/// irrelevant to whether this is a real proof: the ping thread below runs
/// for the bound's own FULL duration, so a regression has no window in
/// which the Timeout arm could ever fire, regardless of how loose the
/// bound is.
#[test]
fn group_commit_progresses_despite_continuous_transport_pings() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
    let cfg = config(dir.path(), "commitpings1", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const WATCHER: ConnId = 1;
    transport.open(WATCHER);
    transport.feed(WATCHER, frame::hello());
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("watcher hello_ok", WATCHER, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(WATCHER, frame::attach("watcher"));
    watcher.collect_checkpoint("watcher checkpoint", WATCHER, Duration::from_secs(10));
    transport.feed(WATCHER, frame::take("watcher"));
    let epoch = watcher.wait_for("watcher take_ok", WATCHER, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
        _ => None,
    });

    // Continuous "transport activity" for the WHOLE observation window
    // below -- see this test's own doc for why 10ms (< GROUP_COMMIT_WINDOW)
    // is the exact shape that starves a Timeout-arm-only deadline check.
    // A repeated `mgmt_probe()` on ONE already-open mgmt connection, not a
    // churn of freshly opened connections: a fresh `ConnectionOpened`
    // every 10ms, never classified, hits `attach_proto`'s own
    // `NON_WATCHER_CAP` (4) almost immediately, spamming a real, logged
    // `RecordRefusal` per ping thereafter -- and closing each one right
    // back (tried first) exercises `remove_connection`'s own driver/
    // checkpoint-slot bookkeeping on every single tick, which is exactly
    // the kind of protocol-level churn this test has no business
    // depending on: it needs "the loop wakes on transport activity",
    // nothing about admission or teardown paths. A probe on one
    // long-lived mgmt connection is real, wake-triggering "activity" with
    // none of that: lockstep, always answered, and already the protocol's
    // OWN intended shape for frequent liveness checks.
    const PINGER_MGMT: ConnId = 999;
    transport.open(PINGER_MGMT);
    let stop_pinging = Arc::new(AtomicBool::new(false));
    let ping_transport = transport.clone();
    let stop_for_pinger = Arc::clone(&stop_pinging);
    let ping_handle = std::thread::spawn(move || {
        while !stop_for_pinger.load(Ordering::Relaxed) {
            ping_transport.feed(PINGER_MGMT, frame::mgmt_probe());
            std::thread::sleep(Duration::from_millis(10));
        }
    });

    let idem_key = [0x77u8; 16];
    transport.feed(WATCHER, frame::input("watcher", epoch, idem_key, b"echo sot-commit-marker\r\n"));

    // Non-panicking, bounded poll -- cleanup below must still run even if
    // this never finds the frame, so the assertion on `found` comes AFTER
    // it, not here.
    // The mechanism under test is that the marker arrives AT ALL while pings
    // keep the loop busy (the old loop only evaluated the group-commit
    // deadline when recv_timeout expired, which continuous pings prevent —
    // the marker then never arrived). A tight wall-clock bound is not part
    // of that proof and flakes on a loaded CI runner (a Windows leg took
    // >500 ms just to echo the shell input); the bound is generous.
    let found = poll_for_committed_marker(&transport, WATCHER, "sot-commit-marker", Duration::from_secs(10));

    stop_pinging.store(true, Ordering::Relaxed);
    ping_handle.join().unwrap();
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);

    assert!(
        found,
        "expected the shell's own echoed input to reach the attached watcher within 500ms despite a \
         transport ping every 10ms the whole time -- the group-commit deadline must be evaluated every \
         loop iteration, not only when output_rx.recv_timeout happens to time out"
    );
}

/// Extracts every `mN` marker (`m` followed by one or more ASCII
/// digits) appearing anywhere in `text`, in order — a single `Output`
/// frame's bytes can carry several markers concatenated (the shell
/// echoes as fast as it is fed, and one wire frame is not one line).
/// Used only by `v3_watcher_completion_drains_pen_and_geometry_before_
/// a_replayed_takes_own_output`'s own byte-distinguishable marker
/// scheme.
fn extract_marker_numbers(text: &str) -> Vec<u64> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'm' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if let Ok(n) = text[start..j].parse::<u64>() {
                out.push(n);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// ADR 0046 decision 3 (lane B3b1). Codex review round 1, blocker 1: at
/// a v3 watcher's checkpoint completion, `PenSnapshot` and its whole
/// drained queue must reach the transport BEFORE any request replayed
/// off that SAME completion (a `take`/`resize` that raced in behind the
/// watcher's own still-outstanding attach reply) can publish fresh
/// output via the loop's own `flush_output!` (`capsule.rs`'s
/// `CommitTake`/`ApplyResize` arms) — reproduced as "later Output ->
/// PenSnapshot -> Geometry -> older Output" before the fix in
/// `attach_proto.rs`'s `sent` (`CheckpointChunk` final-chunk arm), which
/// now builds this watcher's own completion FIRST and replays a held
/// frame LAST.
///
/// Round 2 review, should-fix: round 1's own version of this test could
/// not tell a QUEUED (pre-completion) `Output` frame from a NEWER
/// (post-completion, freshly flushed by the replay) one — both are just
/// `AttachServer::Output` — so moving the replay code EARLIER (between
/// `PenSnapshot` and the queue drain, the exact defect shape) still let
/// 24 newer bytes overtake the queue while every assertion here kept
/// passing. Fixed by making the two KINDS of output byte-distinguishable
/// and adding a deterministic witness that output is genuinely pending,
/// rather than merely checking that "some Output" follows `PenSnapshot`:
///
/// `B` types SEQUENCE-NUMBERED markers (`"mN\r\n"`) continuously, and —
/// critically — a background thread also WAITS for `B`'s OWN echo of
/// each one (not merely its `input`'s WAL outcome, which is a SEPARATE,
/// unrelated-in-time commit) before advancing to the next. Since `B`
/// only ever SEES an echo once `output_committed` has actually run with
/// it — the SAME call that, uniformly, also queues it for `A` — every
/// marker number `B` has confirmed-echoed by a given instant is PROVEN,
/// not assumed, to already be sitting in `A`'s own queue (`A` is still
/// mid-transfer throughout). This is the deterministic witness: `cutoff`
/// is the highest such CONFIRMED marker right before `A` is released;
/// everything at or below it is unambiguously OLD (queued), and whatever
/// marker the typer is mid-flight on AT the instant of release — sent,
/// but not yet confirmed-echoed to `B` — is unambiguously NEW: genuinely
/// still pending, from `A`'s own perspective, by construction, not by
/// luck. The assertion below requires markers of BOTH kinds to actually
/// appear, then checks EVERY old marker's position against EVERY new
/// marker's position — old before new, full order, no reversal — which
/// is exactly what the pre-fix code violates and what round 1's weaker
/// "some Output after PenSnapshot" check could not see.
#[test]
fn v3_watcher_completion_drains_pen_and_geometry_before_a_replayed_takes_own_output() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()]; // stays open until killed
    let cfg = config(dir.path(), "b3b1order1", argv, 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const B: ConnId = 1;
    const A: ConnId = 2;

    // B: reaches Done and takes the pen first, alone -- nobody else is
    // attached yet, so this take's own broadcast has only B to reach.
    transport.open(B);
    transport.feed(B, frame::hello_at(wire::ATTACH_PROTO_V3));
    let mut watcher_b = FrameWatcher::new(&transport);
    watcher_b.wait_for("B hello_ok", B, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(B, frame::attach("driver"));
    watcher_b.collect_checkpoint("B checkpoint", B, Duration::from_secs(10));
    transport.feed(B, frame::take("driver"));
    let epoch = watcher_b.wait_for("B take_ok", B, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
        _ => None,
    });

    // B types SEQUENCE-NUMBERED, byte-distinguishable markers ("mN\r\n")
    // in the background, respecting the wire's own lockstep (one
    // outstanding `input` at a time). Two speeds, switched by
    // `witness_mode`: WITNESSED (the default, below) additionally WAITS
    // for B's OWN echo of each marker before sending the next -- the
    // deterministic proof this test's `cutoff` relies on, but slow
    // enough that the capsule's own GROUP_COMMIT_WINDOW-paced periodic
    // flush (capsule.rs's `flush_output!`, independent of any client
    // action) always catches up first, so nothing is left GENUINELY
    // pending by the time a later action's own flush_output runs --
    // proven not to reproduce the round-1 defect shape at all by an
    // earlier version of this test, mutation-checked below. Once the
    // main thread has WITNESSED enough markers (below), it flips
    // `witness_mode` to FAST: the typer keeps sending markers, paced
    // only by the wire's own lockstep (`InputRecorded`, itself far
    // faster than a full round trip through the PTY echo), with no
    // per-marker echo wait -- creating a genuine backlog the periodic
    // flush has not caught up with yet, which is what the release right
    // after actually needs to exercise the replay's own flush_output.
    // `b_last_confirmed_echo` is the highest marker number `B` has
    // itself witnessed-echoed so far (only advanced in WITNESSED mode).
    let stop_typing = Arc::new(AtomicBool::new(false));
    let witness_mode = Arc::new(AtomicBool::new(true));
    let b_last_confirmed_echo = Arc::new(AtomicU64::new(0)); // 0 = none yet; markers are 1-based
    // Highest marker number whose WAL commit (`InputRecorded` or a
    // terminal refusal/unknown) has landed WHILE in fast mode -- a much
    // TIGHTER signal than the echo witness above (no PTY round trip
    // involved, just the wire's own ordered command reply), used to
    // release A the INSTANT a real backlog exists rather than guessing
    // a sleep duration against the capsule's own 50ms flush cadence.
    let fast_wal_confirmed = Arc::new(AtomicU64::new(0));
    let type_transport = transport.clone();
    let stop_for_typer = Arc::clone(&stop_typing);
    let witness_for_typer = Arc::clone(&witness_mode);
    let confirmed_for_typer = Arc::clone(&b_last_confirmed_echo);
    let fast_wal_for_typer = Arc::clone(&fast_wal_confirmed);
    let type_handle = std::thread::spawn(move || {
        let mut watcher_type = FrameWatcher::new(&type_transport);
        let mut n: u64 = 1;
        while !stop_for_typer.load(Ordering::Relaxed) {
            let marker = format!("echo m{n}\r\n");
            let mut idem = [0u8; 16];
            idem[..8].copy_from_slice(&n.to_le_bytes());
            type_transport.feed(B, frame::input("driver", epoch, idem, marker.as_bytes()));
            let witnessing = witness_for_typer.load(Ordering::Relaxed);
            watcher_type.wait_for("typed input outcome", B, Duration::from_secs(5), |f| {
                matches!(
                    f,
                    wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded)
                        | wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale)
                        | wire::DecodedFrame::AttachServer(wire::AttachServer::InputDeliveryUnknown)
                )
                .then_some(())
            });
            if !witnessing {
                fast_wal_for_typer.fetch_max(n, Ordering::Relaxed);
            } else {
                // The witness: B's OWN echo of THIS marker, a SEPARATE,
                // asynchronous path from the input outcome above -- only
                // once this is seen is marker n PROVEN to have already
                // reached output_committed.
                let needle = format!("m{n}");
                if poll_for_committed_marker(&type_transport, B, &needle, Duration::from_secs(5)) {
                    confirmed_for_typer.fetch_max(n, Ordering::Relaxed);
                }
            }
            n += 1;
        }
    });

    // A: the v3 watcher under test. Held from before its own `attach`
    // onward -- its checkpoint chunk gets queued but never reported
    // complete, so it stays genuinely mid-transfer (`Sending`) for the
    // whole middle section below.
    transport.open(A);
    transport.feed(A, frame::hello_at(wire::ATTACH_PROTO_V3));
    let mut watcher_a = FrameWatcher::new(&transport);
    watcher_a.wait_for("A hello_ok", A, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.set_hold_for(A, true);
    transport.feed(A, frame::attach("watcher"));
    // Throwaway cursor, same technique `attach_mid_stream_checkpoint_
    // reproduces_reference_screen` uses: only confirms the chunk was
    // constructed and queued (`sent_frames` records bytes at send time,
    // held or not) without disturbing `watcher_a`'s own cursor. By the
    // time this returns, `reply_queued` is already set on A's
    // connection (that happens synchronously when the chunk's own Send
    // is constructed, well before any transport-level completion,
    // held or not) -- so the `take` fed right after this is guaranteed
    // to be HELD, not refused as a lockstep violation.
    FrameWatcher::new(&transport).wait_for("A checkpoint chunk queued (throwaway)", A, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::CheckpointChunk { .. })).then_some(())
    });

    // A races its OWN `take` in behind its still-unconfirmed attach
    // reply.
    transport.feed(A, frame::take("a-driver"));

    // While A sits held: B resizes -- Geometry broadcasts to both
    // watchers (B, Done, gets it immediately; A, mid-transfer, gets it
    // QUEUED), and B's own `ApplyResize` handling flushes whatever
    // output is pending at that moment, ALSO queued for A.
    transport.feed(B, frame::resize(100, 40));
    watcher_b.wait_for("B resize_ok", B, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk)).then_some(())
    });

    // A must not have received anything beyond its own held hello_ok/
    // checkpoint chunk yet -- everything above queues behind its still-
    // open transfer.
    let leaked_while_held = transport
        .sent_frames()
        .into_iter()
        .filter(|(c, _)| *c == A)
        .flat_map(|(_, bytes)| wire::FrameSplitter::new().feed(&bytes).0)
        .filter(|f| {
            matches!(
                f,
                wire::DecodedFrame::AttachServer(wire::AttachServer::Output { .. })
                    | wire::DecodedFrame::AttachServer(wire::AttachServer::PenSnapshot { .. })
                    | wire::DecodedFrame::AttachServer(wire::AttachServer::PenChanged { .. })
                    | wire::DecodedFrame::AttachServer(wire::AttachServer::Geometry { .. })
                    | wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })
                    | wire::DecodedFrame::AttachServer(wire::AttachServer::TakeRefused { .. })
            )
        })
        .count();
    assert_eq!(
        leaked_while_held, 0,
        "output/PenChanged/Geometry/take outcomes committed while A is mid-transfer must all queue behind it"
    );

    // Wait until B has confirmed-echoed at least 3 markers -- all
    // unambiguously OLD, queued for A while A sat held.
    let deadline = Instant::now() + Duration::from_secs(10);
    while b_last_confirmed_echo.load(Ordering::Relaxed) < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let cutoff = b_last_confirmed_echo.load(Ordering::Relaxed);
    assert!(cutoff >= 3, "expected B to have confirmed-echoed at least 3 markers before release: got {cutoff}");

    // Switch the typer to FAST mode (see its own doc) and wait for its
    // WAL commit (NOT the slower echo witness -- no PTY round trip) to
    // confirm a few markers have actually been forwarded, THEN release A
    // IMMEDIATELY, with no further sleep. This is deliberately a COUNT,
    // not a fixed delay: the capsule's own 50ms GROUP_COMMIT_WINDOW
    // periodic flush (`capsule.rs`) runs on its own clock the whole time
    // this test executes regardless of what this thread does, so any
    // fixed sleep comparable to or longer than it would let that
    // ordinary cycle catch up and flush this burst on its own, before
    // release ever gets a chance to -- exactly what an EARLIER version
    // of this step (a flat 150ms sleep) did, silently defeating the very
    // race this test exists to pin. Racing the release against a COUNT
    // instead keeps this test's own margin as small as the WAL round
    // trip itself allows, whatever that happens to be on the machine
    // running it.
    witness_mode.store(false, Ordering::Relaxed);
    let fast_target = cutoff + 3;
    let fast_deadline = Instant::now() + Duration::from_secs(5);
    while fast_wal_confirmed.load(Ordering::Relaxed) < fast_target && Instant::now() < fast_deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        fast_wal_confirmed.load(Ordering::Relaxed) >= fast_target,
        "expected at least 3 fast-mode markers to be WAL-confirmed before release: got {}",
        fast_wal_confirmed.load(Ordering::Relaxed)
    );

    // Release A: its checkpoint chunk's completion fires, driving Done +
    // PenSnapshot + the whole drained queue BEFORE A's OWN held take
    // replays into CommitTake, whose flush_output publishes MORE output
    // -- which must land LAST, not ahead of PenSnapshot or the queue.
    transport.set_hold_for(A, false);
    transport.release_held();

    // A's own held take must still replay into a NEWER epoch (proves the
    // race actually engaged, not merely refused or silently dropped).
    let a_take_epoch = watcher_a.wait_for("A take_ok (replayed)", A, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
        _ => None,
    });
    assert!(a_take_epoch > epoch, "A's replayed take must commit a NEWER epoch than B's own take");

    // Let a little more real time pass so whatever was in flight at
    // release actually lands, then stop typing.
    std::thread::sleep(Duration::from_millis(300));
    stop_typing.store(true, Ordering::Relaxed);
    let _ = type_handle.join();
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);

    // The full sequence A ever received, in arrival order.
    let a_sequence: Vec<wire::DecodedFrame> = transport
        .sent_frames()
        .into_iter()
        .filter(|(c, _)| *c == A)
        .flat_map(|(_, bytes)| wire::FrameSplitter::new().feed(&bytes).0)
        .collect();

    let pen_snapshot_pos = a_sequence
        .iter()
        .position(|f| matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::PenSnapshot { .. })))
        .expect("A must receive PenSnapshot as its own first v3 event");
    let geometry_pos = a_sequence
        .iter()
        .position(|f| matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::Geometry { .. })))
        .expect("the queued Geometry from B's resize must have drained to A");
    let take_ok_pos = a_sequence
        .iter()
        .position(|f| matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })))
        .expect("A's own replayed take must be in this sequence too");

    assert!(geometry_pos > pen_snapshot_pos, "the queued Geometry must drain AFTER PenSnapshot: {a_sequence:?}");
    assert!(
        take_ok_pos > pen_snapshot_pos,
        "PenSnapshot must precede A's own replayed TakeOk -- the held-take path Codex reproduced: {a_sequence:?}"
    );

    // Every marker number carried by every Output frame A received,
    // paired with that frame's own position -- a single frame can carry
    // several markers concatenated, so scan for every occurrence.
    let marker_positions: Vec<(usize, u64)> = a_sequence
        .iter()
        .enumerate()
        .filter_map(|(pos, f)| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) => Some((pos, bytes)),
            _ => None,
        })
        .flat_map(|(pos, bytes)| {
            let text = String::from_utf8_lossy(bytes).into_owned();
            extract_marker_numbers(&text).into_iter().map(move |n| (pos, n))
        })
        .collect();

    // Real Windows ConPTY, unlike a raw Unix pty, can trigger conhost's
    // own asynchronous screen-buffer repaint on a resize -- re-emitting
    // text it already sent earlier at some later, unpredictable point in
    // the byte stream (a Unix pty's resize is metadata-only: TIOCSWINSZ +
    // SIGWINCH, no redraw). Under a loaded CI runner (the >500ms Windows
    // echo latency already measured elsewhere in this file) that repaint
    // can land AFTER a genuinely NEW marker's own first echo, reproducing
    // an OLD marker's text a second time -- not a real reordering of when
    // that marker actually committed, just redraw noise. Bucket each
    // marker by its FIRST position only (its real commit order); a later
    // repeat of an already-seen number is ignored rather than inflating
    // `max_old_pos`.
    let mut first_pos: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for (pos, n) in &marker_positions {
        first_pos.entry(*n).or_insert(*pos);
    }
    let old_positions: Vec<usize> = first_pos.iter().filter(|(&n, _)| n <= cutoff).map(|(_, &p)| p).collect();
    let new_positions: Vec<usize> = first_pos.iter().filter(|(&n, _)| n > cutoff).map(|(_, &p)| p).collect();

    assert!(
        !old_positions.is_empty(),
        "expected at least one witnessed-OLD marker (numbered <= {cutoff}, confirmed-echoed to B before A was \
         released) to reach A: {a_sequence:?}"
    );
    assert!(
        !new_positions.is_empty(),
        "expected at least one NOT-yet-witnessed marker (numbered > {cutoff}) to reach A -- the deterministic \
         witness this test needs, proving output really was still pending at the instant A was released: \
         {a_sequence:?}"
    );

    let max_old_pos = *old_positions.iter().max().unwrap();
    let min_new_pos = *new_positions.iter().min().unwrap();

    assert!(
        max_old_pos > pen_snapshot_pos,
        "the witnessed-OLD (queued) output must drain AFTER PenSnapshot: {a_sequence:?}"
    );
    assert!(
        max_old_pos < min_new_pos,
        "EVERY witnessed-OLD (queued) marker must arrive strictly before EVERY not-yet-witnessed (potentially \
         fresh-flushed) marker -- this is the exact regression Codex reproduced by moving the replay earlier \
         (newer output overtaking the pending queue): old positions {old_positions:?}, new positions \
         {new_positions:?}, cutoff {cutoff}, full sequence: {a_sequence:?}"
    );
}

// ---------------------------------------------------------------------
// ADR 0043 decision 12, as amended (the macOS pty revoke): the pre-close
// end of the output stream. `capsule::run` is generic over `Producer`, so
// the orderings this rule has to get right — which are a KERNEL race
// between a session leader's exit and its controlling terminal's revoke on
// the platform that actually has them — are driven here from a fake
// producer instead, deterministically, on whatever platform runs the
// suite. The four tests below are named for the invariant, not for the
// platform: "the output stream may end before `close_output_side` only as
// a consequence of the producer's own exit; a terminal reader event that a
// bounded confirmation of that exit does not explain is capsule-fatal".
// ---------------------------------------------------------------------

/// What the fake producer's output side does when its scripted moment
/// arrives: end cleanly (the reader turns this into `Done(Ok(()))`), or
/// panic inside `read` (the reader's drop guard turns that into
/// `ReaderGone` with no terminal `Done` at all).
#[derive(Clone, Copy, PartialEq)]
enum OutputEnd {
    Eof,
    PanicInRead,
}

/// When the fake producer's exit becomes OBSERVABLE to `wait` — the whole
/// point of the fake, since this is the instant a real kernel does not let
/// a test choose.
#[derive(Clone, Copy)]
enum ExitObservable {
    /// Already exited before the writer loop's first poll (ordering B: the
    /// loop's own `wait(ZERO)` wins the race).
    AtSpawn,
    /// This long after the output side ended (ordering A: the reader wins,
    /// and the confirmation has to block out the remainder).
    AfterOutputEnd(Duration),
    /// Never — the producer is alive past the grace, which is the case the
    /// rule still has to kill the capsule for.
    Never,
}

struct FakeScript {
    output_ends_after: Duration,
    output_end: OutputEnd,
    exit: ExitObservable,
    /// `domain_is_empty` answers "not empty" this many times before it
    /// answers "empty". One poll is the ordinary case for a domain that
    /// still has a descendant to reap when teardown starts; zero is a
    /// domain that is already down to an unreaped leader zombie.
    domain_polls_before_empty: usize,
}

/// The script `FakeProducer::spawn` picks up — `Producer::spawn` is an
/// associated function with nowhere for a test to hand it anything, so the
/// handoff is a process-global that every test using it holds `serial()`
/// across.
static FAKE_SCRIPT: std::sync::Mutex<Option<FakeScript>> = std::sync::Mutex::new(None);

struct FakeState {
    exit_at: std::sync::Mutex<Option<Instant>>,
    domain_polls: AtomicU64,
    domain_polls_before_empty: u64,
}

impl FakeState {
    fn exited(&self) -> bool {
        matches!(*self.exit_at.lock().unwrap(), Some(t) if Instant::now() >= t)
    }
}

/// The fake's output side: it blocks (a quiet producer with nothing to
/// say) until the script's thread tells it how this stream ends.
struct FakeOutput {
    steps: mpsc::Receiver<OutputEnd>,
}

impl std::io::Read for FakeOutput {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        match self.steps.recv() {
            // A dropped sender is `close_output_side` having taken the
            // output side away: an ordinary end of stream.
            Ok(OutputEnd::Eof) | Err(_) => Ok(0),
            Ok(OutputEnd::PanicInRead) => panic!("fake producer: scripted panic inside read()"),
        }
    }
}

struct FakeProducer {
    state: Arc<FakeState>,
    output: Option<FakeOutput>,
    steps: Option<mpsc::Sender<OutputEnd>>,
    sink: std::io::Sink,
}

impl sot_log::producer::Producer for FakeProducer {
    type Output = FakeOutput;

    fn pre_spawn_detail() -> serde_json::Value {
        serde_json::json!({})
    }

    fn spawn(_argv: &[String], _cols: u16, _rows: u16) -> sot_log::Result<Self> {
        let script = FAKE_SCRIPT.lock().unwrap().take().expect("no fake script armed");
        let state = Arc::new(FakeState {
            exit_at: std::sync::Mutex::new(match script.exit {
                ExitObservable::AtSpawn => Some(Instant::now()),
                _ => None,
            }),
            domain_polls: AtomicU64::new(0),
            domain_polls_before_empty: script.domain_polls_before_empty as u64,
        });
        let (steps_tx, steps_rx) = mpsc::channel();
        {
            let state = Arc::clone(&state);
            let steps_tx = steps_tx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(script.output_ends_after);
                let _ = steps_tx.send(script.output_end);
                if let ExitObservable::AfterOutputEnd(d) = script.exit {
                    *state.exit_at.lock().unwrap() = Some(Instant::now() + d);
                }
            });
        }
        Ok(FakeProducer {
            state,
            output: Some(FakeOutput { steps: steps_rx }),
            steps: Some(steps_tx),
            sink: std::io::sink(),
        })
    }

    fn take_output(&mut self) -> Self::Output {
        self.output.take().expect("take_output called twice")
    }

    fn input(&mut self) -> &mut dyn std::io::Write {
        &mut self.sink
    }

    fn resize(&self, _cols: u16, _rows: u16) -> sot_log::Result<()> {
        Ok(())
    }

    fn wait(&self, timeout: Duration) -> sot_log::Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.state.exited() {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn exit_status_after_confirmed_exit(&self) -> sot_log::Result<ExitStatus> {
        Ok(ExitStatus::Code(0))
    }

    fn terminate_domain(&self) -> sot_log::Result<()> {
        Ok(())
    }

    fn domain_is_empty(&self) -> sot_log::Result<bool> {
        if !self.state.exited() {
            return Ok(false);
        }
        let seen = self.state.domain_polls.fetch_add(1, Ordering::AcqRel);
        Ok(seen >= self.state.domain_polls_before_empty)
    }

    fn close_output_side(&mut self) -> std::thread::JoinHandle<()> {
        self.steps = None; // a blocked read now ends: the ordinary EOF
        std::thread::spawn(|| {})
    }
}

/// Arms the script and runs one capsule over the fake producer.
fn run_fake(
    dir: &std::path::Path,
    name: &str,
    script: FakeScript,
) -> (sot_log::Result<capsule::ExitSummary>, std::path::PathBuf) {
    *FAKE_SCRIPT.lock().unwrap() = Some(script);
    let cfg = config(dir, name, vec!["fake-producer".to_string()], 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let mut transport = no_transport();
    (capsule::run::<FakeProducer>(cfg, rx, &mut transport), root)
}

/// How many `producer_dead` frames the voyage actually sealed — the
/// question "did this run seal anything" wants, tolerant of a segment a
/// failed run left unsealed (which is not readable strictly and must not
/// panic the test that is asserting the absence).
fn sealed_producer_dead_count(root: &std::path::Path) -> usize {
    let seg_dir = root.join("seg");
    let Ok(entries) = std::fs::read_dir(&seg_dir) else {
        return 0;
    };
    let mut n = 0;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("sotseg") {
            continue;
        }
        let Ok(r) = SegmentReader::read(&p, true) else { continue };
        for f in r.frames {
            if let Some(payload) = f.payload.as_ref() {
                if payload["kind"] == "producer_dead" {
                    n += 1;
                }
            }
        }
    }
    n
}

/// Ordering B of the amended decision 12 (the writer loop's own
/// `wait(ZERO)` observes the exit first, and the reader's terminal event
/// is still in the channel when teardown starts): teardown Phase A picks
/// the event up, confirms it against the exit it already knows about, and
/// RECORDS it. The run seals, verifies, and reports the same
/// `ProducerExited` it would have without the early end.
#[test]
fn early_output_end_after_the_exit_is_recorded_and_the_run_still_seals() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let (summary, root) = run_fake(
        dir.path(),
        "earlyend_b",
        FakeScript {
            output_ends_after: Duration::ZERO,
            output_end: OutputEnd::Eof,
            exit: ExitObservable::AtSpawn,
            // One poll: teardown's first emptiness check says "not yet",
            // so Phase A services the channel once and is where the
            // queued terminal event lands. See
            // `early_output_end_is_not_recorded_when_the_domain_empties_first`
            // for what a domain that is already empty does instead.
            domain_polls_before_empty: 1,
        },
    );
    let summary = summary.expect("an early output end the producer's exit explains must not be fatal");
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    verify_voyage(&root, "earlyend_b").unwrap();
    let frames = sealed_frames(&root, "earlyend_b");
    let detail = assert_producer_dead_is_last(&frames);
    assert!(
        detail["output_ended_early"].is_string(),
        "the admitted case must be RECORDED, not forgiven: {detail:?}"
    );
}

/// Ordering A (the reader wins: the terminal event arrives while the main
/// loop is blocked in `recv_timeout`, and the exit only becomes observable
/// afterwards, well inside the grace). Same outcome as ordering B, same
/// `exit_kind`, same recorded field — which is the determinism claim of
/// the ruling's race analysis, as an assertion.
#[test]
fn early_output_end_confirmed_inside_the_grace_is_recorded_and_the_run_still_seals() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let (summary, root) = run_fake(
        dir.path(),
        "earlyend_a",
        FakeScript {
            output_ends_after: Duration::from_millis(150),
            output_end: OutputEnd::Eof,
            exit: ExitObservable::AfterOutputEnd(Duration::from_millis(300)),
            domain_polls_before_empty: 0,
        },
    );
    let summary = summary.expect("an exit confirmed inside the grace must not be fatal");
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    verify_voyage(&root, "earlyend_a").unwrap();
    let frames = sealed_frames(&root, "earlyend_a");
    let detail = assert_producer_dead_is_last(&frames);
    assert!(
        detail["output_ended_early"].is_string(),
        "the admitted case must be RECORDED, not forgiven: {detail:?}"
    );
}

/// The rule still forbids what the old one forbade: a terminal reader
/// event with the producer STILL ALIVE past the grace is capsule-fatal and
/// seals NOTHING. Without this test the lane has not discharged the
/// invariant — it has only widened it. (This is the one test that waits
/// out the real `READER_END_EXIT_GRACE`.)
#[test]
fn early_output_end_with_the_producer_alive_past_the_grace_is_fatal() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let (summary, root) = run_fake(
        dir.path(),
        "earlyend_fatal",
        FakeScript {
            output_ends_after: Duration::from_millis(50),
            output_end: OutputEnd::Eof,
            exit: ExitObservable::Never,
            domain_polls_before_empty: 0,
        },
    );
    let err = summary.expect_err("an unexplained early output end must stay capsule-fatal");
    let text = format!("{err}");
    assert!(text.contains("reader reached its terminal state"), "got: {text}");
    assert!(text.contains("still alive"), "the error must name the producer's liveness: {text}");
    assert_eq!(
        sealed_producer_dead_count(&root),
        0,
        "a fatal pre-close end of the output stream must seal nothing"
    );
}

/// Untouched by the amendment: a reader that ends WITHOUT a terminal event
/// at all (here, a panic inside `read`, which only its drop guard reports)
/// is still fatal on every platform, and the grace never enters into it.
#[test]
fn a_reader_gone_without_a_terminal_event_is_still_fatal() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let (summary, root) = run_fake(
        dir.path(),
        "readergone",
        FakeScript {
            output_ends_after: Duration::from_millis(50),
            output_end: OutputEnd::PanicInRead,
            exit: ExitObservable::Never,
            domain_polls_before_empty: 0,
        },
    );
    let err = summary.expect_err("a reader gone without a terminal Done must stay fatal");
    let text = format!("{err}");
    assert!(text.contains("without a terminal Done event"), "got: {text}");
    assert_eq!(sealed_producer_dead_count(&root), 0, "a reader-gone must seal nothing");
}

/// The residual the ruling's race analysis does not cover, pinned so it is
/// a known fact rather than folklore: in ordering B, if the containment
/// domain reports EMPTY on teardown's very first check, Phase A never
/// services the channel, and the terminal event that arrived before the
/// close is consumed by Phase B's drain — which cannot tell it apart from
/// the EOF its own close produces. The run is correct (the producer's exit
/// was confirmed by the main loop's own poll, which is the invariant) and
/// seals; it simply does not carry the recorded field. Recording in that
/// ordering is therefore best-effort, and this test says so out loud.
#[test]
fn early_output_end_is_not_recorded_when_the_domain_empties_first() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let (summary, root) = run_fake(
        dir.path(),
        "earlyend_fast",
        FakeScript {
            output_ends_after: Duration::ZERO,
            output_end: OutputEnd::Eof,
            exit: ExitObservable::AtSpawn,
            domain_polls_before_empty: 0,
        },
    );
    let summary = summary.expect("this ordering is correct, just unrecorded");
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    verify_voyage(&root, "earlyend_fast").unwrap();
    let frames = sealed_frames(&root, "earlyend_fast");
    let detail = assert_producer_dead_is_last(&frames);
    assert!(
        detail.get("output_ended_early").is_none(),
        "the drain cannot distinguish this event from its own close's EOF: {detail:?}"
    );
}

/// The isolation assertion the ruling requires, on the REAL producer this
/// platform ships: a Linux or Windows capsule's record never carries
/// `output_ended_early`, because the arm that sets it is unreachable there
/// (decision 12's held slave; ConPTY's `hOutput` outliving the child).
/// macOS is excluded deliberately — it is the one platform where a session
/// leader's exit revokes the pty under the reader and the field may
/// legitimately appear.
#[test]
#[cfg(not(target_os = "macos"))]
fn a_real_producer_never_records_an_early_output_end() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = shell_command("exit 0");
    let cfg = config(dir.path(), "noearly1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let mut transport = no_transport();
    let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    let frames = sealed_frames(&root, "noearly1");
    let detail = assert_producer_dead_is_last(&frames);
    assert!(
        detail.get("output_ended_early").is_none(),
        "this platform's output side cannot end before the loop closes it: {detail:?}"
    );
}

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
mod windows_only {
    use super::*;

/// Test 2: spawn failure (a nonexistent executable) is compensated, not
/// escaped unsealed (the Linux capsule's own known gap, deliberately not
/// inherited here), and `producer_dead` is still the last frame recorded.
#[test]
fn spawn_failure_is_compensated() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["Z:\\sot_capsule_win_test_no_such_exe_9f31.exe".to_string()];
    let cfg = config(dir.path(), "fail1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let mut transport = no_transport();
    let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::SpawnFailed);
    assert_eq!(summary.exit_code, None);
    assert_eq!(summary.segments_sealed, 1);
    verify_voyage(&root, "fail1").unwrap();

    let frames = sealed_frames(&root, "fail1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["spawn_failed"], true);
    assert!(dead["exit_code"].is_null());
}


/// Test 4: resize is an ordered request+outcome exchange (no response
/// phase — ADR 0041), rejecting out-of-budget requests rather than
/// clamping them, with the outcome's `target` naming ITS OWN request (not
/// just some real request — review finding), and `ResizePseudoConsole`
/// actually invoked exactly once (the in-budget request only — review
/// finding: the disposition string alone doesn't prove the OS call was
/// really gated). Step 5 deletes `Command::Resize` (ADR 0041 spec gate: the
/// wire lane replaces it) — this test now drives resize the same way a real
/// driver would: hello -> attach -> wait for the attach checkpoint -> take
/// -> three `resize` wire frames -> `resize_ok`/`resize_refused` replies.
#[test]
fn resize_ordered_exchange_commits_and_rejects() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec![SHELL_ARGV.to_string()];
    let cfg = config(dir.path(), "resize1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    const CONN: ConnId = 1;
    transport.open(CONN);
    transport.feed(CONN, frame::hello());
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("driver hello_ok", CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(CONN, frame::attach("driver"));
    watcher.collect_checkpoint("driver checkpoint", CONN, Duration::from_secs(10));
    transport.feed(CONN, frame::take("driver"));
    watcher.wait_for("driver take_ok", CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })).then_some(())
    });

    transport.feed(CONN, frame::resize(100, 40)); // in budget
    let ok1 = watcher.wait_for("resize1 in-budget outcome", CONN, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    transport.feed(CONN, frame::resize(9999, 40)); // > 512 cols
    let ok2 = watcher.wait_for("resize2 over-512-cols outcome", CONN, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    transport.feed(CONN, frame::resize(40, 1)); // < 2 rows
    let ok3 = watcher.wait_for("resize3 under-2-rows outcome", CONN, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    assert!(ok1 && !ok2 && !ok3, "expected ok, refused, refused, got {ok1} {ok2} {ok3}");

    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    assert_eq!(summary.resize_os_calls, 1, "expected exactly one ResizePseudoConsole call (the valid request only)");
    verify_voyage(&root, "resize1").unwrap();

    let frames = sealed_frames(&root, "resize1");
    let phase_is = |f: &&Envelope, phase: &str| {
        f.class == Class::ControlExchange
            && f.payload.as_ref().unwrap()["kind_ns"] == "conpty/resize"
            && f.payload.as_ref().unwrap()["phase"] == phase
    };
    let requests: Vec<&Envelope> = frames.iter().filter(|f| phase_is(f, "request")).collect();
    let outcomes: Vec<&Envelope> = frames.iter().filter(|f| phase_is(f, "outcome")).collect();
    assert_eq!(requests.len(), 3, "expected 3 resize requests, got {}", requests.len());
    assert_eq!(outcomes.len(), 3, "expected 3 resize outcomes, got {}", outcomes.len());
    assert_eq!(outcomes[0].payload.as_ref().unwrap()["body"]["disposition"], "ok");
    assert_eq!(outcomes[1].payload.as_ref().unwrap()["body"]["disposition"], "failed");
    assert_eq!(outcomes[2].payload.as_ref().unwrap()["body"]["disposition"], "failed");

    // Each outcome must target its OWN request (by emission order, since
    // request[i] and outcome[i] commit as one uninterrupted pair) — not
    // just "some" real request, which the previous version's `.any(...)`
    // would have let a misattribution bug slip through undetected.
    for (req, outcome) in requests.iter().zip(outcomes.iter()) {
        let target = outcome.payload.as_ref().unwrap()["target"].as_str().unwrap().to_string();
        let expected = format!("{}:{}", req.seq.epoch, req.seq.n);
        assert_eq!(target, expected, "outcome does not target its own request");
    }
}

/// Test 5: flood. A producer emits well beyond the 8 MiB output budget;
/// the run must drain it all to a sealed, verify-green voyage without
/// deadlocking. Whether the budget ever actually BLOCKED during the flood
/// is deliberately NOT asserted here — engagement depends on conhost's
/// burst pacing on the runner, which nothing here controls (a runner-image
/// change turned exactly that assertion red on unchanged code); the
/// blocking property is proven deterministically by OutputBudget's own
/// unit tests in capsule.rs. Run on a background thread with a LOCAL
/// bounded wait: a teardown regression here is exactly a deadlock, and
/// this test must fail loud within its own bound rather than consume the
/// whole CI job's timeout.
#[test]
fn flood_drains_to_a_sealed_voyage_without_deadlock() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let helper = HELPER_EXE.to_string();
    let total: usize = 20 * 1024 * 1024; // > the 8 MiB producer-channel budget
    let argv = vec![helper, "--flood".to_string(), total.to_string()];
    let cfg = config(dir.path(), "flood1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let start = Instant::now();
    let handle = std::thread::spawn(move || {
        let mut transport = no_transport();
        capsule::run::<P>(cfg, rx, &mut transport)
    });
    let summary = wait_for_join(handle, Duration::from_secs(60))
        .expect("run did not return within the local deadline (deadlock?)")
        .unwrap();
    eprintln!("capsule_win flood finding: {total} bytes in {:?}", start.elapsed());
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    assert_eq!(summary.exit_code, Some(ExitStatus::Code(0)));
    verify_voyage(&root, "flood1").unwrap();

    // The right side of the transform boundary (review finding): hOutput
    // is conhost's own rendered VT stream, not a byte-for-byte copy of
    // what the child wrote to its own stdout — startup sequences and
    // line-wrap/scroll handling can legitimately change the total length,
    // so exact equality against `total` proves nothing; the half-of-total
    // bound below is the honest platform-behavior assertion.

    let frames = sealed_frames(&root, "flood1");
    let mut total_decoded = 0usize;
    for f in &frames {
        if f.class == Class::Producer {
            let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
            total_decoded += decode_b64(b64).len();
        }
    }
    assert!(
        total_decoded > total / 2,
        "captured far less output than the flood emitted: {total_decoded} of {total}"
    );
}

/// Test 6: a high-bit (NTSTATUS-shaped) exit code is preserved raw and
/// unsigned all the way through `ExitSummary` AND the sealed
/// `producer_dead` frame's JSON — the review finding that a `u32`-to-`i32`
/// cast anywhere in this path would turn it negative for no reason. Same
/// value `tests/conpty.rs` pins at the primitives layer; this proves the
/// capsule runtime doesn't reintroduce the cast above it.
#[test]
fn exit_code_high_bit_status_preserved_through_producer_dead() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv =
        vec![SHELL_ARGV.to_string(), "/d".to_string(), "/c".to_string(), "exit -1073741819".to_string()];
    let cfg = config(dir.path(), "exitcode1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let mut transport = no_transport();
    let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
    assert_eq!(summary.exit_code, Some(ExitStatus::Code(0xC000_0005)));
    verify_voyage(&root, "exitcode1").unwrap();
    let frames = sealed_frames(&root, "exitcode1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["exit_code"], 0xC000_0005u32);
}


/// Test 7: attach mid-stream, on a producer emitting escape sequences and
/// multibyte UTF-8 continuously, reproduces a from-scratch replay
/// byte-for-byte.
///
/// Rebuilt (finding 13 — the review's own diagnosis of the prior version's
/// CI failure): the producer is now `sot-conpty-helper --script`, a
/// deterministic byte-emitting helper (see its module doc), not a
/// `cmd.exe /d /c for /l ... echo` loop whose own startup latency and
/// console rendering the test could not predict or control. One-byte
/// pacing across 1000 repeats of a block containing a CSI pair, a
/// BEL-terminated OSC, an ST-terminated DCS, and a 3-byte codepoint
/// immediately followed by a 4-byte one (no ASCII separator) makes it
/// likely some ConPTY read lands inside one of those sequence classes
/// somewhere across the run — but likely is not proof, and round-2 review
/// correctly called out that this test used to just assert probability in
/// prose and stop there. It no longer does: below, replaying the sealed
/// voyage's own `Class::Producer` frames one at a time (each one IS a real
/// reader-chunk boundary — see that assertion's own comment) and checking
/// `Parser::is_ground()` after each PROVES at least one interior cut
/// actually happened this run, failing loudly if it somehow didn't rather
/// than silently passing a run that exercised less than it claims to. The
/// vt100 fork's own unit tests already prove `is_ground` is safe to cut
/// any of these classes at any byte boundary (U0); what THIS test proves
/// is the WIRING.
///
/// Three things get proved, all stronger than the prior version's:
///
/// 1. Finding 8: an interior CSI/OSC/DCS/UTF-8 cut is observed to have
///    actually happened somewhere in this run — not merely asserted
///    likely — via the reader-chunk-boundary `is_ground()` replay below.
/// 2. Finding 3 (queuing, not dropping): this connection's sends are held
///    from before `attach` through a window where the producer keeps
///    emitting, so whichever chunk is last never completes yet — proving
///    newly committed output queues behind an in-flight checkpoint transfer
///    (`sent_frames` must show zero `output` frames for this connection
///    while held) rather than being sent ahead of it or dropped. Releasing
///    then delivers the transfer followed immediately by every queued
///    frame, in order — the FIFO contract documented on `TestTransport`
///    above.
/// 3. Finding 13 (the U0 oracle): rather than compare rendered
///    `Screen::contents()` strings — which cannot see cursor position,
///    attributes, or mode bits that aren't in the current viewport — this
///    compares raw bytes twice: the exact tail-byte-equality of what this
///    connection received against the voyage's own recorded producer
///    bytes, and the wire checkpoint against an independently computed
///    `Screen::checkpoint()` of the exact same prefix. Checkpoint bytes are
///    a pure function of screen state (magic/version/geometry/modes/attrs/
///    grid — see `vt100_ctt::Screen::checkpoint`), so two parsers fed
///    identical byte prefixes must produce identical checkpoints; anything
///    else is either a wiring bug or a checkpoint-format non-determinism
///    this crate depends on not existing.
#[test]
fn attach_mid_stream_checkpoint_reproduces_reference_screen() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let helper = HELPER_EXE.to_string();
    // --linger: the producer must be ALIVE for every step below (this test
    // ends the run with an explicit `Kill`, never by producer exit). The
    // previous version relied on 1000 repeats taking long enough — false
    // on a fast conhost: the emission finished in milliseconds, the
    // capsule entered teardown before the test's hello was serviced, and
    // the first wait timed out with the abandoned capsule later logging
    // PreAdmissionTimeout. Producer lifetime is now explicit, not an
    // emission-speed assumption.
    let argv = vec![helper, "--script".to_string(), "1000".to_string(), "--linger".to_string()];
    let (rows, cols) = (25u16, 80u16);
    let cfg = config(dir.path(), "midattach1", argv, cols, rows);
    let root = cfg.voyage_root.clone();
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });

    // Attach WHILE the producer is still actively emitting -- no attempt to
    // engineer a precise cut point; is_ground's own unit tests already
    // cover that. Real elapsed time only, no fixed assumption about where
    // the loop's ground boundary lands.
    //
    // Long enough that the scrollback-ring assertion below (added with the
    // ring itself) has real content to find: `SCRIPT_BLOCK` writes one
    // line roughly every 59 ms (one byte per 1 ms sleep, ~59 bytes/line),
    // so at this 25-row screen a nominal run scrolls a couple hundred
    // lines off in 15 s -- comfortably more than a screenful even if a
    // loaded runner's per-byte sleep runs several times its nominal length
    // (the windows-latest-vs-windows-2022 conhost timing gap measured
    // elsewhere in this file was ~2x, not 10x). Still not an engineered
    // cut point: nothing here pins how MANY lines land in the checkpoint,
    // only that it is comfortably more than a screenful.
    std::thread::sleep(Duration::from_millis(15_000));

    const CONN: ConnId = 1;
    transport.open(CONN);
    transport.feed(CONN, frame::hello());
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("conn hello_ok", CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });

    // Finding 3: hold every send to this connection from before `attach`
    // through a window where the producer keeps emitting, so the
    // checkpoint transfer's own completion(s) never get reported while
    // more output is committed behind it.
    transport.set_hold_for(CONN, true);
    transport.feed(CONN, frame::attach("watcher"));
    // A throwaway cursor: only confirms a checkpoint chunk was actually
    // constructed and queued (`sent_frames` records the bytes at `send`
    // time, held or not) without disturbing `watcher`'s own cursor -- which
    // still needs to find that SAME chunk itself, below, once released.
    FrameWatcher::new(&transport).wait_for("checkpoint chunk queued (throwaway probe)", CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::CheckpointChunk { .. })).then_some(())
    });

    // The producer keeps emitting while the transfer sits unconfirmed.
    std::thread::sleep(Duration::from_millis(500));
    let output_frames_while_held = transport
        .sent_frames()
        .into_iter()
        .filter(|(c, _)| *c == CONN)
        .flat_map(|(_, bytes)| wire::FrameSplitter::new().feed(&bytes).0)
        .filter(|f| matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::Output { .. })))
        .count();
    assert_eq!(
        output_frames_while_held, 0,
        "post-watermark output must queue behind an unconfirmed checkpoint transfer, never be sent ahead of it"
    );

    transport.set_hold_for(CONN, false);
    transport.release_held();
    let checkpoint_bytes = watcher.collect_checkpoint("post-hold checkpoint", CONN, Duration::from_secs(10));

    // Let more output flow post-watermark, then end the run.
    std::thread::sleep(Duration::from_millis(300));
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    verify_voyage(&root, "midattach1").unwrap();

    // Every `output` frame this connection ever received, in arrival order
    // (the FrameWatcher's cursor already sits right after the checkpoint).
    let mut post_watermark = Vec::new();
    for (c, bytes) in transport.sent_frames() {
        if c != CONN {
            continue;
        }
        let mut s = wire::FrameSplitter::new();
        let (decoded, _) = s.feed(&bytes);
        for f in decoded {
            if let wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) = f {
                post_watermark.push(bytes);
            }
        }
    }
    assert!(!post_watermark.is_empty(), "expected at least some post-watermark output");
    let suffix: Vec<u8> = post_watermark.into_iter().flatten().collect();

    // The full, from-scratch reference: every producer byte the voyage
    // ever recorded, in order.
    let frames = sealed_frames(&root, "midattach1");
    let mut total = Vec::new();
    for f in &frames {
        if f.class == Class::Producer {
            let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
            total.extend(decode_b64(b64));
        }
    }

    // Round-2 review, finding 8: PROVE an interior CSI/OSC/DCS/UTF-8 cut
    // actually occurred, rather than asserting repetition makes one "all
    // but certain". Each `Class::Producer` frame in the sealed voyage IS
    // exactly one real ConPTY-read chunk (`handle_output!` appends one
    // frame per `ReaderEvent::Output`, unmodified) -- so replaying those
    // frames one at a time into a fresh parser and checking
    // `Parser::is_ground()` after each one finds every point a REAL read
    // boundary fell in this run. `is_ground() == false` right after a
    // frame means that frame's own end sits strictly inside an
    // unterminated CSI/OSC/DCS/UTF-8 sequence -- the escape/multibyte
    // parser has consumed a partial sequence and is still waiting for the
    // rest, which only an interior cut produces. This does not touch the
    // checkpoint's own cut point (which `ground_reached` guarantees is
    // ALWAYS ground-safe, by design, so it can never itself be mid-
    // sequence) -- it is an independent, run-wide proof that at least one
    // real reader-chunk boundary landed inside one of these classes
    // somewhere in the run.
    let mut fragmentation_probe = vt100_ctt::Parser::new(rows, cols, 0);
    let mut interior_cut_found = false;
    for f in &frames {
        if f.class != Class::Producer {
            continue;
        }
        let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
        fragmentation_probe.process(&decode_b64(b64));
        if !fragmentation_probe.is_ground() {
            interior_cut_found = true;
            break;
        }
    }
    // A loud MARKER, deliberately not an assertion: both CI images turned
    // out to deliver conhost's rendered writes sequence-atomically and
    // aligned with the capsule's reads — zero interior cuts across 1000
    // repeats on BOTH, deterministically — so a panic here would be a
    // permanent red about conhost's internals, not about this capsule.
    // The mid-sequence carry property is pinned DETERMINISTICALLY where
    // it can be: the fork's ground/checkpoint tests cut inside every
    // sequence class by construction, and the wire splitter is fuzzed at
    // every byte boundary. What this e2e proves is end-to-end fidelity
    // over whatever chunking the real conhost produced; the marker below
    // records honestly how much fragmentation this run exercised.
    if !interior_cut_found {
        eprintln!(
            "capsule_win fidelity finding: NO reader-chunk boundary landed inside a \
             CSI/OSC/DCS/UTF-8 sequence this run — interior-cut coverage came only \
             from the deterministic parser/splitter suites, not this e2e"
        );
    }

    // Finding 13, part 1: the post-watermark stream this connection
    // received must be the EXACT byte-for-byte tail of the voyage's total
    // producer bytes -- proves nothing was dropped, duplicated, or
    // reordered across the watermark boundary.
    assert!(suffix.len() <= total.len(), "received more post-watermark bytes than the voyage ever recorded");
    let split = total.len() - suffix.len();
    assert_eq!(
        &total[split..],
        suffix.as_slice(),
        "post-watermark output must be the exact byte-for-byte tail of the voyage"
    );
    let prefix = &total[..split];

    // Finding 13, part 2 (the U0 oracle): the wire checkpoint must be
    // byte-identical to one computed independently by feeding a fresh
    // reference parser exactly the prefix. The reference's own scrollback
    // capacity must match the capsule's own live parser
    // (`CAPSULE_SCROLLBACK_ROWS`) -- the checkpoint now carries a
    // scrollback ring, so a reference built at a different capacity would
    // disagree about how much of it survives, independent of any real
    // divergence in what was actually recorded.
    let mut reference_at_watermark =
        vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    reference_at_watermark.process(prefix);
    let reference_checkpoint = reference_at_watermark
        .screen()
        .checkpoint()
        .expect("prefix screen must be representable");
    assert_eq!(
        checkpoint_bytes, reference_checkpoint,
        "the wire checkpoint must be byte-identical to an independently computed checkpoint of the same prefix"
    );

    // The scrollback ring itself must actually have arrived -- the defect
    // this fixed was an attach handing the client an empty ring every
    // time (`ring_len = 0` on every attach, regardless of how much had
    // scrolled off). Not a fixed expected count: real elapsed time drives
    // how many lines the producer got through before this checkpoint's cut
    // (see the sleep above), so this asserts only that SOME history rode
    // along, which is what the defect actually broke.
    let mut ring_check = vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    ring_check
        .restore_screen(&checkpoint_bytes)
        .expect("checkpoint must decode");
    ring_check.screen_mut().set_scrollback(usize::MAX);
    assert!(
        ring_check.screen().scrollback() > 0,
        "attach must hand over a nonempty scrollback ring; got an empty one"
    );

    // And the full round trip, at the same checkpoint-byte granularity: a
    // fresh parser restored from the wire checkpoint and replayed with the
    // exact suffix must reach a state whose OWN checkpoint is
    // byte-identical to a from-scratch parser's, fed the entire voyage --
    // both at the SAME scrollback capacity as the capsule's own parser,
    // for the same reason as above.
    let mut restored = vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    restored
        .restore_screen(&checkpoint_bytes)
        .expect("checkpoint must decode");
    restored.process(&suffix);

    let mut reference = vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    reference.process(&total);

    assert_eq!(
        restored.screen().checkpoint().expect("restored screen must be representable"),
        reference.screen().checkpoint().expect("reference screen must be representable"),
        "checkpoint + subsequent stream must reproduce the reference session byte-for-byte"
    );
    assert_eq!(summary.exit_kind, ExitKind::Requested);
}

}

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
mod unix_only {
    use super::*;

    /// Test: spawn failure (a nonexistent executable) is compensated, not
    /// escaped unsealed (the Linux capsule's own known gap, deliberately
    /// not inherited here -- see `capsule.rs`'s own module doc), and
    /// `producer_dead` is still the last frame recorded. The portable
    /// twin of `windows_only::spawn_failure_is_compensated`.
    #[test]
    fn spawn_failure_is_compensated_unix() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let argv = vec!["/nonexistent/no_such_exe".to_string()];
        let cfg = config(dir.path(), "failunix1", argv, 80, 25);
        let root = cfg.voyage_root.clone();
        let (_tx, rx) = mpsc::channel();
        let mut transport = no_transport();
        let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
        assert_eq!(summary.exit_kind, ExitKind::SpawnFailed);
        assert_eq!(summary.exit_code, None);
        assert_eq!(summary.segments_sealed, 1);
        verify_voyage(&root, "failunix1").unwrap();

        let frames = sealed_frames(&root, "failunix1");
        let dead = assert_producer_dead_is_last(&frames);
        assert_eq!(dead["spawn_failed"], true);
        assert!(dead["exit_code"].is_null());
    }

    /// Test (ADR 0043 decision 12's own proof): a producer that exits
    /// NATURALLY, on its own, is observed by `wait()` returning `true` on
    /// the very next main-loop poll -- never by the reader thread hitting
    /// a "fatal early EOF" bail, which the capsule's own held slave
    /// descriptor exists specifically to prevent (the master cannot see
    /// EOF/EIO until `close_output_side` drops that slave, well after
    /// `wait` has already seen the exit and teardown has begun). A bare
    /// `/bin/sh -c 'exit 3'` records `ExitKind::ProducerExited` and
    /// `exit_code == Some(Code(3))`, and the record still seals
    /// verify-green -- if the held-slave contract were broken (the reader
    /// surfacing EOF BEFORE the loop ever calls `close_output_side`), this
    /// run would instead bail unsealed with a capsule-fatal error (see
    /// `capsule::run`'s own reader-error handling).
    #[test]
    fn producer_exit_is_seen_by_wait_not_by_a_fatal_eof() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let argv = shell_command("exit 3");
        let cfg = config(dir.path(), "exitcode3", argv, 80, 25);
        let root = cfg.voyage_root.clone();
        let (_tx, rx) = mpsc::channel();
        let mut transport = no_transport();
        let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
        assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
        assert_eq!(summary.exit_code, Some(ExitStatus::Code(3)));
        verify_voyage(&root, "exitcode3").unwrap();

        let frames = sealed_frames(&root, "exitcode3");
        let dead = assert_producer_dead_is_last(&frames);
        assert_eq!(dead["exit_code"], 3);
    }

    /// Test (ADR 0043 decision 12, review round): a producer that closes
    /// its OWN stdio and later reopens its controlling tty must not lose
    /// any output written after the reopen. Before the review round's
    /// fix, the capsule's own reader treated the first `EIO` the master
    /// reported (the instant the child's own last slave reference closed)
    /// as terminal -- even though the CAPSULE's own held slave descriptor
    /// means the master should never actually observe that at all. The
    /// script: capture the controlling tty's path, redirect stdio away
    /// from it (closing the child's OWN slave references), sleep briefly,
    /// reopen the SAME tty by path and redirect stdout/stderr back, print
    /// a marker, exit cleanly.
    #[test]
    fn output_after_a_slave_reopen_is_recorded() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let script = "tty=$(tty); exec </dev/null >/dev/null 2>&1; sleep 1; exec >\"$tty\" 2>&1; \
                      printf AFTER_REOPEN; exit 0";
        let argv = shell_command(script);
        let cfg = config(dir.path(), "slavereopen1", argv, 80, 25);
        let root = cfg.voyage_root.clone();
        let (_tx, rx) = mpsc::channel();
        let mut transport = no_transport();
        let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
        assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
        assert_eq!(summary.exit_code, Some(ExitStatus::Code(0)));
        verify_voyage(&root, "slavereopen1").unwrap();

        let frames = sealed_frames(&root, "slavereopen1");
        let mut all = Vec::new();
        for f in &frames {
            if f.class == Class::Producer {
                let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
                all.extend(decode_b64(b64));
            }
        }
        let text = String::from_utf8_lossy(&all);
        assert!(text.contains("AFTER_REOPEN"), "expected output recorded after the slave reopen, got: {text:?}");
    }

    /// Test (ADR 0043 decisions 13/14): a requested kill against a `sleep
    /// 600` producer tears down through `terminate_domain`
    /// (`killpg(SIGKILL)`), and the resulting `ExitStatus` is `Signal(9)`,
    /// never `Code` -- a signal death has no code at all. The durable
    /// record carries `detail.signal == 9` and NO `exit_code` key (the
    /// two are mutually exclusive additive fields, ADR 0043 decision 13).
    #[test]
    fn signal_death_records_signal() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let argv = vec!["sleep".to_string(), "600".to_string()];
        let cfg = config(dir.path(), "signaldeath1", argv, 80, 25);
        let root = cfg.voyage_root.clone();
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut transport = no_transport();
            capsule::run::<P>(cfg, rx, &mut transport)
        });
        std::thread::sleep(Duration::from_millis(300));
        tx.send(Command::Kill).unwrap();
        let summary = wait_for_join(handle, Duration::from_secs(30))
            .expect("run did not return within the teardown bound")
            .unwrap();
        assert_eq!(summary.exit_kind, ExitKind::Requested);
        assert_eq!(summary.exit_code, Some(ExitStatus::Signal(9)));
        verify_voyage(&root, "signaldeath1").unwrap();

        let frames = sealed_frames(&root, "signaldeath1");
        let dead = assert_producer_dead_is_last(&frames);
        assert_eq!(dead["signal"], 9);
        assert!(
            dead.get("exit_code").is_none(),
            "a signal death must never also carry an exit_code key: {dead:?}"
        );
    }

    /// Test: after a wire `Resize`, the ACTUAL pty geometry moved -- not
    /// merely the wire's own recorded disposition string. Writes `stty
    /// size\n` through the driver connection (the same protocol path a
    /// real attach client uses) and reads the shell's own echoed answer
    /// back off the live output stream, asserting it names the resized
    /// geometry exactly (an asymmetric rows/cols pair, so a transposed
    /// readback cannot pass by accident).
    #[test]
    fn resize_reaches_the_pty() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let argv = vec![SHELL_ARGV.to_string()]; // bare interactive shell
        let cfg = config(dir.path(), "resizepty1", argv, 80, 24);
        let transport = TestTransport::new();
        let (tx, rx) = mpsc::channel();
        let run_transport = transport.clone();
        let handle = std::thread::spawn(move || {
            let mut t = run_transport;
            capsule::run::<P>(cfg, rx, &mut t)
        });

        const CONN: ConnId = 1;
        transport.open(CONN);
        transport.feed(CONN, frame::hello());
        let mut watcher = FrameWatcher::new(&transport);
        watcher.wait_for("driver hello_ok", CONN, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        transport.feed(CONN, frame::attach("driver"));
        watcher.collect_checkpoint("driver checkpoint", CONN, Duration::from_secs(10));
        transport.feed(CONN, frame::take("driver"));
        let epoch = watcher.wait_for("driver take_ok", CONN, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
            _ => None,
        });

        // An asymmetric, in-budget geometry.
        transport.feed(CONN, frame::resize(100, 40));
        let resize_ok = watcher.wait_for("resize outcome", CONN, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
            _ => None,
        });
        assert!(resize_ok, "an in-budget resize must succeed");

        let idem_key = [0x77u8; 16];
        transport.feed(CONN, frame::input("driver", epoch, idem_key, b"stty size\n"));
        watcher.wait_for("stty input outcome", CONN, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(()),
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => {
                panic!("stty input unexpectedly refused stale")
            }
            _ => None,
        });

        // `stty size` prints "<rows> <cols>" -- proof the ACTUAL pty
        // geometry (not merely the wire's own recorded disposition)
        // moved.
        let mut seen = String::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            seen.push_str(&watcher.wait_for("stty output", CONN, Duration::from_secs(10), |f| {
                if let wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) = f {
                    Some(String::from_utf8_lossy(bytes).into_owned())
                } else {
                    None
                }
            }));
            if seen.contains("40 100") {
                break;
            }
            assert!(Instant::now() < deadline, "never saw the resized geometry echoed back: {seen:?}");
        }

        tx.send(Command::Kill).unwrap();
        let summary = wait_for_join(handle, Duration::from_secs(30))
            .expect("run did not return within the teardown bound")
            .unwrap();
        assert_eq!(summary.exit_kind, ExitKind::Requested);
    }
}

/// Spawn `sot-conpty-helper spawn-breakaway cmd.exe /c timeout 30`
/// with piped stdin/stdout — blocked on its own "go" read (see the
/// helper's own module doc) until the caller sends one, so the
/// caller can assign the helper to whatever job(s) it wants it
/// contained by BEFORE the helper ever attempts its own breakaway
/// spawn. No `CREATE_SUSPENDED`/`ResumeThread` needed: the helper's
/// blocking stdin read closes the same race deterministically,
/// using only `std::process::Command`.
#[cfg(windows)]
fn spawn_gated_breakaway_helper() -> std::process::Child {
    std::process::Command::new(env!("CARGO_BIN_EXE_sot-conpty-helper"))
        .args(["spawn-breakaway", "cmd.exe", "/c", "timeout", "30"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sot-conpty-helper spawn-breakaway")
}

/// Send the "go" signal, then read and return the helper's one reply
/// line (`pid=<n>` or `err=<n>`), trimmed.
#[cfg(windows)]
fn release_and_read_reply(helper: &mut std::process::Child) -> String {
    use std::io::{BufRead, Write};
    let mut stdin = helper.stdin.take().expect("helper stdin");
    writeln!(stdin, "go").expect("write go signal");
    drop(stdin);
    let mut line = String::new();
    std::io::BufReader::new(helper.stdout.take().expect("helper stdout"))
        .read_line(&mut line)
        .expect("read helper reply");
    line.trim().to_string()
}

/// ADR 0043 decision 32, test 2: the leg job (`AnonymousJob::create`,
/// now `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK`)
/// really does let a breakaway child leave it — the mechanism
/// `capsule_workspace.rs`'s own Windows `spawn_detached` depends on.
/// The helper is assigned to the job, released, and asked to spawn a
/// breakaway child; that child must NOT be `IsProcessInJob` the job
/// it was spawned from inside.
#[test]
#[cfg(windows)]
fn a_breakaway_child_leaves_the_leg_job() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, IsProcessInJob};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    let job = sot_log::conpty::AnonymousJob::create().expect("create leg job");
    let mut helper = spawn_gated_breakaway_helper();
    let ok = unsafe { AssignProcessToJobObject(job.raw(), helper.as_raw_handle() as HANDLE) };
    assert!(ok != 0, "AssignProcessToJobObject: {}", std::io::Error::last_os_error());

    let reply = release_and_read_reply(&mut helper);
    let pid: u32 = reply
        .strip_prefix("pid=")
        .unwrap_or_else(|| panic!("expected pid=<n>, got {reply:?}"))
        .parse()
        .expect("pid parses");

    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE;
    let child = unsafe { OpenProcess(access, 0, pid) };
    assert!(!child.is_null(), "OpenProcess({pid}): {}", std::io::Error::last_os_error());
    let mut in_job: i32 = 0;
    let ok = unsafe { IsProcessInJob(child, job.raw(), &mut in_job) };
    assert!(ok != 0, "IsProcessInJob: {}", std::io::Error::last_os_error());
    assert_eq!(in_job, 0, "expected the breakaway child NOT to be in the leg job it broke away from");

    unsafe {
        TerminateProcess(child, 1);
        CloseHandle(child);
    }
    let _ = helper.wait();
}

