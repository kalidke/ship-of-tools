//! `sot-conpty-helper [--child]`
//! `sot-conpty-helper --flood <total-bytes>`
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

#[cfg(windows)]
fn main() {
    use std::io::{BufRead, Write};

    if let Some(pos) = std::env::args().position(|a| a == "--flood") {
        let total: usize = std::env::args()
            .nth(pos + 1)
            .and_then(|s| s.parse().ok())
            .expect("--flood needs a byte count");
        flood(total);
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

#[cfg(not(windows))]
fn main() {
    eprintln!("sot-conpty-helper is Windows-only (ADR 0041 owned ConPTY layer)");
    std::process::exit(2);
}
