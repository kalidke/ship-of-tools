#![cfg(all(unix, target_os = "linux"))]
//! Real-helper end-to-end conformance (the P2 wiring, ADR 0040 §Gates):
//! the REAL claude-sdk-helper, the REAL pinned SDK, and the REAL vendored
//! CLI binary, driven fully offline against a local fake Messages API
//! (tests/fixtures/fake_messages_api.mjs) — the only fake sits at the
//! network boundary, the documented public HTTP contract.
//!
//! This file carries the ADR 0040 RULE-5 GATE: the no-replay resume
//! fixture. The adapter may not ship against a pinned SDK version until
//! `e2e_two_turns_resume_no_replay` passes against it.
//!
//! Gated on SOT_HELPER_E2E=1 (needs node + the built helper); a plain
//! `cargo test` skips, keeping the default suite hermetic.

use sot_log::claude::{run, ClaudeConfig, Fence, OperatorCmd};
use sot_log::segment::{RetentionClass, SegmentReader};
use sot_log::verify::verify_voyage;
use sot_log::{Class, RefKind};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn gate() -> bool {
    if std::env::var("SOT_HELPER_E2E").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: set SOT_HELPER_E2E=1 (needs node + the built helper)");
    false
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    // Real SDK turns spawn the vendored CLI (seconds each); serialized so
    // scheduler contention can't starve the in-flight windows.
    static E2E_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    E2E_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn helper_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../adapters/claude-sdk-helper")
}

fn helper_main_js() -> PathBuf {
    let p = helper_dir().join("dist/src/main.js");
    assert!(
        p.exists(),
        "helper not built: run `npm ci && npm run build` in adapters/claude-sdk-helper"
    );
    p.canonicalize().unwrap()
}

/// The pinned SDK version, read from the helper's own package.json — the
/// same source of truth the helper's hello attestation uses. Never
/// hardcode the pin twice.
fn pinned_sdk_version() -> String {
    let pkg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(helper_dir().join("package.json")).unwrap())
            .unwrap();
    pkg["dependencies"]["@anthropic-ai/claude-agent-sdk"]
        .as_str()
        .expect("pinned SDK dependency entry")
        .to_string()
}

/// Per-test scratch. SOT_E2E_KEEP=1 leaks the tempdir (and prints it) so a
/// failing run's voyage — the diagnostics — survives the panic.
fn scratch() -> (PathBuf, Option<tempfile::TempDir>) {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_path_buf();
    if std::env::var("SOT_E2E_KEEP").is_ok() {
        eprintln!("SOT_E2E_KEEP: scratch retained at {}", base.display());
        std::mem::forget(tmp);
        (base, None)
    } else {
        (base, Some(tmp))
    }
}

/// The fake Messages API, one per test. Killed on drop.
struct FakeApi {
    child: Child,
    port: u16,
}

impl FakeApi {
    fn start() -> FakeApi {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_messages_api.mjs");
        let mut child = Command::new("node")
            .arg(&fixture)
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("node on PATH");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .expect("fake api prints its port");
        let port: u16 = line.trim().parse().expect("fake api port line");
        FakeApi { child, port }
    }
}

impl Drop for FakeApi {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The offline environment for the CLI, as KEY=VALUE pairs: config/HOME
/// isolated per test, all traffic to the fake API. The api key is a
/// placeholder — nothing here ever reaches a real endpoint.
fn offline_env(port: u16, scratch: &Path) -> Vec<String> {
    let cfg = scratch.join("cfg");
    let home = scratch.join("home");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    vec![
        format!("ANTHROPIC_BASE_URL=http://127.0.0.1:{port}"),
        "ANTHROPIC_API_KEY=sot-e2e-offline-placeholder".into(),
        format!("CLAUDE_CONFIG_DIR={}", cfg.display()),
        format!("HOME={}", home.display()),
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1".into(),
        "DISABLE_TELEMETRY=1".into(),
        // Shorten the helper's interrupt-liveness bound (default 30s) so
        // the pinned-hang scenario concludes in test time.
        "HELPER_TEST_INTERRUPT_TIMEOUT_MS=5000".into(),
    ]
}

/// In-process config: the helper rides env(1) so the overrides are scoped
/// to this capsule's producer, not the test process.
fn e2e_config(scratch: &Path, name: &str, port: u16) -> ClaudeConfig {
    let mut argv = vec!["/usr/bin/env".to_string()];
    argv.extend(offline_env(port, scratch));
    argv.push("node".into());
    argv.push(helper_main_js().to_string_lossy().into_owned());
    ClaudeConfig {
        voyage_root: scratch.join(name),
        voyage_id: name.into(),
        retention: RetentionClass::Discard,
        helper_argv: argv,
        expected_sdk_version: pinned_sdk_version(),
        fence: Fence::test_unfenced(),
    }
}

/// Count needle occurrences across every segment file of the voyage
/// (`.open` included — test rig exception to the never-tail rule, same as
/// the conformance rig's readiness gate).
fn count_in_voyage(root: &Path, needle: &[u8]) -> usize {
    let mut n = 0;
    if let Ok(entries) = std::fs::read_dir(root.join("seg")) {
        for e in entries.flatten() {
            if let Ok(bytes) = std::fs::read(e.path()) {
                n += bytes.windows(needle.len()).filter(|w| *w == needle).count();
            }
        }
    }
    n
}

/// Real turns take seconds (CLI cold boot per query); gate on segment
/// content, never fixed settles. `still` lets a dead driver end the wait.
fn wait_for_count(root: &Path, needle: &[u8], n: usize, mut still: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if count_in_voyage(root, needle) >= n {
            return true;
        }
        if !still() || Instant::now() > deadline {
            return count_in_voyage(root, needle) >= n;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The adapter frees its busy state at the helper's `turn_end` EVENT — a
/// few ms after the turn_close FRAME the waits key on, and never itself a
/// frame. A command sent inside that gap is (correctly) refused, so every
/// post-close command waits out the gap first; `refused_turns == 0` in the
/// summary asserts the settle was enough, legibly.
const TURN_END_SETTLE: Duration = Duration::from_millis(750);

/// Successor-readiness gate: true when ONE `.open` segment contains every
/// needle. The crashed epoch's `.open` lingers (with its own
/// producer_ready) until recovery renames it, and recovery intermediates
/// duplicate its bytes — but only the SUCCESSOR's segment ever holds both
/// its synthesized_death closes and its own producer_ready.
fn open_has_all(root: &Path, needles: &[&[u8]]) -> bool {
    if let Ok(entries) = std::fs::read_dir(root.join("seg")) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "open") {
                if let Ok(bytes) = std::fs::read(&p) {
                    if needles
                        .iter()
                        .all(|n| bytes.windows(n.len()).any(|w| w == *n))
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn wait_open_has_all(root: &Path, needles: &[&[u8]], mut still: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if open_has_all(root, needles) {
            return true;
        }
        if !still() || Instant::now() > deadline {
            return open_has_all(root, needles);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn all_sealed_frames(root: &Path) -> Vec<sot_log::Envelope> {
    let seg_dir = root.join("seg");
    let mut names: Vec<String> = std::fs::read_dir(&seg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".sotseg"))
        .collect();
    names.sort();
    let mut out = vec![];
    for n in names {
        out.extend(SegmentReader::read(&seg_dir.join(n), true).unwrap().frames);
    }
    out
}

/// Process-tree quiescence (ADR 0040 §Gates): nothing spawned for this
/// fixture may survive it. The scratch path rides only the ENVIRONMENT
/// (CLAUDE_CONFIG_DIR/HOME) — cmdlines never carry it — so the scan reads
/// /proc/*/environ (own-uid processes are readable; that covers everything
/// this test can have spawned).
fn assert_no_survivors(scratch: &Path) {
    let needle = scratch.to_string_lossy().into_owned().into_bytes();
    let me = std::process::id().to_string();
    let mut survivors = vec![];
    for e in std::fs::read_dir("/proc").unwrap().flatten() {
        let pid = e.file_name().to_string_lossy().into_owned();
        if !pid.chars().all(|c| c.is_ascii_digit()) || pid == me {
            continue;
        }
        if let Ok(env) = std::fs::read(e.path().join("environ")) {
            if env.windows(needle.len()).any(|w| w == needle) {
                let cmd = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
                survivors.push(format!("{pid}: {}", String::from_utf8_lossy(&cmd).replace('\0', " ")));
            }
        }
    }
    assert!(survivors.is_empty(), "processes survived the fixture:\n{}", survivors.join("\n"));
}

/// THE RULE-5 GATE (ADR 0040 §Attribution): two turns through the real
/// pinned stack, the second resuming the first's session. On resume the
/// SDK must NOT replay prior mainline messages — a replayed id-less
/// assistant/user message would be attributed to the CURRENT turn by rule
/// 5 and break the exact counts below. The same run is the basic
/// integration proof: attestation, WAL, attribution, verify — all against
/// the real helper.
#[test]
fn e2e_two_turns_resume_no_replay() {
    if !gate() {
        return;
    }
    let _s = lock();
    let api = FakeApi::start();
    let (base, _scratch) = scratch();
    let dir = base.as_path();
    let cfg = e2e_config(dir, "e1", api.port);
    let root = cfg.voyage_root.clone();
    let (tx, rx) = mpsc::channel();
    let h = std::thread::spawn(move || run(cfg, rx).unwrap());
    assert!(wait_for_count(&root, b"producer_ready", 1, || !h.is_finished()), "helper never ready");
    tx.send(OperatorCmd::Turn("hello fixture".into())).unwrap();
    assert!(wait_for_count(&root, b"turn_close", 1, || !h.is_finished()), "turn 1 did not close");
    std::thread::sleep(TURN_END_SETTLE);
    tx.send(OperatorCmd::Turn("second turn".into())).unwrap();
    assert!(wait_for_count(&root, b"turn_close", 2, || !h.is_finished()), "turn 2 did not close");
    std::thread::sleep(TURN_END_SETTLE);
    tx.send(OperatorCmd::Shutdown).unwrap();
    drop(tx);
    let summary = h.join().unwrap();
    assert_eq!(summary.refused_turns, 0, "a command landed in the close-to-turn_end gap");
    assert_eq!(summary.turns, 2, "terminal: {}", summary.terminal_reason);
    assert_eq!(summary.terminal_reason, "shutdown");
    assert_eq!(summary.unresolved_correlation_warnings, 0, "rule-5 model drift");
    verify_voyage(&root, "e1").unwrap();

    let frames = all_sealed_frames(&root);
    let opens: Vec<_> = frames.iter().filter(|f| f.class == Class::TurnOpen).collect();
    assert_eq!(opens.len(), 2);
    // Exactly one assistant message per turn, each attributed to its own
    // turn, carrying its own fixture text. Replay breaks the count; a
    // shuffled attribution breaks the text binding.
    let assistants: Vec<_> = frames
        .iter()
        .filter(|f| {
            f.class == Class::Producer
                && f.payload.as_ref().is_some_and(|p| p["type"] == "assistant")
        })
        .collect();
    assert_eq!(assistants.len(), 2, "one assistant message per turn — no replay");
    for (i, a) in assistants.iter().enumerate() {
        let t = opens[i].seq;
        assert!(
            a.refs.iter().any(|r| r.kind == RefKind::CausedBy && r.frame == t),
            "assistant {i} attributed to its own turn"
        );
        let text = a.payload.as_ref().unwrap().pointer("/message/content/0/text").unwrap();
        assert_eq!(text, &serde_json::json!(format!("fixture reply {}", i + 1)));
    }
    // A replayed prior user message on resume would land as a user-type
    // work-product frame; the real SDK emits none.
    let users = frames
        .iter()
        .filter(|f| f.class == Class::Producer && f.payload.as_ref().is_some_and(|p| p["type"] == "user"))
        .count();
    assert_eq!(users, 0, "replayed user message detected");
    // Resume proof — the gate is vacuous unless turn 2 actually resumed
    // turn 1's session.
    let results: Vec<_> = frames
        .iter()
        .filter(|f| f.class == Class::Producer && f.payload.as_ref().is_some_and(|p| p["type"] == "result"))
        .collect();
    assert_eq!(results.len(), 2);
    let sid = |f: &sot_log::Envelope| f.payload.as_ref().unwrap()["session_id"].clone();
    assert_eq!(sid(results[0]), sid(results[1]), "turn 2 did not resume turn 1's session");
    assert!(sid(results[0]).as_str().is_some_and(|s| !s.is_empty()));
    drop(api);
    assert_no_survivors(dir);
}

/// Interrupt against the REAL SDK with a genuinely in-flight query — and
/// this PINS the SDK's observed reality: 0.3.241 hangs `interrupt()`
/// forever when the query is mid-API-call (isolated repro: the CLI aborts
/// the HTTP request, then neither settles the promise nor concludes the
/// iterator; identical against a stalled and a flowing stream). The
/// helper's interrupt-liveness bound turns that hang into the terminal
/// `interrupt_unanswered`, and the adapter closes the abandoned turn
/// honestly. An SDK bump that FIXES the hang must fail this test — re-pin
/// it then to the fixed semantics (a close with reason "interrupted").
#[test]
fn e2e_interrupt_during_in_flight_query() {
    if !gate() {
        return;
    }
    let _s = lock();
    let api = FakeApi::start();
    let (base, _scratch) = scratch();
    let dir = base.as_path();
    let cfg = e2e_config(dir, "e2", api.port);
    let root = cfg.voyage_root.clone();
    let (tx, rx) = mpsc::channel();
    let h = std::thread::spawn(move || run(cfg, rx).unwrap());
    assert!(wait_for_count(&root, b"producer_ready", 1, || !h.is_finished()), "helper never ready");
    tx.send(OperatorCmd::Turn("SOT-DRIP hold this turn".into())).unwrap();
    assert!(
        wait_for_count(&root, b"\"forwarded\"", 1, || !h.is_finished()),
        "turn never forwarded"
    );
    // Let the query reach the flowing stream (forwarded commits at the
    // helper's stdin, a beat before the HTTP request lands).
    std::thread::sleep(Duration::from_millis(750));
    tx.send(OperatorCmd::Interrupt).unwrap();
    assert!(
        wait_for_count(&root, b"turn_close", 1, || !h.is_finished()),
        "the interrupt-liveness bound never concluded the turn"
    );
    drop(tx);
    let summary = h.join().unwrap();
    assert!(
        summary.terminal_reason.contains("interrupt_unanswered"),
        "real-SDK interrupt semantics changed — re-pin this fixture (terminal: {})",
        summary.terminal_reason
    );
    verify_voyage(&root, "e2").unwrap();

    let frames = all_sealed_frames(&root);
    let close = frames
        .iter()
        .find(|f| f.class == Class::TurnClose)
        .expect("turn close present");
    let reason = close.payload.as_ref().unwrap()["reason"].as_str().unwrap().to_string();
    assert_eq!(reason, "synthesized_death", "abandoned turn closed by the termination path");
    // The interrupt REQUEST committed durably before the op reached the
    // helper; no response ever came — the record must say exactly that.
    let phases: Vec<&str> = frames
        .iter()
        .filter_map(|f| f.payload.as_ref().and_then(|p| p["phase"].as_str()))
        .collect();
    assert!(phases.contains(&"request"), "interrupt request frame missing");
    assert!(!phases.contains(&"response"), "a response frame appeared for an unanswered interrupt");
    drop(api);
    assert_no_survivors(dir);
}

/// The P1 kill sweep rerun under this adapter, on a delegated host: kill
/// -9 the CAPSULE mid-turn, prove the helper subtree survives as orphans,
/// act on the RECORDED locator exactly as a successor epoch would
/// (cgroup.kill → populated 0), then run the successor epoch and verify
/// the whole voyage Complete-green. First live execution of the cgroup
/// fence. Skips (loudly) where cgroup delegation is unavailable.
#[test]
fn e2e_kill_domain_sweep() {
    if !gate() {
        return;
    }
    let _s = lock();
    match Fence::discover("e2e-probe") {
        Ok(Fence::Cgroup(p)) => {
            let _ = std::fs::remove_dir(&p);
        }
        _ => {
            eprintln!("skipped: no cgroup delegation on this host");
            return;
        }
    }
    let api = FakeApi::start();
    let (base, _scratch) = scratch();
    let dir = base.as_path();
    let root = dir.join("e3");
    let mut capsule = Command::new(env!("CARGO_BIN_EXE_sot-capsule"));
    capsule.args([
        "claude",
        &root.to_string_lossy(),
        "e3",
        &helper_main_js().to_string_lossy(),
        &pinned_sdk_version(),
    ]);
    for kv in offline_env(api.port, dir) {
        let (k, v) = kv.split_once('=').unwrap();
        capsule.env(k, v);
    }
    // Null stdio: an inherited pipe held by a leaked descendant would hold
    // the whole test runner hostage on a failure; the log frames are the
    // diagnostics. KillOnDrop makes a panicking assert leave no orphans.
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut capsule = KillOnDrop(
        capsule
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sot-capsule binary"),
    );
    let mut stdin = capsule.0.stdin.take().unwrap();
    let pid = capsule.0.id() as i32;
    // A pre-ready turn is (correctly) refused and never forwarded — wait
    // for the producer before submitting, same as the in-process scenarios.
    assert!(
        wait_for_count(&root, b"producer_ready", 1, || matches!(capsule.0.try_wait(), Ok(None))),
        "capsule producer never ready"
    );
    writeln!(stdin, "{}", serde_json::json!({"turn": "SOT-STALL hold"})).unwrap();
    assert!(
        wait_for_count(&root, b"\"forwarded\"", 1, || matches!(capsule.0.try_wait(), Ok(None))),
        "turn never forwarded (capsule died early?)"
    );
    std::thread::sleep(Duration::from_millis(750));
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let _ = capsule.0.wait();

    // The successor's act: read the authority-bearing locator from the
    // (unsealed, possibly torn) log — never from ambient state.
    let open_seg = std::fs::read_dir(root.join("seg"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "open"))
        .expect(".open segment after SIGKILL");
    let reader = SegmentReader::read(&open_seg, false).unwrap();
    let locator = reader
        .frames
        .iter()
        .filter(|f| f.class == Class::Lifecycle)
        .find_map(|f| {
            let p = f.payload.as_ref()?;
            if p["kind"] == "producer_spawn" {
                Some(p["detail"]["kill_domain"].clone())
            } else {
                None
            }
        })
        .expect("producer_spawn locator in the log");
    assert_eq!(locator["scheme"], "cgroup");
    let fence_dir = PathBuf::from(locator["path"].as_str().unwrap());
    // The orphaned helper subtree survived the capsule — the reason the
    // fence exists.
    let procs = std::fs::read_to_string(fence_dir.join("cgroup.procs")).unwrap();
    assert!(!procs.trim().is_empty(), "no orphans — the sweep proved nothing");
    std::fs::write(fence_dir.join("cgroup.kill"), "1").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ev = std::fs::read_to_string(fence_dir.join("cgroup.events")).unwrap_or_default();
        if ev.lines().any(|l| l.trim() == "populated 0") || ev.is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "kill domain did not quiesce");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::remove_dir(&fence_dir).expect("quiesced domain removable");

    // The successor epoch: recovers the torn tail, closes the orphan turn,
    // runs a normal turn, and the WHOLE voyage verifies Complete.
    let cfg2 = e2e_config(dir, "e3", api.port);
    let (tx, rx) = mpsc::channel();
    let h = std::thread::spawn(move || run(cfg2, rx).unwrap());
    // Readiness gates on the SUCCESSOR's own .open segment — the only file
    // that ever holds both its synthesized closes and its own ready.
    assert!(
        wait_open_has_all(&root, &[b"synthesized_death", b"producer_ready"], || !h.is_finished()),
        "successor never ready"
    );
    tx.send(OperatorCmd::Turn("hello fixture".into())).unwrap();
    assert!(wait_for_count(&root, b"turn_close", 2, || !h.is_finished()), "successor turn did not close");
    std::thread::sleep(TURN_END_SETTLE);
    tx.send(OperatorCmd::Shutdown).unwrap();
    drop(tx);
    let summary = h.join().unwrap();
    assert_eq!(summary.refused_turns, 0, "a command landed in the close-to-turn_end gap");
    assert_eq!(summary.turns, 1, "terminal: {}", summary.terminal_reason);
    verify_voyage(&root, "e3").unwrap();
    drop(api);
    assert_no_survivors(dir);
}
