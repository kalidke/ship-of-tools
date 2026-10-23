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
    let root = cfg.voyage_root.clone();
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
    let root = cfg.voyage_root.clone();
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
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let summary = capsule::run::<P>(cfg, rx).unwrap();
    assert_eq!(summary.exit_code, Some(ExitStatus::Code(0xC000_0005)));
    assert_eq!(ended_of(&root).exit_code, Some(0xC000_0005u32));
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
