//! `sotd stdio-bridge [--host <host> | --endpoint <unix:PATH|pipe:PATH>]` —
//! the last inch of a cross-host dial, and a byte shuttle to a local
//! endpoint for a caller on this box. With no argument it
//! resolves THIS box's own control endpoint, at the label this box resolves
//! for itself (`local_daemon_label()`), never one a caller names; `--host
//! <host>` resolves the hub's own relay socket for that host instead;
//! `--endpoint` takes the local endpoint a caller on this box already holds.
//! Every form connects through `connect_own` (whatever shape the endpoint has
//! here — Unix socket or Windows named pipe), and copies bytes both ways until
//! either side reaches EOF.
//!
//! Why a process and not a port forward: the endpoint's shape is the
//! owning box's own business. A caller elsewhere forwards a byte stream to
//! this process (an ssh channel, today) and never learns whether the last
//! inch was a socket or a pipe. That is also why the two forms a caller elsewhere
//! uses carry no path: the path is DERIVED on the box that owns it, never carried
//! across the wire. `--endpoint` is for a caller on this box that already holds a
//! local endpoint; the ssh forms never pass it.
//!
//! The no-argument and `--endpoint` forms read the caller's first line before they copy anything: a `link.hello`
//! makes the bridge the far end of a window's link (`sot_topology::link::router`), the connection already made
//! becoming its channel 0 and each channel the window opens one more connection to the same endpoint; any other
//! line goes to the endpoint unchanged and the copy starts as below. `--host` never routes: its endpoint is the hub's
//! relay socket, where each connection is a login.
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

use sot_platform::transport::TransportError;
use sot_topology as topology;

/// The usage line, printed on a bad argument and for `--help`.
pub(crate) const USAGE: &str = "Usage: sotd stdio-bridge [--host <host> | --endpoint <unix:PATH|pipe:PATH>]";
/// The one failure code. It says "this did not work" and nothing more —
/// the stderr line says which thing.
const EXIT_FAILED: i32 = 1;

/// One copy chunk. Big enough that a screenful of terminal output is one
/// write; small enough to stay on the stack in both directions.
const CHUNK: usize = 64 * 1024;

/// How long a routing form waits for the caller's first line: the daemon's own bound on a first frame
/// (`ADMISSION_READ_BOUND`).
const FIRST_LINE_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

/// This box's own already-hardened client for its own endpoint — the same
/// two `sot-capsule` clients `LaneDial::Local` dials with, never a second
/// connector written here. `connect_own` connects only to an endpoint this
/// OS account serves (ADR 0049, User isolation).
#[cfg(unix)]
type Bridged = sot_platform::transport::socket_unix::SocketClient;
#[cfg(windows)]
type Bridged = sot_platform::transport::pipe_win::PipeClient;

fn connect(path: &Path) -> Result<Bridged, TransportError> {
    sot_platform::transport::connect_own::connect_own(path)
}

/// The one local scheme on this platform: a Unix socket here, a named pipe on Windows.
const LOCAL_SCHEME: &str = if cfg!(windows) { "pipe:" } else { "unix:" };

/// The path a `--endpoint` value names: this platform's scheme and a nonempty path after it.
/// A bare name without the scheme, or the other platform's scheme, is refused; a Windows caller supplies
/// `pipe:\\.\pipe\<name>`.
fn local_endpoint(endpoint: &str) -> Option<std::path::PathBuf> {
    endpoint.strip_prefix(LOCAL_SCHEME).filter(|p| !p.is_empty()).map(std::path::PathBuf::from)
}

pub fn run(args: &[String]) -> i32 {
    // The forms whose endpoint is a local one on this box route a link; `--host`'s is a relay socket, a login per
    // connection.
    let routes = !matches!(args, [flag, _] if flag == "--host");
    let path = match args {
        // This box's own daemon, at the label it resolves for itself —
        // never a caller-supplied one. A `--label` argument was how a
        // Windows box came to dial a pipe name nothing listens on
        // (`local` vs `sot`); dropping it is what makes this obey the
        // rule stated at the top of this file.
        [] => crate::paths::session_socket_path(crate::paths::local_daemon_label()),
        // The hub's own relay socket for `<name>`, derived on the hub —
        // never carried over the wire.
        [flag, host] if flag == "--host" => {
            // The path is built from the name; a name outside the grammar builds none.
            if !topology::endpoint::is_plain_host_name(host) {
                eprintln!("sotd stdio-bridge: `{host}` is not a plain host name");
                return EXIT_FAILED;
            }
            sot_topology::relay_socket_path(host)
        }
        // A local endpoint a caller on this box already holds: the same connect below, so it reaches only
        // what this OS account serves (ADR 0049, User isolation).
        [flag, endpoint] if flag == "--endpoint" => match local_endpoint(endpoint) {
            Some(path) => path,
            None => {
                eprintln!("sotd stdio-bridge: {endpoint}: not a local endpoint here ({LOCAL_SCHEME}<path>)");
                return EXIT_FAILED;
            }
        },
        _ => {
            eprintln!("{USAGE}");
            return EXIT_FAILED;
        }
    };

    let client = match connect(&path) {
        Ok(c) => c,
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

    let first = if routes {
        match first_line() {
            Ok(line) => line,
            Err(why) => {
                eprintln!("sotd stdio-bridge: {why}");
                return EXIT_FAILED;
            }
        }
    } else {
        Vec::new()
    };
    let hello = first.strip_suffix(b"\n").and_then(|line| serde_json::from_slice::<sot_protocol::Frame>(line).ok());
    if let Some(hello) = hello.filter(sot_topology::link::router::is_link_hello) {
        return route(hello, client, path);
    }
    // Any other first line is the caller's first bytes for the daemon, unchanged.
    if !first.is_empty() {
        if let Err(e) = client.write_all(&first) {
            eprintln!("sotd stdio-bridge: writing to the daemon: {e}");
            return EXIT_FAILED;
        }
    }

    let client = Arc::new(client);

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

/// The caller's first line, newline included: at most `MAX_ENVELOPE_BYTES` and one more byte without one, or what came
/// before the end of input, within [`FIRST_LINE_BOUND`]. What follows it stays in stdin's own buffer, so the copy or
/// the router reads it next.
fn first_line() -> Result<Vec<u8>, String> {
    use std::io::BufRead;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut line = Vec::new();
        let read = loop {
            let chunk = match stdin.fill_buf() {
                Ok(chunk) => chunk,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => break Err(format!("reading stdin: {e}")),
            };
            if chunk.is_empty() {
                break Ok(());
            }
            let room = sot_protocol::codec::MAX_ENVELOPE_BYTES + 1 - line.len();
            let take = chunk.iter().position(|&b| b == b'\n').map_or(chunk.len(), |at| at + 1).min(room);
            line.extend_from_slice(&chunk[..take]);
            stdin.consume(take);
            if line.ends_with(b"\n") || line.len() > sot_protocol::codec::MAX_ENVELOPE_BYTES {
                break Ok(());
            }
        };
        let _ = tx.send(read.map(|()| line));
    });
    rx.recv_timeout(FIRST_LINE_BOUND).unwrap_or_else(|_| Err(format!("no first line within {FIRST_LINE_BOUND:?}")))
}

/// How long the router waits for a byte from the window before it ends the link. `SOT_TEST_LINK_SILENCE_MS` overrides
/// `SILENCE_BOUND` for tests, read once; unset in every real deployment.
fn silence_bound() -> std::time::Duration {
    std::env::var("SOT_TEST_LINK_SILENCE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map_or(sot_topology::link::SILENCE_BOUND, std::time::Duration::from_millis)
}

/// The far end of a window's link: `client`, the connection already made, is channel 0, and each channel the window
/// opens is one more connection to `path` through `connect_own`. Exits 0 when the window closed the link or channel 0
/// ended, else 1 with the reason on one stderr line.
fn route(hello: sot_protocol::Frame, client: Bridged, path: std::path::PathBuf) -> i32 {
    use sot_topology::link::router::{Connector, Duplex};
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("sotd stdio-bridge: the link's runtime: {e}");
            return EXIT_FAILED;
        }
    };
    let served = runtime.block_on(async move {
        let mut stdout = tokio::io::stdout();
        let control = match sot_topology::link::adopt(client) {
            Ok(control) => control,
            Err(e) => {
                let why = format!("{}: {e}", path.display());
                sot_topology::link::router::refuse_unreachable(&hello, &mut stdout, &why).await;
                return Err(why);
            }
        };
        let connect: Connector = std::sync::Arc::new(move || {
            let path = path.clone();
            Box::pin(async move { sot_topology::link::connect_local(&path).await.map(|s| Box::new(s) as Box<dyn Duplex>) })
        });
        sot_topology::link::router::serve_within(hello, tokio::io::stdin(), stdout, Box::new(control), connect, silence_bound()).await
    });
    // Tokio's stdin reads on a blocking thread no one can cancel, and dropping the runtime would wait for that read: a
    // link that ended while its window's input stayed open and silent would leave the router running. The runtime is
    // let go without waiting, and the process exits on return.
    runtime.shutdown_background();
    match served {
        Ok(()) => 0,
        Err(why) => {
            eprintln!("sotd stdio-bridge: the link ended: {why}");
            EXIT_FAILED
        }
    }
}
