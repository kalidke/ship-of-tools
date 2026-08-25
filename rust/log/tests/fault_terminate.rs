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

#[test]
fn terminate_sweep_recovers_green_every_round() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(VOYAGE);
    VoyageStore::bootstrap(&root, VOYAGE, RetentionClass::Discard).unwrap();

    let writer_bin = env!("CARGO_BIN_EXE_sot-fault-writer");
    let mut sealed_frames_before: u64 = 0;

    for round in 0..ROUNDS {
        // A chatty writer that would run ~forever; the kill is what ends it.
        let mut writer = std::process::Command::new(writer_bin)
            .args([root.to_str().unwrap(), VOYAGE])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sot-fault-writer");

        std::thread::sleep(Duration::from_millis(delay_ms(round)));
        // Child::kill(): TerminateProcess on Windows, SIGKILL on unix — no
        // drop handlers, no seal, no flush — the crash the format exists to
        // survive. No PTY orphan to reap here (unlike fault_kill.rs): this
        // writer has no child of its own.
        writer.kill().unwrap();
        writer.wait().unwrap();

        // Reopen = reconcile + recover under the writer lock. The next
        // incarnation must (a) come up, (b) seal the previous run's tip,
        // (c) leave the voyage verify-green with nothing sealed lost.
        let mut store = VoyageStore::open_for_writing(&root, VOYAGE)
            .unwrap_or_else(|e| panic!("round {round}: reopen after kill failed: {e}"));
        store.seal_survivor().unwrap_or_else(|e| {
            panic!("round {round}: survivor seal failed: {e}");
        });
        drop(store); // release the lock before verify + the next writer

        verify_voyage(&root, VOYAGE)
            .unwrap_or_else(|e| panic!("round {round}: verify failed after recovery: {e}"));

        let sealed_now = count_sealed_frames(&root);
        assert!(
            sealed_now >= sealed_frames_before,
            "round {round}: sealed history shrank ({sealed_frames_before} -> {sealed_now})"
        );
        sealed_frames_before = sealed_now;
    }

    // The sweep must have actually recorded something across the rounds —
    // a vacuous pass (writer killed before any frame every time) would
    // prove nothing. The preamble alone (2 frames) guarantees frames per
    // round, so demand evidence of at least half the rounds landing real
    // history.
    assert!(
        sealed_frames_before >= (ROUNDS as u64 / 2) * 2,
        "sweep too vacuous: only {sealed_frames_before} sealed frames after {ROUNDS} rounds"
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
