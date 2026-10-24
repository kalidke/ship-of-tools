//! Capsule tests that exercise real Windows mechanism (ConPTY, job objects).

    use super::*;

/// Test 2: spawn failure (a nonexistent executable) is compensated, not escaped with nothing for the supervisor to
/// read: `ended` is still left, finished, naming the failure.
#[test]
fn spawn_failure_is_compensated() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["Z:\\sot_capsule_win_test_no_such_exe_9f31.exe".to_string()];
    let cfg = config(dir.path(), "fail1", argv, 80, 25);
    let root = cfg.state_dir.clone();
    let (_tx, rx) = mpsc::channel();
    let summary = capsule::run::<P>(cfg, rx).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::SpawnFailed);
    assert_eq!(summary.exit_code, None);
    let ended = ended_of(&root);
    assert!(ended.finished && ended.exit_code.is_none());
    assert!(ended.reason.as_deref().is_some_and(|r| r.starts_with("spawn_failed")), "{ended:?}");
}


/// Test 4: resize rejects out-of-budget requests rather than clamping them, and `ResizePseudoConsole` is
/// never invoked for them (the attach's size and the in-budget request only — review
/// finding: the disposition string alone doesn't prove the OS call was
/// really gated). Drives resize the way a real driver does: attach as the
/// driver on the row socket, then three `resize` requests.
#[test]
fn resize_commits_and_rejects() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let side = rowside::RowSide::new();
    let argv = vec![SHELL_ARGV.to_string()];
    let mut cfg = config(dir.path(), "resize1", argv, 80, 25);
    cfg.signals = Some(Arc::clone(&side.signals));
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule::run::<P>(cfg, rx));

    let mut driver = side.driver(80, 25);
    let ok1 = driver.resize(100, 40).is_ok(); // in budget
    let ok2 = driver.resize(9999, 40).is_ok(); // > 512 cols
    let ok3 = driver.resize(40, 1).is_ok(); // < 2 rows
    assert!(ok1 && !ok2 && !ok3, "expected ok, refused, refused, got {ok1} {ok2} {ok3}");

    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    // A driver's attach applies its own size first (one call), then the in-budget resize; the refused ones never reach
    // `ResizePseudoConsole`.
    assert_eq!(summary.resize_os_calls, 2, "expected ResizePseudoConsole for the attach's size and the valid request only");
}

/// Test 5: flood. A producer emits well beyond the 8 MiB output budget;
/// the run must drain it all and finish without deadlocking. Whether the budget ever actually BLOCKED during the flood
/// is deliberately NOT asserted here — engagement depends on conhost's
/// burst pacing on the runner, which nothing here controls (a runner-image
/// change turned exactly that assertion red on unchanged code); the
/// blocking property is proven deterministically by OutputBudget's own
/// unit tests in capsule/. Run on a background thread with a LOCAL
/// bounded wait: a teardown regression here is exactly a deadlock, and
/// this test must fail loud within its own bound rather than consume the
/// whole CI job's timeout.
#[test]
fn flood_drains_without_deadlock() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let helper = HELPER_EXE.to_string();
    let total: usize = 20 * 1024 * 1024; // > the 8 MiB producer-channel budget
    let argv = vec![helper, "--flood".to_string(), total.to_string()];
    let cfg = config(dir.path(), "flood1", argv, 80, 25);
    let root = cfg.state_dir.clone();
    let (_tx, rx) = mpsc::channel();
    let start = Instant::now();
    let handle = std::thread::spawn(move || capsule::run::<P>(cfg, rx));
    let summary = wait_for_join(handle, Duration::from_secs(60))
        .expect("run did not return within the local deadline (deadlock?)")
        .unwrap();
    eprintln!("capsule_win flood finding: {total} bytes in {:?}", start.elapsed());
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    assert_eq!(summary.exit_code, Some(ExitStatus::Code(0)));
    assert!(ended_of(&root).finished);
}

/// Test 6: a high-bit (NTSTATUS-shaped) exit code is preserved raw and
/// unsigned all the way through `ExitSummary` AND the `ended` file — the review finding that a `u32`-to-`i32`
/// cast anywhere in this path would turn it negative for no reason. Same
/// value `tests/conpty.rs` pins at the primitives layer; this proves the
/// capsule runtime doesn't reintroduce the cast above it.
#[test]
fn exit_code_high_bit_status_preserved_through_ended() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv =
        vec![SHELL_ARGV.to_string(), "/d".to_string(), "/c".to_string(), "exit -1073741819".to_string()];
    let cfg = config(dir.path(), "exitcode1", argv, 80, 25);
    let root = cfg.state_dir.clone();
    let (_tx, rx) = mpsc::channel();
    let summary = capsule::run::<P>(cfg, rx).unwrap();
    assert_eq!(summary.exit_code, Some(ExitStatus::Code(0xC000_0005)));
    assert_eq!(ended_of(&root).exit_code, Some(0xC000_0005u32));
}


/// The snapshot a freshly attached viewer is sent: its pieces, joined, up to the one flagged last.
fn collect_snapshot(client: &mut sot_capsule::row::RowClient, within: Duration) -> Vec<u8> {
    use sot_capsule::row::RowEvent;
    let deadline = Instant::now() + within;
    let mut bytes = Vec::new();
    while Instant::now() < deadline {
        match client.next_event(Duration::from_millis(100)).expect("the row stays up") {
            Some(RowEvent::Snapshot { bytes: piece, last }) => {
                bytes.extend(piece);
                if last {
                    return bytes;
                }
            }
            Some(RowEvent::Output(_)) => panic!("output arrived before the snapshot ended"),
            _ => {}
        }
    }
    panic!("no snapshot within {within:?}");
}

/// Test 7: a viewer that attaches mid-stream, on a producer emitting escape sequences and multibyte UTF-8
/// continuously, is sent a snapshot and then exactly the bytes after it: the output of a viewer attached from the start
/// is its from-scratch reference. The producer is `sot-conpty-helper --script` (a deterministic byte emitter that
/// `--linger`s, so it is alive for every step and the run ends by the test's `Kill`).
///
/// Three things are proved:
/// 1. The late viewer's output is the exact byte-for-byte tail of the reference viewer's total output: nothing was
///    dropped, duplicated or reordered across the snapshot's cut.
/// 2. Its snapshot is byte-identical to a checkpoint computed independently by feeding a fresh parser the reference's
///    own start snapshot and the prefix before the cut (checkpoint bytes are a pure function of screen state).
/// 3. The snapshot carries a nonempty scrollback ring, and a parser restored from it and fed the suffix reaches the
///    same checkpoint as a from-scratch parser fed the whole reference stream.
///
/// A resync would send a viewer a second snapshot and break the reference, so the reference viewer is read
/// continuously and the test fails if it sees one.
#[test]
fn attach_mid_stream_snapshot_reproduces_reference_screen() {
    use sot_capsule::row::RowEvent;
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let side = rowside::RowSide::new();
    let argv = vec![HELPER_EXE.to_string(), "--script".to_string(), "1000".to_string(), "--linger".to_string()];
    let (rows, cols) = (25u16, 80u16);
    let mut cfg = config(dir.path(), "midattach1", argv, cols, rows);
    cfg.signals = Some(Arc::clone(&side.signals));
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule::run::<P>(cfg, rx));

    // The reference: a viewer attached at the very start, read continuously until told to stop.
    let mut early = side.watcher(cols, rows);
    let early_snapshot = collect_snapshot(&mut early, Duration::from_secs(20));
    let stop = Arc::new(AtomicBool::new(false));
    let reference = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut frames: Vec<Vec<u8>> = Vec::new();
            let mut resynced = false;
            while !stop.load(Ordering::Acquire) {
                match early.next_event(Duration::from_millis(100)).expect("the reference viewer stays up") {
                    Some(RowEvent::Output(bytes)) => frames.push(bytes),
                    Some(RowEvent::Snapshot { .. }) => resynced = true,
                    _ => {}
                }
            }
            (frames, resynced)
        })
    };

    // Attach WHILE the producer is still emitting, with no attempt to engineer a cut point: `is_ground`'s own unit
    // tests cover that. Long enough that the scrollback assertion below has real content to find (one line roughly
    // every 59 ms, so a couple hundred lines scroll off a 25-row screen in 15 s).
    std::thread::sleep(Duration::from_millis(15_000));
    let mut late = side.watcher(cols, rows);
    let snapshot = collect_snapshot(&mut late, Duration::from_secs(20));

    // Let more output flow after the cut, then end the run and read until the row has been quiet for a second.
    let mut suffix: Vec<u8> = Vec::new();
    let read_for = |late: &mut sot_capsule::row::RowClient, suffix: &mut Vec<u8>, quiet: Duration, at_most: Duration| {
        let (start, mut last_output) = (Instant::now(), Instant::now());
        while start.elapsed() < at_most && last_output.elapsed() < quiet {
            match late.next_event(Duration::from_millis(100)).expect("the late viewer stays up") {
                Some(RowEvent::Output(bytes)) => {
                    suffix.extend(bytes);
                    last_output = Instant::now();
                }
                Some(RowEvent::Snapshot { .. }) => panic!("the late viewer was resynced"),
                _ => {}
            }
        }
    };
    read_for(&mut late, &mut suffix, Duration::from_secs(60), Duration::from_millis(300));
    tx.send(Command::Kill).unwrap();
    read_for(&mut late, &mut suffix, Duration::from_secs(1), Duration::from_secs(30));
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    stop.store(true, Ordering::Release);
    let (early_frames, resynced) = reference.join().unwrap();
    assert!(!resynced, "the reference viewer was resynced, so it is no reference");
    let total: Vec<u8> = early_frames.iter().flatten().copied().collect();

    assert!(!suffix.is_empty(), "expected at least some output after the snapshot");
    assert!(suffix.len() <= total.len(), "the late viewer received more output than the reference ever saw");
    let split = total.len() - suffix.len();
    assert_eq!(
        &total[split..],
        suffix.as_slice(),
        "the output after the snapshot must be the exact byte-for-byte tail of the reference stream"
    );
    let prefix = &total[..split];

    // The snapshot against an independently replayed prefix. The reference parser's scrollback capacity must match the
    // capsule's own (`CAPSULE_SCROLLBACK_ROWS`), or it would disagree about how much of the ring survives.
    let mut at_cut = vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    at_cut.restore_screen(&early_snapshot).expect("the early snapshot must decode");
    at_cut.process(prefix);
    assert_eq!(
        snapshot,
        at_cut.screen().checkpoint().expect("the prefix screen must be representable"),
        "the snapshot must be byte-identical to an independently computed checkpoint of the same prefix"
    );

    // The scrollback ring must have arrived: an empty one on every attach was the defect this guards.
    let mut ring = vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    ring.restore_screen(&snapshot).expect("the snapshot must decode");
    ring.screen_mut().set_scrollback(usize::MAX);
    assert!(ring.screen().scrollback() > 0, "the snapshot must carry a nonempty scrollback ring");

    // The full round trip: restored from the snapshot and fed the suffix, a parser reaches the checkpoint a from-scratch
    // parser reaches on the whole reference stream.
    let mut restored = vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    restored.restore_screen(&snapshot).expect("the snapshot must decode");
    restored.process(&suffix);
    let mut whole = vt100_ctt::Parser::new(rows, cols, capsule::CAPSULE_SCROLLBACK_ROWS);
    whole.restore_screen(&early_snapshot).expect("the early snapshot must decode");
    whole.process(&total);
    assert_eq!(
        restored.screen().checkpoint().expect("the restored screen must be representable"),
        whole.screen().checkpoint().expect("the reference screen must be representable"),
        "a viewer attached mid-stream must reach the screen a viewer attached from the start reached"
    );
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
    let stdout = helper.stdout.take().expect("helper stdout");
    sot_platform::test_wait::within(std::time::Duration::from_secs(30), "the helper's reply line", move || {
        let mut line = String::new();
        std::io::BufReader::new(stdout).read_line(&mut line).expect("read helper reply");
        line.trim().to_string()
    })
}

/// ADR 0043 decision 32 as amended 2026-10-03: the agent job
/// (`AnonymousJob::create`, `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` alone)
/// refuses a breakaway. The helper is assigned to the job, released, and
/// asked to spawn a breakaway child; `CreateProcess` must fail with
/// `ERROR_ACCESS_DENIED` (5) — MSYS asks to break away on every spawn whose
/// job allows it, so a job that allowed it let a row's git-bash children
/// outlive the row.
#[test]
#[cfg(windows)]
fn the_agent_job_refuses_a_breakaway() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

    let job = sot_platform::host::winjob::AnonymousJob::create().expect("create leg job");
    let mut helper = spawn_gated_breakaway_helper();
    let ok = unsafe { AssignProcessToJobObject(job.raw(), helper.as_raw_handle() as HANDLE) };
    assert!(ok != 0, "AssignProcessToJobObject: {}", std::io::Error::last_os_error());

    let reply = release_and_read_reply(&mut helper);
    if let Some(pid) = reply.strip_prefix("pid=") {
        let _ = sot_platform::test_wait::try_run_within(
            std::process::Command::new("taskkill").args(["/F", "/PID", pid]),
            std::time::Duration::from_secs(30),
        );
    }
    let _ = helper.wait();
    assert_eq!(reply, "err=5", "the leg job let a child break away");
}
