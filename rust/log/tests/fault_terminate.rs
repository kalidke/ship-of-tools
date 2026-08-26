#![cfg(any(target_os = "linux", windows))] // seals segments -> needs the store's rename arm (linux renameat2 / windows MoveFileExW)
//! Randomized terminate sweep — the PORTABLE fault-harness half of ADR 0041's
//! merge gates: a minimal cross-platform writer binary (`sot-fault-writer`,
//! no PTY, no shell) is killed mid-write via `std::process::Child::kill()`,
//! which the standard library implements as `TerminateProcess` on Windows
//! and `SIGKILL` on unix — so the exact same test exercises the store's
//! crash-recovery path on both OSes. On Windows this IS the kill-sweep gate
//! (ADR 0041 step 2: "a TerminateProcess fault sweep as the kill-sweep
//! analog"). On unix it complements `fault_kill.rs`, which additionally
//! exercises the real `sot-capsule` binary on a real PTY — that coverage
//! (producer-on-PTY, group-commit, echo watermark) is unix-only and stays in
//! that test; this one covers only the store's crash surface, portably.
//!
//! Repeats across many rounds ON THE SAME VOYAGE, so recovery products of
//! round N become the sealed history round N+1 must chain from.
//!
//! Honest scope: this covers crash-at-arbitrary-write-point via process
//! death. It does NOT simulate storage-level faults (ENOSPC/EIO injection,
//! power-loss write reordering below the fsync barrier) — those need a
//! syscall shim or dm-flakey and are named follow-ups in the ADR's gate
//! list, not silently claimed here.

use sot_log::segment::{RetentionClass, SegmentReader, SegmentState};
use sot_log::verify::verify_voyage;
use sot_log::voyage::VoyageStore;
use std::path::Path;
use std::time::Duration;

const ROUNDS: usize = 12;
const VOYAGE: &str = "fault-terminate-voyage";

/// Deterministic-per-run pseudo-random delays without Date/rand deps:
/// mix the round index with the process id.
fn delay_ms(round: usize) -> u64 {
    let x = (round as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(std::process::id() as u64);
    5 + (x % 90)
}

fn count_sealed_frames(root: &Path) -> u64 {
    let seg_dir = root.join("seg");
    let mut n = 0;
    let mut names: Vec<String> = std::fs::read_dir(&seg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        if name.ends_with(".sotseg") {
            let r = SegmentReader::read(&seg_dir.join(&name), true).unwrap();
            n += r.frames.len() as u64;
        }
    }
    n
}

/// The round's live `.open` tip, if any (rounds are sequential and every
/// prior tip is sealed before the next spawn, so at most one exists).
fn open_tip(root: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(root.join("seg"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "open"))
}

#[test]
fn terminate_sweep_recovers_green_every_round() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(VOYAGE);
    VoyageStore::bootstrap(&root, VOYAGE, RetentionClass::Discard).unwrap();

    let writer_bin = env!("CARGO_BIN_EXE_sot-fault-writer");
    let mut sealed_frames_before: u64 = 0;
    let mut torn_rounds: usize = 0;

    for round in 0..ROUNDS {
        // A chatty writer that would run ~forever; the kill is what ends it.
        let mut writer = std::process::Command::new(writer_bin)
            .args([root.to_str().unwrap(), VOYAGE])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sot-fault-writer");

        // Writer-ready handshake: the random delay must measure WRITING
        // time, not process-startup time — without this, a slow spawn eats
        // the whole window and the round silently proves nothing. Ready =
        // the round's .open exists and has grown past the header (the
        // stream is flowing).
        let ready_deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(tip) = open_tip(&root) {
                if std::fs::metadata(&tip).map(|m| m.len() >= 4096).unwrap_or(false) {
                    break;
                }
            }
            if let Some(status) = writer.try_wait().unwrap() {
                panic!("round {round}: writer died before ready ({status:?})");
            }
            if std::time::Instant::now() >= ready_deadline {
                // Dropping a Child does NOT terminate it — a bare panic here
                // would leak an endless disk-writing process.
                let _ = writer.kill();
                let _ = writer.wait();
                panic!("round {round}: writer never became ready");
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        std::thread::sleep(Duration::from_millis(delay_ms(round)));
        // Child::kill(): TerminateProcess on Windows, SIGKILL on unix — no
        // drop handlers, no seal, no flush — the crash the format exists to
        // survive. No PTY orphan to reap here (unlike fault_kill.rs): this
        // writer has no child of its own.
        writer.kill().unwrap();
        let status = writer.wait().unwrap();
        // The kill must be what ended it — CAUSALLY, not just "unsuccessful"
        // (`Child::kill()` returns Ok on an already-exited child). Unix:
        // the SIGKILL signal. Windows: std's kill is TerminateProcess(_, 1)
        // and the writer reserves exit 1 for exactly that (organic failure
        // exits 3; a clean end is unrepresentable), so code 1 proves the
        // termination was ours.
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(
                status.signal(),
                Some(libc::SIGKILL),
                "round {round}: writer not ended by SIGKILL ({status:?})"
            );
        }
        #[cfg(windows)]
        assert_eq!(
            status.code(),
            Some(1),
            "round {round}: writer not ended by TerminateProcess ({status:?})"
        );

        // Observe (read-only) whether this round's tip is provably torn —
        // deterministic tear coverage lives in reconcile_matrix; here we
        // just report how often the randomized sweep hit one.
        if let Some(tip) = open_tip(&root) {
            if let Ok(r) = SegmentReader::read(&tip, false) {
                if r.tail_tear.is_some() {
                    torn_rounds += 1;
                }
            }
        }

        // Reopen = reconcile + recover under the writer lock. The next
        // incarnation must (a) come up, (b) seal the previous run's tip,
        // (c) leave the voyage verify-green with nothing sealed lost.
        // Bounded retry on "lock held": Windows documents post-termination
        // lock release as resource-dependent with no bound — a slow release
        // fails closed, so retrying is correct (and what a real supervisor
        // does).
        let reopen_deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut store = loop {
            match VoyageStore::open_for_writing(&root, VOYAGE) {
                Ok(s) => break s,
                Err(sot_log::Error::State(m))
                    if m.contains("lock held") && std::time::Instant::now() < reopen_deadline =>
                {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => panic!("round {round}: reopen after kill failed: {e}"),
            }
        };
        store.seal_survivor().unwrap_or_else(|e| {
            panic!("round {round}: survivor seal failed: {e}");
        });
        drop(store); // release the lock before verify + the next writer

        verify_voyage(&root, VOYAGE)
            .unwrap_or_else(|e| panic!("round {round}: verify failed after recovery: {e}"));

        // EVERY round must land real history: the handshake guarantees the
        // kill hit an actively writing process, so at least the preamble's
        // 2 frames of that round's epoch survive recovery. (This per-round
        // bound replaces a cumulative total, which one productive round
        // could have satisfied alone.)
        let sealed_now = count_sealed_frames(&root);
        assert!(
            sealed_now >= sealed_frames_before + 2,
            "round {round}: no new sealed history ({sealed_frames_before} -> {sealed_now})"
        );
        sealed_frames_before = sealed_now;
    }

    eprintln!(
        "terminate sweep: {torn_rounds}/{ROUNDS} rounds hit a provably torn tail \
         ({sealed_frames_before} sealed frames total)"
    );

    // And no residue: quiescent state = only .sotseg files (each round's
    // reopen sealed the previous tip; the last round's tip was sealed by the
    // final seal_survivor above).
    let residue: Vec<String> = std::fs::read_dir(root.join("seg"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(&format!(".{}", SegmentState::Sealed.ext())))
        .collect();
    assert!(
        residue.is_empty(),
        "non-sealed residue after final recovery: {residue:?}"
    );
}
