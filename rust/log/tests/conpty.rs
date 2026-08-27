#![cfg(windows)]
//! Integration tests for the owned ConPTY + job containment layer
//! (`src/conpty.rs`, ADR 0041 §"Containment and the owned ConPTY layer").
//!
//! Lives in `tests/` rather than an inline `#[cfg(test)] mod` because one
//! of these (the containment test) needs `env!("CARGO_BIN_EXE_...")` to
//! find its helper binary, which Cargo only wires up for integration test
//! binaries — not for a library's own unit tests. The rest are kept here
//! too for one home and one `cargo test -p sot-log conpty` filter.

use sot_log::conpty::ConptySpawn;
use sot_log::Error;
use std::fs::File;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HANDLE;

/// Move `reader` into a background thread that reads until EOF/error,
/// forwarding each chunk over the returned channel — the Microsoft
/// walkthrough's own recommendation ("service each channel on a separate
/// thread") applied to a test. EOF here does NOT mean "the child exited":
/// ConPTY's internal host owns `hOutput` and keeps it open until
/// `ClosePseudoConsole`, regardless of whether the hosted child is still
/// running — so EOF only ever arrives after `close_pty`, which is why
/// every test below polls `active_processes` for the child's exit and only
/// calls `close_pty` afterward, with this thread already draining.
fn spawn_reader_thread(mut reader: File) -> (Receiver<Vec<u8>>, std::thread::JoinHandle<()>) {
    use std::io::Read;
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    (rx, handle)
}

/// Bounded-wait for the spawned child to exit, observed through the JOB
/// (not a process handle — `conpty.rs` deliberately doesn't keep one).
fn wait_for_zero_active(job: &sot_log::conpty::AnonymousJob, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let n = job.active_processes().unwrap();
        if n == 0 {
            return;
        }
        assert!(Instant::now() < deadline, "active_processes never reached 0 (last: {n})");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Every `pid:<digits>` occurrence in `text`, in order.
fn extract_pids(text: &str) -> Vec<u32> {
    let mut pids = Vec::new();
    let mut rest = text;
    while let Some(idx) = rest.find("pid:") {
        rest = &rest[idx + 4..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(pid) = digits.parse() {
            pids.push(pid);
        }
    }
    pids
}

/// Test 1: E2E spawn. `cmd.exe /d /c echo <marker>` runs to completion
/// under the owned pseudoconsole (`/d`: skip any machine/user AutoRun
/// registry hook — a runner with one configured would otherwise corrupt
/// every capture in this file); the marker must appear in what comes out
/// `reader`, and the job's `ActiveProcesses` must reach 0 once the shell
/// exits — proving both the data path and the containment accounting work
/// end to end. Also carries the `IsProcessInJob` probe on THIS test process
/// (formerly its own spawn — folded in here, one fewer spawn): informational
/// only, never asserted on, since hosted CI runners have historically been
/// jobbed themselves by the runner's own supervision, a fact about the
/// environment, not a defect.
#[test]
fn e2e_spawn_echoes_marker_and_job_reaches_zero_active() {
    let marker = "SOT_CONPTY_MARKER_9f3a";
    let argv = vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), format!("echo {marker}")];
    let spawn = ConptySpawn::spawn(&argv, 80, 25).unwrap();
    eprintln!(
        "conpty probe: this test process is jobbed = {:?}",
        spawn.detail.spawning_process_was_jobbed
    );
    let (rx, reader_thread) = spawn_reader_thread(spawn.reader);

    // Wait for the CONTENT while the session is live — never close first
    // and hope the final frame carried it. Timed chunk log so a timeout
    // failure records WHEN each frame arrived, not just what.
    let start = Instant::now();
    let mut all = Vec::new();
    let mut seen = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !seen {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                eprintln!(
                    "e2e chunk @{}ms: {:?}",
                    start.elapsed().as_millis(),
                    String::from_utf8_lossy(&chunk)
                );
                all.extend(chunk);
                seen = String::from_utf8_lossy(&all).contains(marker);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                eprintln!("e2e reader DISCONNECTED @{}ms", start.elapsed().as_millis());
                break;
            }
        }
    }

    wait_for_zero_active(&spawn.job, Duration::from_secs(15));
    spawn.pty.close_pty();
    reader_thread.join().unwrap();
    while let Ok(chunk) = rx.recv() {
        eprintln!("e2e post-close chunk: {:?}", String::from_utf8_lossy(&chunk));
        all.extend(chunk);
    }
    let text = String::from_utf8_lossy(&all);
    assert!(text.contains(marker), "expected marker in captured output, got: {text:?}");
}

/// Test 2: containment. A parent helper process spawns a grandchild (via a
/// PLAIN `std::process::Command` — no job code of its own; the grandchild
/// joins our job purely through Windows' own default job-membership
/// inheritance, which is exactly the property under test). Both processes
/// are read back by PID from the parent's relayed stdout, opened by
/// handle, and then the job handle is DROPPED — which
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` turns into "kill everything still
/// in the job" regardless of how cleanly (or not) this test would
/// otherwise have torn things down. Both process handles must signal (die)
/// within the bound.
#[test]
fn containment_kills_child_and_grandchild() {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_FAILED, WAIT_OBJECT_0};
    use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForMultipleObjects, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let helper = env!("CARGO_BIN_EXE_sot-conpty-helper").to_string();
    let spawn = ConptySpawn::spawn(&[helper], 80, 25).unwrap();
    let (rx, _reader_thread) = spawn_reader_thread(spawn.reader);

    // Readiness by CONTENT (both `pid:` lines seen), never a fixed sleep
    // and never `start /b` (which would detach the grandchild from the
    // plain parent-child relationship under test).
    let mut text = String::new();
    let mut pids = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while pids.len() < 2 {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                text.push_str(&String::from_utf8_lossy(&chunk));
                pids = extract_pids(&text);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        assert!(Instant::now() < deadline, "never saw two pid: lines; captured so far: {text:?}");
    }
    assert!(pids.len() >= 2, "expected parent + grandchild pid, got {pids:?}");

    let access = PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE;
    let handles: Vec<HANDLE> = pids[..2]
        .iter()
        .map(|&pid| {
            let h = unsafe { OpenProcess(access, 0, pid) };
            assert!(!h.is_null(), "OpenProcess({pid}) failed: {}", std::io::Error::last_os_error());
            h
        })
        .collect();

    // Drop just the job (a whole-`spawn` drop isn't available here — its
    // `reader` was already moved into the thread above — but the job's
    // handle closing is the exact mechanism under test; `pty`/`writer`
    // dropping too, moments later at scope end, changes nothing about it).
    drop(spawn.job);

    let wait_ms = 10_000u32;
    let result = unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 1, wait_ms) };
    // Microsoft's documented contract for bWaitAll=TRUE: success is the
    // RANGE WAIT_OBJECT_0..WAIT_OBJECT_0+nCount, not only the exact value
    // WAIT_OBJECT_0 — asserting equality alone is stricter than the API
    // actually promises.
    let n = handles.len() as u32;
    match result {
        WAIT_FAILED => panic!(
            "WaitForMultipleObjects failed: {}",
            std::io::Error::last_os_error()
        ),
        r if (WAIT_OBJECT_0..WAIT_OBJECT_0 + n).contains(&r) => {}
        r => panic!("expected BOTH process handles to signal (die) within {wait_ms}ms; result={r}"),
    }
    for h in handles {
        unsafe { CloseHandle(h) };
    }
}

/// Test 3: partial-spawn unwind, repeated. A nonexistent executable fails
/// at `CreateProcessW` specifically (every earlier stage — pipes,
/// pseudoconsole, job, attribute list — succeeds; only the final call
/// fails) — 50 times in a row, proving each failed attempt's resources
/// actually released (RAII drop order, not manual cleanup: nothing in
/// `spawn`'s error branches runs any cleanup code at all). A REAL spawn
/// immediately after must still succeed — the absence of resource
/// exhaustion IS the test.
#[test]
fn unwind_repeated_failed_spawns_then_one_real_spawn_succeeds() {
    let bogus = vec![r"Z:\this\path\definitely\does\not\exist.exe".to_string()];
    for i in 0..50 {
        match ConptySpawn::spawn(&bogus, 80, 25) {
            Err(Error::Conpty(e)) => {
                assert_eq!(e.op, "CreateProcessW", "attempt {i}: {e}");
                // The CLASS matters, not just the stage: the first real
                // Windows run failed here with ERROR_INVALID_HANDLE (a
                // broken attribute list) and this assert's stage-only
                // predecessor PASSED on it — a wrong-reason pass. A
                // nonexistent path must fail as not-found (2) or
                // path-not-found (3), nothing else.
                let code = e.source.raw_os_error();
                assert!(
                    matches!(code, Some(2) | Some(3)),
                    "attempt {i}: expected FILE/PATH_NOT_FOUND, got {e}"
                );
            }
            Err(other) => panic!("attempt {i}: expected Error::Conpty, got {other:?}"),
            Ok(_) => panic!("attempt {i}: nonexistent executable unexpectedly spawned"),
        }
    }

    let argv = vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), "exit 0".to_string()];
    let spawn = ConptySpawn::spawn(&argv, 80, 25).unwrap();
    let (_rx, reader_thread) = spawn_reader_thread(spawn.reader);
    wait_for_zero_active(&spawn.job, Duration::from_secs(10));
    spawn.pty.close_pty();
    reader_thread.join().unwrap();
}

/// Test 4: the contract test (replaces a dead spike, per the step-4 spec
/// gate). With `dwFlags = 0` (no cursor inheritance), records whether
/// conhost answers DA1 (`ESC[c`) unprompted on `hOutput` at startup — the
/// next unit's query-answer state machine needs this settled, so it is
/// LOGGED, not asserted on. What IS asserted: the session still produces
/// the requested output correctly without this process ever answering
/// anything on `writer`.
#[test]
fn contract_first_output_bytes_and_da1_presence() {
    let marker = "SOT_CONPTY_CONTRACT_7e21";
    let argv = vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), format!("echo {marker}")];
    let spawn = ConptySpawn::spawn(&argv, 80, 25).unwrap();
    let (rx, reader_thread) = spawn_reader_thread(spawn.reader);

    // Read until the MARKER itself, bounded by a deadline — not a fixed
    // collection window. A fixed window can under-collect on a loaded
    // runner and log a false-negative DA1 finding even though the marker
    // assertion at the end would still have passed; reading until the
    // marker arrives means the accumulated prefix inspected below is
    // whatever actually preceded it, not an arbitrary time slice.
    let mut all = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !String::from_utf8_lossy(&all).contains(marker) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => all.extend(chunk),
            Err(RecvTimeoutError::Timeout) => {
                assert!(
                    Instant::now() < deadline,
                    "marker never arrived; captured so far: {:?}",
                    String::from_utf8_lossy(&all)
                );
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // Precise, or the finding is noise: cmd output through ConPTY is FULL
    // of ordinary CSI sequences, so "any ESC[" would always be true and
    // tell the next unit nothing. The host-facing DA1 query is exactly
    // ESC [ c (with ESC [ 0 c as its legal synonym); log that, plus an
    // escaped prefix of the raw first bytes so the next unit's state
    // machine is designed against recorded reality rather than a boolean.
    let da1_query = all.windows(3).any(|w| w == b"\x1b[c")
        || all.windows(4).any(|w| w == b"\x1b[0c");
    let prefix: String = all.iter().take(200).map(|&b| {
        if (0x20..0x7f).contains(&b) { (b as char).to_string() } else { format!("\\x{b:02x}") }
    }).collect();
    eprintln!("conpty contract finding: DA1 query (ESC[c / ESC[0c) in first bytes = {da1_query}");
    eprintln!("conpty contract finding: first bytes = {prefix}");

    wait_for_zero_active(&spawn.job, Duration::from_secs(10));
    spawn.pty.close_pty();
    reader_thread.join().unwrap();
    while let Ok(chunk) = rx.recv() {
        all.extend(chunk);
    }
    let text = String::from_utf8_lossy(&all);
    assert!(text.contains(marker), "expected marker in output, got: {text:?}");
}

/// Test: the ADR's actual pinned termination sequence — nothing else in
/// this file exercises it (every other test lets the child exit on its
/// own and never calls `terminate` at all). Spawns the HELPER, a live,
/// long-sleeping tree, so termination is doing real work rather than
/// racing a child that was about to exit anyway: `job.terminate()` ->
/// poll `active_processes() == 0` while the reader keeps draining ->
/// `close_pty()` with that same reader still running -> reader EOF/join.
/// Also the only place `resize()` and the `PrimaryProcess` primitives
/// (`wait`, `exit_code`) get exercised at all.
#[test]
fn pinned_termination_sequence_terminate_drain_close() {
    let helper = env!("CARGO_BIN_EXE_sot-conpty-helper").to_string();
    let spawn = ConptySpawn::spawn(&[helper], 80, 25).unwrap();
    let (rx, reader_thread) = spawn_reader_thread(spawn.reader);

    // A valid resize while the session is live — no other test calls this.
    spawn.pty.resize(100, 30).unwrap();

    spawn.job.terminate().unwrap();
    wait_for_zero_active(&spawn.job, Duration::from_secs(10));

    // The primary-process handle survives termination (Windows: a process
    // handle stays valid after its process exits) — `wait` must see it
    // already signaled, and `exit_code_after_confirmed_exit` must report
    // the code `TerminateJobObject` used.
    assert!(
        spawn.process.wait(Duration::from_secs(5)).unwrap(),
        "process handle never signaled after job termination"
    );
    assert_eq!(
        spawn.process.exit_code_after_confirmed_exit().unwrap(),
        1,
        "expected TerminateJobObject's own exit code"
    );

    spawn.pty.close_pty();
    reader_thread.join().unwrap();
    while rx.recv().is_ok() {} // drain to EOF; content is incidental to a kill-based end
}

/// Test: an embedded NUL in an argument has no representation in a
/// NUL-terminated UTF-16 command line — `spawn` must refuse it loudly
/// before building one, rather than let it silently truncate the command
/// line at the first NUL and run something other than what was asked for.
#[test]
fn spawn_rejects_embedded_nul_in_argv() {
    let argv = vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), "echo\0evil".to_string()];
    let err = ConptySpawn::spawn(&argv, 80, 25).unwrap_err();
    assert!(matches!(err, Error::State(_)), "expected Error::State, got {err:?}");
}


/// Test: `GetExitCodeProcess`'s 259/STILL_ACTIVE ambiguity, resolved
/// honestly. cmd.exe's own `exit <n>` calls `ExitProcess(n)` with the raw
/// value — 259 is a legitimate (if Microsoft-discouraged) exit code, and
/// `exit_code_after_confirmed_exit` must report it as exactly that, never
/// silently lost to the STILL_ACTIVE ambiguity (review finding: an earlier
/// version's `exit_code()` mapped raw 259 to `None` unconditionally, even
/// after a confirmed exit).
#[test]
fn exit_code_259_is_reported_raw_not_lost_to_still_active() {
    let argv = vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), "exit 259".to_string()];
    let spawn = ConptySpawn::spawn(&argv, 80, 25).unwrap();
    let (_rx, reader_thread) = spawn_reader_thread(spawn.reader);
    wait_for_zero_active(&spawn.job, Duration::from_secs(15));
    assert!(
        spawn.process.wait(Duration::from_secs(5)).unwrap(),
        "process never signaled"
    );
    assert_eq!(spawn.process.exit_code_after_confirmed_exit().unwrap(), 259);
    spawn.pty.close_pty();
    reader_thread.join().unwrap();
}

/// Test: a high-bit (NTSTATUS-shaped) exit code round-trips as the exact
/// unsigned 32-bit value `GetExitCodeProcess` returns — this is the value
/// an access-violation crash would report, and this crate must preserve
/// its bit pattern rather than reinterpreting it as a Unix-style signed
/// status anywhere below the process-exit boundary (review finding).
/// `-1073741819` is `0xC0000005` (`STATUS_ACCESS_VIOLATION`) reinterpreted
/// as a signed 32-bit literal — `ExitProcess` takes the raw bits, not a
/// signed value, so this is the standard way to make a process exit with
/// that code deliberately.
#[test]
fn exit_code_high_bit_status_round_trips_unsigned() {
    let argv =
        vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), "exit -1073741819".to_string()];
    let spawn = ConptySpawn::spawn(&argv, 80, 25).unwrap();
    let (_rx, reader_thread) = spawn_reader_thread(spawn.reader);
    wait_for_zero_active(&spawn.job, Duration::from_secs(15));
    assert!(
        spawn.process.wait(Duration::from_secs(5)).unwrap(),
        "process never signaled"
    );
    assert_eq!(spawn.process.exit_code_after_confirmed_exit().unwrap(), 0xC000_0005);
    spawn.pty.close_pty();
    reader_thread.join().unwrap();
}
