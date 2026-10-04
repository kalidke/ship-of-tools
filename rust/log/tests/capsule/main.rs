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

#[path = "../support/transports.rs"]
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

mod attach;
mod shutdown;
mod commit;
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
