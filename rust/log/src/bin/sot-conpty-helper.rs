//! `sot-conpty-helper [--child]`
//! `sot-conpty-helper --flood <total-bytes> [--linger]`
//! `sot-conpty-helper --script [repeats] [--linger]`
//!
//! Minimal helper binary for `conpty.rs`'s containment test (ADR 0041
//! §"Containment and the owned ConPTY layer") and `capsule_win.rs`'s
//! backpressure test (step 4). Prints its own PID as `pid:<n>\n`, then:
//!
//! - top-level invocation (no `--child`): spawns ITSELF again with
//!   `--child`, via a plain `std::process::Command` — no special job or
//!   console flags. That plainness IS the point: Windows' own default
//!   job-membership inheritance (no breakaway limit set on the containing
//!   job) is exactly what the containment test exercises — a grandchild
//!   spawned by an in-job process joins the SAME job automatically, with
//!   no code here asking for it. Relays the grandchild's PID line onto its
//!   own stdout, so a reader of only the TOP-LEVEL process's output (the
//!   `ConptySpawn` under test never sees the grandchild directly) learns
//!   both PIDs.
//! - `--child`: nothing further to print.
//!
//! Both levels then sleep for a long time rather than exiting: the
//! containment test proves job termination kills a LIVE tree, not that it
//! reaps one that already exited on its own. Readiness for the test is
//! "both `pid:` lines have been read from the pipe" — never a fixed sleep
//! and never `start /b`, which would detach the grandchild from normal
//! stdio inheritance instead of leaving it a plain, job-inheriting child.
//!
//! `--flood <total-bytes>` is the capsule backpressure test's producer: it
//! writes exactly `total-bytes` bytes of a cheap, position-independent
//! repeating pattern to stdout as fast as it can, then exits 0 — a NATURAL
//! exit, unlike the `--child` modes above, so a test can also observe the
//! capsule's own teardown draining the flood completely before sealing.
//! Content is deliberately not meaningful: the test verifies byte COUNT
//! (all of it survived the 8 MiB budget without loss or duplication), not
//! content.
//!
//! `--script [repeats]` (default 20) is ADR 0041 step 5's attach-fidelity
//! test's producer (finding 13: a deterministic byte-emitting helper, not
//! an interactive shell whose startup/rendering output the test cannot
//! predict or control). Writes a FIXED byte sequence — plain text, a CSI
//! color pair, an OSC (BEL-terminated) title, a 3-byte and a 4-byte UTF-8
//! codepoint, and a DCS (ST-terminated) payload — ONE BYTE AT A TIME with a
//! short sleep and an explicit flush between each, `repeats` times, then
//! exits 0 (a natural exit, so the run's own teardown is exercised too).
//! One-byte pacing does not GUARANTEE any particular ConPTY read boundary
//! (conhost's own buffering is opaque to a producer outside it) — it
//! removes every reason for conhost to coalesce ahead of bytes not yet
//! written, which is the most a producer can promise from outside; the
//! fidelity property under test must hold regardless of where the actual
//! cut falls, precisely because this helper does not engineer one.

#[cfg(windows)]
fn main() {
    use std::io::{BufRead, Write};

    if let Some(pos) = std::env::args().position(|a| a == "--flood") {
        let total: usize = std::env::args()
            .nth(pos + 1)
            .and_then(|s| s.parse().ok())
            .expect("--flood needs a byte count");
        flood(total);
        if std::env::args().any(|a| a == "--linger") {
            // Stay alive after the flood until externally terminated: a
            // test that asserts "the driver stays functional while the
            // producer runs" needs the producer to still be running when
            // its post-flood assertions execute — without this, the
            // producer's own exit races those assertions into teardown,
            // where producer-bound ops are (correctly) no longer served.
            std::thread::sleep(std::time::Duration::from_secs(600));
        }
        return;
    }

    if std::env::args().any(|a| a == "--script") {
        let pos = std::env::args().position(|a| a == "--script").unwrap();
        let repeats: usize = std::env::args().nth(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(20);
        script(repeats);
        if std::env::args().any(|a| a == "--linger") {
            // Same rule as --flood --linger: a test whose assertions
            // require a LIVE producer must not let the producer's own
            // emission speed decide whether that holds — 1000 script
            // repeats finish in milliseconds on a fast conhost, and a
            // producer exit mid-test moves the capsule into teardown,
            // where admission (correctly) is no longer served.
            std::thread::sleep(std::time::Duration::from_secs(600));
        }
        return;
    }

    let is_child = std::env::args().any(|a| a == "--child");
    println!("pid:{}", std::process::id());
    let _ = std::io::stdout().flush();

    if !is_child {
        let exe = std::env::current_exe().expect("current_exe");
        // Never waited on, by design: this whole process (and the
        // grandchild with it) is expected to be killed by the test's job
        // termination, not to exit gracefully — there is no code path here
        // that would ever reach a `.wait()` call. Windows has no zombie-
        // process table entry to leak either way: the OS reclaims an
        // exited process's resources once its last handle closes,
        // regardless of whether anything waited on it.
        #[allow(clippy::zombie_processes)]
        let mut grandchild = std::process::Command::new(exe)
            .arg("--child")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn grandchild");
        let stdout = grandchild.stdout.take().expect("grandchild stdout");
        let mut line = String::new();
        std::io::BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read grandchild pid line");
        print!("{line}");
        let _ = std::io::stdout().flush();
        // Deliberately not waited on here: the test kills the whole tree
        // via the job, never by waiting for either process to exit.
    }

    std::thread::sleep(std::time::Duration::from_secs(3600));
}

/// Write exactly `total` bytes to stdout in fixed-size chunks, flush once
/// at the end, then return (the caller exits 0 right after). `0..=9`
/// repeating: cheap to generate at high throughput, and a decoder can
/// verify total length alone without needing to know a chunk boundary.
#[cfg(windows)]
fn flood(total: usize) {
    use std::io::Write;
    const CHUNK: usize = 64 * 1024;
    let pattern: Vec<u8> = (0..CHUNK).map(|i| b'0' + (i % 10) as u8).collect();
    let mut stdout = std::io::stdout().lock();
    let mut written = 0usize;
    while written < total {
        let n = CHUNK.min(total - written);
        stdout.write_all(&pattern[..n]).expect("flood write");
        written += n;
    }
    let _ = stdout.flush();
}

/// The fixed byte sequence `--script` emits, repeated: plain text; a CSI
/// SGR pair (color on, "red", color off); an OSC title set, BEL-terminated;
/// a 3-byte UTF-8 codepoint (★ U+2605) immediately followed by a 4-byte one
/// (😀 U+1F600); a DCS payload, ST-terminated; a trailing newline.
/// Round-2 review deletion residue: this used to be `pub` with a doc
/// claiming the integration test imports it to build its own reference
/// byte stream -- no such consumer exists (a `src/bin/*.rs` binary has no
/// library target another crate file could `use` regardless), and the
/// test proves fidelity from what the CAPSULE actually recorded, not from
/// re-deriving an expected stream out-of-band. Private; this binary is its
/// only user. `#[cfg(windows)]`, matching `script()` (its only reader):
/// without it, a non-Windows build (this file's own `#[cfg(not(windows))]`
/// `main` never calls `script`) sees a private const nothing in that build
/// ever reads.
#[cfg(windows)]
const SCRIPT_BLOCK: &[u8] =
    b"plain text\x1b[31mred\x1b[0m\x1b]0;title\x07\xe2\x98\x85\xf0\x9f\x98\x80\x1bPdcs-payload\x1b\\done\n";

/// Writes [`SCRIPT_BLOCK`] `repeats` times, one byte at a time with a short
/// sleep and an explicit flush between each (see the module doc), then
/// returns (the caller exits 0 right after — a natural exit).
#[cfg(windows)]
fn script(repeats: usize) {
    use std::io::Write;
    use std::time::Duration;
    let mut stdout = std::io::stdout().lock();
    for _ in 0..repeats {
        for &b in SCRIPT_BLOCK {
            stdout.write_all(&[b]).expect("script write");
            let _ = stdout.flush();
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("sot-conpty-helper is Windows-only (ADR 0041 owned ConPTY layer)");
    std::process::exit(2);
}
