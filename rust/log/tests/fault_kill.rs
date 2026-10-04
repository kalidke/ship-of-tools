#![cfg(target_os = "linux")]
//! Randomized kill -9 sweep — the fault-harness half of ADR 0039's merge
//! gates that deterministic surgery can't cover: a REAL capsule process
//! (the actual `sot-capsule` binary, producer on a real PTY) is SIGKILLed at
//! a random moment mid-write, and the store must come back green — every
//! sealed byte intact, the torn tail (if any) provably classified and
//! recovered, the chain continued by the next epoch. Repeats across many
//! rounds ON THE SAME VOYAGE, so recovery products of round N become the
//! sealed history round N+1 must chain from.
//!
//! Honest scope: this covers crash-at-arbitrary-write-point via process
//! death. It does NOT simulate storage-level faults (ENOSPC/EIO injection,
//! power-loss write reordering below the fsync barrier) — those need a
//! syscall shim or dm-flakey and are named follow-ups in the ADR's gate
//! list, not silently claimed here.
//!
//! Known limits (review round 2; logged by ruling, not fixed):
//! - Counts, not content: the checks prove that the frames the waiter saw
//!   survive in number, not unchanged. A recovery that rewrote a frame and
//!   regenerated its CRCs and seal would pass here; recovery.rs's
//!   `torn_tail_recovers_and_keeps_valid_prefix` pins the verbatim copy.
//! - The bound is what the waiter read, not what existed at the kill: a
//!   recovery that drops complete frames appended after that read passes.
//! - Load: the waiter's 30 s deadline, and the producer's 60 s cap (a test
//!   thread starved past it before the kill fails "not SIGKILL").
//! - The `pkill` reap marker is per test, not per run: two concurrent runs
//!   on one host can kill each other's capsules.
//! - Random kill points do not guarantee a torn record or a segment
//!   rotation in any run.

use sot_log::store::segment::{RetentionClass, SegmentReader, SegmentState};
use sot_log::store::verify::verify_voyage;
use sot_log::store::voyage::VoyageStore;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
#[path = "support/capsule_guard.rs"]
mod capsule_guard;

use std::time::Duration;

const ROUNDS: usize = 12;

/// Deterministic-per-run pseudo-random delays without Date/rand deps:
/// mix the round index with the process id.
fn delay_ms(round: usize) -> u64 {
    let x = (round as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(std::process::id() as u64);
    5 + (x % 90)
}

/// Frames in sealed segments: (all, producer output).
fn count_sealed_frames(root: &Path) -> (u64, u64) {
    let seg_dir = root.join("seg");
    let mut n = (0, 0);
    let mut names: Vec<String> = std::fs::read_dir(&seg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        if name.ends_with(".sotseg") {
            let r = SegmentReader::read(&seg_dir.join(&name), true).unwrap();
            n.0 += r.frames.len() as u64;
            n.1 += r.frames.iter().filter(|f| f.class == sot_log::Class::Producer).count() as u64;
        }
    }
    n
}

/// Blocks until the capsule's `.open` segment holds one producer-output frame, and
/// returns how many frames that segment held then. This is the order proof that the
/// kill below lands inside the output stream, under any load that lets the capsule
/// start writing within 30 s.
fn wait_first_output(
    root: &Path,
    capsule: &mut capsule_guard::CapsuleGuard,
    round: usize,
) -> u64 {
    let seg_dir = root.join("seg");
    let start = std::time::Instant::now();
    let mut last_err: Option<String> = None;
    loop {
        for e in std::fs::read_dir(&seg_dir).unwrap().map(|e| e.unwrap()) {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some(SegmentState::Open.ext()) {
                continue;
            }
            // Non-strict, a torn tail is not an Err; an Err is a header not yet
            // written, corruption, or the file renamed away. Keep it for the
            // timeout message.
            match SegmentReader::read(&path, false) {
                Ok(r) => {
                    if r.frames.iter().any(|f| f.class == sot_log::Class::Producer) {
                        return r.frames.len() as u64;
                    }
                }
                Err(e) => last_err = Some(e.to_string()),
            }
        }
        if let Some(status) = capsule.child_mut().try_wait().unwrap() {
            panic!("round {round}: capsule exited ({status}) before writing producer output");
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!(
                "round {round}: no producer frame in an open segment within 30 s (last read error: {last_err:?})"
            );
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn kill9_sweep_recovers_green_every_round() {
    // ADR 0043 "Decisions for LU2" LU2b: the Linux `run` arm now binds a
    // real Unix socket transport unconditionally (`capsule/`'s one
    // unified writer loop, formerly a separate, wire-less Linux-only
    // loop) -- `bind` canonically validates the voyage id BEFORE any OS
    // call (ADR 0043 property 33), so it must be a real UUID now, not the
    // old mnemonic string; and `SOT_RUNTIME_DIR` must point at a private,
    // isolated dir the socket can actually bind under (never the default
    // discovery path, which a real supervisor -- absent here -- would
    // normally have exported). Sequential rounds, one capsule process
    // alive at a time, so ONE isolated dir for the whole sweep is safe.
    let voyage = uuid::Uuid::now_v7().to_string();
    // `tempdir_in("/tmp")`, never the default (ambient `$TMPDIR`) --
    // review round: a long ambient `TMPDIR` broke `sun_path`'s 108-byte
    // limit in the reviewer's own repro. Mirrors `tests/e2e_socket/`'s
    // and `tests/socket_unix/`'s identical device.
    let runtime_dir = tempfile::Builder::new().prefix("sot-fk").tempdir_in("/tmp").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(runtime_dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(&voyage);
    VoyageStore::bootstrap(&root, &voyage, RetentionClass::Discard).unwrap();

    let capsule_bin = env!("CARGO_BIN_EXE_sot-capsule");
    let mut sealed_before: (u64, u64) = (0, 0);

    for round in 0..ROUNDS {
        // A chatty producer that runs until its PTY dies or 60 s pass; the kill
        // is what ends it, and the cap means a test that dies before its kill
        // leaves no capsule writing.
        // `--assume-no-rollback-target`: this harness has no supervisor
        // and therefore no real rollout evidence to construct -- see
        // `sot-capsule run`'s own refusal message for what the flag
        // actually asserts. The old stdout-echo flag is gone entirely
        // (LU2b): wire fan-out replaced it, and this harness attaches no
        // wire client at all.
        let capsule = std::process::Command::new(capsule_bin)
            .args([
                "run",
                root.to_str().unwrap(),
                &voyage,
                "--assume-no-rollback-target",
                "--",
                "/bin/sh",
                "-c",
                "end=$(($(date +%s)+60)); i=0; while echo payload-line-$i; do i=$((i+1)); \
                 [ $((i % 1000)) -ne 0 ] || [ $(date +%s) -lt $end ] || break; done",
            ])
            .env("SOT_RUNTIME_DIR", runtime_dir.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sot-capsule");
        let mut capsule = capsule_guard::CapsuleGuard::new(capsule, &root);

        // Order, not timing: the kill comes after the first producer frame, so
        // every round adds output to the sealed history; the random delay then
        // picks the moment inside the stream.
        let seen = wait_first_output(&root, &mut capsule, round);
        std::thread::sleep(Duration::from_millis(delay_ms(round)));
        // SIGKILL: no drop handlers, no seal, no flush — the crash the
        // format exists to survive.
        unsafe {
            libc::kill(capsule.id() as i32, libc::SIGKILL);
        }
        let status = capsule.child_mut().wait().unwrap();
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "round {round}: capsule ended by {status}, not SIGKILL"
        );
        // Reap the orphaned producer too (its own session on a now-dead
        // PTY — it can block there indefinitely). The marker string is
        // unique to this test.
        let _ = std::process::Command::new("pkill")
            .args(["-9", "-f", "payload-line-"])
            .status();

        // Reopen = reconcile + recover under the writer lock. The next
        // incarnation must (a) come up, (b) seal the previous run's tip,
        // (c) leave the voyage verify-green with nothing sealed lost.
        let mut store = VoyageStore::open_for_writing(&root, &voyage)
            .unwrap_or_else(|e| panic!("round {round}: reopen after kill failed: {e}"));
        store.seal_survivor().unwrap_or_else(|e| {
            panic!("round {round}: survivor seal failed: {e}");
        });
        drop(store); // release the lock before verify + the next capsule

        verify_voyage(&root, &voyage)
            .unwrap_or_else(|e| panic!("round {round}: verify failed after recovery: {e}"));

        let sealed_now = count_sealed_frames(&root);
        // Everything `wait_first_output` saw, a producer frame among it, was in
        // the page cache before the kill, and recovery keeps the valid prefix: so
        // each round keeps at least `seen` frames and one producer frame.
        assert!(
            sealed_now.0 >= sealed_before.0 + seen,
            "round {round}: the {seen} frames seen before the kill did not all survive ({} -> {})",
            sealed_before.0,
            sealed_now.0
        );
        assert!(
            sealed_now.1 > sealed_before.1,
            "round {round}: no producer frame survived the kill ({} -> {})",
            sealed_before.1,
            sealed_now.1
        );
        sealed_before = sealed_now;
    }

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
