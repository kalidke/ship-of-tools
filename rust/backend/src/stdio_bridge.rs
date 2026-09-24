//! `sotd stdio-bridge --label <label>` — the last inch of a cross-host
//! dial. It resolves THIS box's own control endpoint for `label`
//! (`paths::session_socket_path`, whatever shape that endpoint has here —
//! Unix socket or Windows named pipe), connects with the same bounded
//! connector `LaneDial::Local` uses, and copies bytes both ways until
//! either side reaches EOF.
//!
//! Why a process and not a port forward: the endpoint's shape is the
//! owning box's own business. A caller elsewhere forwards a byte stream to
//! this process (an ssh channel, today) and never learns whether the last
//! inch was a socket or a pipe. That is also why there is no `--socket`
//! override here: the path is DERIVED on the box that owns it, never
//! carried across the wire.
//!
//! Three rules, each a correctness requirement rather than a style note:
//!
//! 1. **Nothing of its own on stdout, ever** — no greeting, no progress
//!    line, no trailing newline. The frame stream is newline-delimited
//!    (`codec::read_frame` reads to `\n`) and has no resynchronisation, so
//!    a single stray byte fails far from its cause. Diagnostics go to
//!    stderr, which the caller's journal keeps.
//! 2. **Byte-transparent** — fixed-size `read`/`write_all` of raw bytes,
//!    no line-oriented reads, no string conversion, no newline
//!    translation. A payload carrying both `\n` and `\r\n` survives
//!    unchanged (Rust's own stdio translates nothing on any platform; the
//!    caller must not run this under a pty, whose line discipline would).
//! 3. **Zero on a clean EOF; nonzero and exactly one stderr line naming
//!    the cause on any failure.** The line is the diagnosis — an absent
//!    endpoint, a refused connect, a copy that died — and the caller's
//!    journal is its only reader. There is deliberately no per-cause exit
//!    code: nothing branches on one, and a code nobody reads is a promise
//!    that has to stay true forever for nothing in return. The day
//!    something does branch, it gets a code of its own then.
//!
//! The platform split is the connect and nothing else: one `Bridged` type
//! per target, one `connect`, and a single copy loop over both.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use sot_log::transport::TransportError;

/// The one failure code. It says "this did not work" and nothing more —
/// the stderr line says which thing.
const EXIT_FAILED: i32 = 1;

/// One copy chunk. Big enough that a screenful of terminal output is one
/// write; small enough to stay on the stack in both directions.
const CHUNK: usize = 64 * 1024;

/// This box's own already-hardened client for its own endpoint — the same
/// two `sot-log` clients `LaneDial::Local` dials with, never a second
/// connector written here.
#[cfg(unix)]
type Bridged = sot_log::socket_unix::SocketClient;
#[cfg(windows)]
type Bridged = sot_log::pipe_win::PipeClient;

#[cfg(unix)]
fn connect(path: &Path) -> Result<Bridged, TransportError> {
    sot_log::socket_unix::connect_unix_socket_unchallenged(path)
}

#[cfg(windows)]
fn connect(path: &Path) -> Result<Bridged, TransportError> {
    let text = path.to_str().ok_or_else(|| TransportError::Io {
        op: "connect",
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "pipe path is not valid Unicode"),
    })?;
    // `connect_pipe_path_unchallenged`'s mid-dial cancel hook. Nothing
    // sets it here — the dial is the first thing this process does and no
    // other thread exists yet to cancel it.
    let dial_cancel = std::sync::atomic::AtomicBool::new(false);
    sot_log::pipe_win::connect_pipe_path_unchallenged(text, &dial_cancel)
}

pub fn run(args: &[String]) -> i32 {
    let label = match args {
        [flag, label] if flag == "--label" => label.clone(),
        _ => {
            eprintln!("Usage: sotd stdio-bridge --label <label>");
            return EXIT_FAILED;
        }
    };

    let path = crate::paths::session_socket_path(&label);
    let client = match connect(&path) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            // The whole diagnosis in one line: which endpoint, and what
            // it said — an absent one and a refusing one read differently
            // here without needing to read differently in `$?`. Both
            // connectors treat a missing endpoint as fatal on the first
            // attempt, so this is said at once rather than after a
            // connect bound.
            eprintln!("sotd stdio-bridge: {}: {e}", path.display());
            return EXIT_FAILED;
        }
    };

    // stdin → daemon, on its own thread, because both directions block.
    // `upstream` carries this thread's verdict to the one below: it is
    // stored BEFORE `cancel()`, and the loop below reads it only after
    // observing the `Cancelled` that only this `cancel()` can produce, so
    // the value is always the settled one.
    let upstream = Arc::new(AtomicI32::new(0));
    {
        let client = Arc::clone(&client);
        let upstream = Arc::clone(&upstream);
        std::thread::spawn(move || {
            let mut buf = [0u8; CHUNK];
            let mut stdin = std::io::stdin().lock();
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Err(e) = client.write_all(&buf[..n]) {
                            eprintln!("sotd stdio-bridge: writing to the daemon: {e}");
                            upstream.store(EXIT_FAILED, Ordering::SeqCst);
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        eprintln!("sotd stdio-bridge: reading stdin: {e}");
                        upstream.store(EXIT_FAILED, Ordering::SeqCst);
                        break;
                    }
                }
            }
            // Either end of stdin means the caller is gone: close the
            // daemon side at once rather than holding a row's lane open
            // behind a keepalive timeout.
            client.cancel();
        });
    }

    // daemon → stdout, on this thread, whose return ends the process (and
    // with it the thread above, still blocked in `read`).
    let mut buf = [0u8; CHUNK];
    let mut stdout = std::io::stdout().lock();
    loop {
        match client.read(&mut buf) {
            Ok(0) => return 0,
            Ok(n) => {
                // Flushed per chunk: a request/reply wire that buffers a
                // reply until the next write deadlocks both ends.
                if let Err(e) = stdout.write_all(&buf[..n]).and_then(|()| stdout.flush()) {
                    eprintln!("sotd stdio-bridge: writing to stdout: {e}");
                    return EXIT_FAILED;
                }
            }
            Err(TransportError::Cancelled) => return upstream.load(Ordering::SeqCst),
            Err(e) => {
                eprintln!("sotd stdio-bridge: reading from the daemon: {e}");
                return EXIT_FAILED;
            }
        }
    }
}
