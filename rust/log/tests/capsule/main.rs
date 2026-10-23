#![cfg(any(target_os = "linux", target_os = "macos", windows))]
//! Integration tests for the capsule runtime (`src/capsule/`): a run to its end, its `ended` and screen files, and
//! what a viewer on the row socket sees of it. Lives in `tests/` for the same reason `tests/conpty.rs` does: some of
//! these need `env!("CARGO_BIN_EXE_...")` to find a helper binary, which Cargo only wires up for integration test
//! binaries, and the rest are kept here too for one home and one `cargo test -p sot-log --test capsule` filter.
//!
//! Nothing in this file reads `/proc`, opens a pidfd or expects PDEATHSIG: the Linux-kernel-specific mechanism lives in
//! `tests/supervisor/`, which keeps its own gate. `unix_only` below is `cfg(unix)` because what it exercises (a real
//! spawn failure, a real signal death, pty geometry) is Unix mechanism, not Linux mechanism.
//!
//! A viewer is a `RowClient` on the row socket the run feeds (`rowside`), the way every shipped viewer attaches.

use sot_log::capsule::producer::ExitStatus;
use sot_log::capsule::{self, CapsuleConfig, Command, Ended, ExitKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

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

    /// The helper binary the flood test needs: `env!("CARGO_BIN_EXE_...")` only resolves inside an integration test
    /// binary, which is why this lives here rather than in the helper's own `src/bin/*.rs`.
    #[cfg(windows)]
    pub const HELPER_EXE: &str = env!("CARGO_BIN_EXE_sot-conpty-helper");
}
#[cfg(windows)]
use producer_under_test::HELPER_EXE;
use producer_under_test::{SHELL_ARGV, P};

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
        screen_path: Some(dir.join(format!("{name}.screen"))),
        argv,
        cols,
        rows,
        signals: None,
    }
}

/// The `ended` file the leg left in the folder at `root`.
fn ended_of(root: &std::path::Path) -> Ended {
    capsule::read_ended(root).unwrap().expect("the leg left no ended file")
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
    let summary = capsule::run::<P>(cfg, rx).unwrap();
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
    let summary = capsule::run::<P>(cfg, rx).unwrap();
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
    let handle = std::thread::spawn(move || capsule::run::<P>(cfg, rx));
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

mod output_end;
mod row_run;
mod rowside;

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

/// A stop is not an end of the run: `run` returns `Requested`, and `ended` records no request, so a resume of the row
/// starts the agent again.
#[test]
fn a_stop_ends_the_run_without_asking_for_an_end() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), "stop1", vec![SHELL_ARGV.to_string()], 80, 25);
    let root = cfg.voyage_root.clone();
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule::run::<P>(cfg, rx));
    std::thread::sleep(Duration::from_millis(500));
    tx.send(Command::Stop).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    let ended = ended_of(&root);
    assert!(ended.finished && !ended.requested, "a stop asks for no end: {ended:?}");
    assert_eq!(ended.reason.as_deref(), Some("supervisor_stop"));
}
