//! `sot-pty-helper [--child]`
//! `sot-pty-helper --flood <total-bytes> [--linger]`
//! `sot-pty-helper --script [repeats] [--linger]`
//! `sot-pty-helper --script [repeats] --drip`
//!
//! Unix twin of `sot-conpty-helper.rs` (ADR 0043 "Decisions for LU2"
//! LU2b) — same modes, same exit codes, the exact same `SCRIPT_BLOCK`
//! bytes (shared via `support/helper_common.rs`, `#[path]`-included by
//! both) — see that file's own module doc for what each mode proves.
//! Nothing about the byte-emitting behavior differs on Unix; only the
//! top-level bare/`--child` mode's own containment mechanism does (a
//! Unix process GROUP, via `producer_pty.rs`'s own `setsid`, rather than
//! a Windows job object) — this binary itself needs no special call for
//! that: a plain `Command` grandchild simply inherits the SAME process
//! group its parent already has, by default.

#[path = "support/helper_common.rs"]
mod helper_common;

#[cfg(unix)]
fn main() {
    use helper_common::{flood_pattern, SCRIPT_BLOCK};
    use std::io::{BufRead, Write};

    if let Some(pos) = std::env::args().position(|a| a == "--flood") {
        let total: usize = std::env::args()
            .nth(pos + 1)
            .and_then(|s| s.parse().ok())
            .expect("--flood needs a byte count");
        flood(total, &flood_pattern(64 * 1024));
        if std::env::args().any(|a| a == "--linger") {
            // Stay alive after the flood until externally terminated --
            // see `sot-conpty-helper.rs`'s identical doc on its own
            // `--flood --linger` for why.
            std::thread::sleep(std::time::Duration::from_secs(600));
        }
        return;
    }

    if std::env::args().any(|a| a == "--script") {
        let pos = std::env::args().position(|a| a == "--script").unwrap();
        let repeats: usize = std::env::args().nth(pos + 1).and_then(|s| s.parse().ok()).unwrap_or(20);
        script(repeats, SCRIPT_BLOCK);
        if std::env::args().any(|a| a == "--drip") {
            // Never returns -- see `drip`'s own doc and
            // `sot-conpty-helper.rs`'s identical one.
            drip();
        }
        if std::env::args().any(|a| a == "--linger") {
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
        // grandchild with it) is expected to be killed by the test's
        // process-GROUP kill, not to exit gracefully -- see
        // `sot-conpty-helper.rs`'s identical doc for the Windows-job
        // analogue of this same reasoning.
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
        // Deliberately not waited on here: the test kills the whole
        // process group, never by waiting for either process to exit.
    }

    std::thread::sleep(std::time::Duration::from_secs(3600));
}

/// Write exactly `total` bytes to stdout in `pattern`-sized chunks, flush
/// once at the end, then return (the caller exits 0 right after).
#[cfg(unix)]
fn flood(total: usize, pattern: &[u8]) {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    let mut written = 0usize;
    while written < total {
        let n = pattern.len().min(total - written);
        stdout.write_all(&pattern[..n]).expect("flood write");
        written += n;
    }
    let _ = stdout.flush();
}

/// Writes `block` `repeats` times, one byte at a time with a short sleep
/// and an explicit flush between each (see the module doc), then returns
/// (the caller exits 0 right after — a natural exit).
#[cfg(unix)]
fn script(repeats: usize, block: &[u8]) {
    use std::io::Write;
    use std::time::Duration;
    let mut stdout = std::io::stdout().lock();
    for _ in 0..repeats {
        for &b in block {
            stdout.write_all(&[b]).expect("script write");
            let _ = stdout.flush();
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// See `sot-conpty-helper.rs`'s identical `drip` doc (the module doc's
/// `--drip` section): one short, cheap, position-independent plain-text
/// line every ~200 ms, forever.
#[cfg(unix)]
fn drip() -> ! {
    use std::io::Write;
    use std::time::Duration;
    let mut stdout = std::io::stdout().lock();
    let mut n: u64 = 0;
    loop {
        n += 1;
        let _ = writeln!(stdout, "drip {n}");
        let _ = stdout.flush();
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("sot-pty-helper is Unix-only (ADR 0043 \"Decisions for LU2\")");
    std::process::exit(2);
}
