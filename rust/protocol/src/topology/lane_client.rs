//! ADR 0045 decision 3: `DaemonLaneEndpoint`, the attach client's own
//! `sot_log::lane::client::Endpoint` for the lane bridge — a capsule row's
//! supervisor or voyage lane, piped through the row's OWN daemon
//! (`lane.connect`, `crate::ops::LANE_CONNECT`) instead of a loopback
//! named pipe or Unix socket the platform endpoints dial directly. Lives
//! in `sot-protocol`, not `sot-log`, because it is a WIRE CLIENT of this
//! crate's own `LaneConnectReq`/`LaneConnectRes` — `sot-log` has no
//! dependency the other way.
//!
//! # The split identity proof
//!
//! A `lane.connect` dial gets its OWN peer-identity report for free: the
//! daemon it asked ran steps 1-3 of the challenge on ITS dial and
//! returned the observed `(pid, created)` in [`crate::ops::
//! LaneConnectRes`]. [`DaemonLaneEndpoint::authenticate_server`] simply
//! hands that report back — no OS-level check of its own is possible
//! from here, reaching the peer only through a bridged pipe.
//! [`DaemonLaneEndpoint::challenge`] then runs `sot_log::identity::challenge::
//! exchange_identity` (steps 4-5, the SAME wire round trip every
//! platform endpoint's own `challenge()` runs) over that pipe and
//! accepts the result ONLY when it equals the daemon's own report.
//!
//! # One bounded, cancellable dial+handshake, one stream adapter
//!
//! [`LaneStream`] is the ONE adapter every transport (`Tcp`/`Unix`/
//! `Pipe`) goes through, implementing `sot_log::lane::client::Client`
//! directly — [`DaemonLaneClient`] is just `{stream: LaneStream, peer}`,
//! delegating every `Client` call straight to `stream`. `Unix` reuses
//! `sot_log::lane::socket_unix::SocketClient` and `Pipe` reuses `sot_log::
//! pipe_win::PipeClient` verbatim (both already bounded, cancellable
//! connectors with real `cancel()`s); `Tcp` gets a small local
//! [`TcpClient`] wrapper matching the same shape. [`DaemonLaneEndpoint::
//! dial`] then runs in two ABSOLUTE-deadline phases sharing this one
//! adapter: connect (2 s, each transport's own bounded connector — never
//! a blocking call an external deadline merely gives up ON without
//! actually stopping), then [`run_handshake`] (a SEPARATE 2 s bound
//! covering the whole write+read round trip as ONE operation, not a
//! per-read socket timeout a trickle of bytes could extend indefinitely
//! — `shutdown`/`cancel` on expiry, the same mechanism every transport's
//! own `Client::cancel` already provides).
//!
//! # Refusals and uncertainty are typed
//!
//! [`classify_reply`] never lets `lane.connect`'s wire outcome collapse
//! into a bare `io::Error`: a `Refused` is terminal, `Unreachable`/
//! `Undetermined` are retried by the caller (`sot_log::attach_client::client`'s
//! three connect sites), and only `lane_absent` decodes onto
//! `TransportError::Io` with a `NotFound`/`ConnectionRefused` kind — the
//! one case `is_endpoint_absent()` recognizes.
//!
//! # No process-control authority; no independent trust
//!
//! [`BridgedPeer`] implements ONLY `sot_log::lane::client::PeerIdentity` (bare
//! `pid`/`created`) — never `PeerProcess`: this endpoint cannot wait on
//! or terminate a process it only ever reaches through the daemon's own
//! pipe. And [`DaemonLaneEndpoint`] itself holds no kernel handle on
//! that process at all — see its own doc for the trust this implies.

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sot_log::identity::challenge::{ChallengeOutcome, PeerAuthOutcome, PeerAuthenticated, StatusFailure};
use sot_log::lane::client::{Client, Endpoint, PeerIdentity};
use sot_log::identity::exchange::IdentityExchange;
use sot_log::lane::transport::{TransportError, CONNECT_BOUND};

use crate::{op, Frame, Kind, LaneConnectReq, LaneConnectRes};

/// How to reach a row's daemon — a local Unix socket / Windows named
/// pipe (`Local`, matching the platform endpoints' own transport for
/// this host's daemon), an ssh child (`Ssh`, C3 as amended — the
/// transport for every OTHER host now that the daemon has no TCP
/// listener), or the loopback tunnel (`Tcp`, **kept** only as the dial
/// of `rust/backend/tests/lane_bridge.rs`'s hermetic harness — it opens
/// no port, since nothing listens on TCP anywhere in the daemon, so a
/// client-side address shape guards nothing; `lane_dial()`
/// (`rust/frontend/src/net/hosts.rs`) stops producing it, and it is a 0.6.7
/// deletion candidate once that harness is ported to `Local`). Carries
/// no row: the row rides `Endpoint`'s own `lane` argument, named exactly
/// once — never duplicated onto the dial value itself.
pub enum LaneDial {
    Tcp(SocketAddr),
    Local(PathBuf),
    /// The recipe and its host's link gate: a down gate makes the dial
    /// fail with `TransportError::LinkDown` and start no ssh.
    Ssh(crate::topology::ssh_bridge::SshRecipe, crate::topology::ssh_bridge::LinkGate),
}

/// An `Endpoint` value naming one daemon connection, never a row. `token`
/// mirrors `ProxyConnectReq::token` / `LaneConnectReq::token` — ignored by
/// the daemon.
///
/// **Trust limitation**: this endpoint holds NO kernel handle on the
/// lane's actual peer process and can run no OS-level identity check of
/// its own — every identity claim it ever makes traces back to the
/// DAEMON's own observation (`LaneConnectRes`'s `pid`/`created`, the
/// daemon's steps 1-3 on ITS dial), bound only by the wire `hello`
/// [`DaemonLaneEndpoint::challenge`] runs over the resulting pipe. A
/// daemon this client already trusts to control the row is the one
/// thing standing behind that report; nothing here re-verifies it
/// independently, by design (decision 3's split).
pub struct DaemonLaneEndpoint {
    pub dial: LaneDial,
    pub token: Option<String>,
}

/// The lane peer's identity, exactly as the DAEMON'S OWN dial observed
/// it — deliberately the same two fields as `sot_log::identity::challenge::
/// PeerAuthenticated`, but a separate type: nothing here is ever spelled
/// `ChallengedProcess` or `PeerAuthenticated`, so no consumer can mistake
/// a peer proven through a THIRD PARTY'S own OS-level check for one this
/// endpoint proved directly.
pub struct BridgedPeer {
    pub pid: u32,
    pub created: u64,
}

impl PeerIdentity for BridgedPeer {
    fn pid(&self) -> u32 {
        self.pid
    }
    fn created(&self) -> u64 {
        self.created
    }
}

/// The `Tcp` twin of `sot_log::lane::socket_unix::SocketClient`/`pipe_win::
/// PipeClient`: neither of those exists for a loopback TCP tunnel, so
/// this is the small adapter that gives `Tcp` the SAME shape — a
/// `cancelled` flag checked before AND interpreted after every I/O call
/// (a `shutdown` racing a blocked read/write can otherwise surface as a
/// generic `ConnectionAborted` instead of `Cancelled`), `cancel()` doing
/// `shutdown(Both)`.
///
/// `shutdown(Both)` alone is not the whole mechanism: on Winsock it does
/// NOT unblock a `recv` a peer thread already has parked (only closing
/// the socket does, and closing here would race that thread's own
/// borrowed `&TcpStream`) — Unix delivers the ordered EOF at once, but a
/// Windows reader would otherwise hang until the peer itself closes.
/// [`TcpClient::read`] is bounded instead: the same poll-a-cancel-flag-
/// between-bounded-waits shape `connect_pipe_path_unchallenged` already
/// uses for cancelling a dial in flight (B4a), applied here to the
/// blocking read every platform shares — one mechanism, not a per-OS
/// branch, and a no-op cost on Unix, where `shutdown` still wins the
/// race well inside one poll tick.
struct TcpClient {
    stream: TcpStream,
    cancelled: AtomicBool,
}

/// [`TcpClient::read`]'s poll granularity: small enough that `cancel()`'s
/// worst-case latency stays far inside every deadline a caller bounds a
/// read with (the lane handshake's own 2 s `CONNECT_BOUND`; this test
/// suite's `< 2s` assertion), large enough not to busy-spin the reader
/// thread while idle.
const READ_POLL_INTERVAL: Duration = Duration::from_millis(200);

impl TcpClient {
    /// Sets the read timeout once, at construction, rather than on every
    /// `read()` call — the poll-and-recheck loop is `read`'s concern, not
    /// a repeated syscall per byte.
    fn new(stream: TcpStream) -> Result<Self, TransportError> {
        stream
            .set_nodelay(true)
            .map_err(|source| TransportError::Io { op: "lane nodelay", source })?;
        stream
            .set_read_timeout(Some(READ_POLL_INTERVAL))
            .map_err(|source| TransportError::Io { op: "lane read", source })?;
        Ok(Self { stream, cancelled: AtomicBool::new(false) })
    }
}

impl Client for TcpClient {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        use std::io::Write;
        (&self.stream).write_all(bytes).map_err(|source| {
            if self.cancelled.load(Ordering::SeqCst) {
                TransportError::Cancelled
            } else {
                TransportError::Io { op: "lane write", source }
            }
        })
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        use std::io::Read;
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(TransportError::Cancelled);
            }
            match (&self.stream).read(buf) {
                Ok(n) => return Ok(n),
                // The poll tick expiring with nothing to read — not a
                // real failure, just another lap to re-check `cancelled`
                // (`WouldBlock`/`TimedOut`: which one a platform's own
                // `set_read_timeout` actually surfaces is not portably
                // specified, so both are treated identically here).
                Err(source) if matches!(source.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
                Err(source) => {
                    return Err(if self.cancelled.load(Ordering::SeqCst) {
                        TransportError::Cancelled
                    } else {
                        TransportError::Io { op: "lane read", source }
                    });
                }
            }
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        // Unblocks a Unix reader at once (ordered EOF); on Windows the
        // bounded poll loop in `read` above is what actually completes
        // the cancel — this call still matters there too, since it is
        // what the poll loop's own `cancelled` check observes.
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// The `Ssh` twin of `TcpClient`: a spawned `ssh … sotd stdio-bridge`
/// child whose stdin/stdout carry the lane bridge's own frames.
/// `ChildStdout`/`ChildStdin` are converted to `File` through `OwnedFd`
/// (unix) / `OwnedHandle` (windows) at construction, so `read`/
/// `write_all` go through `&self` exactly as `(&self.stream)` does for
/// `TcpClient` above.
///
/// One named difference from `TcpClient`: a pipe has no
/// `set_read_timeout`, so `READ_POLL_INTERVAL` has no analogue here.
/// `cancel()` sets the flag and **kills the child**; the kill closes the
/// child's stdout, which EOFs a parked read on both platforms — the
/// Winsock objection in `TcpClient`'s own doc ("closing here would race
/// that thread's own borrowed `&TcpStream`") does not apply, because the
/// handle closed is the CHILD's, not the stream the reader borrows.
/// `Drop` kills and waits, so no ssh child outlives its client.
///
/// No new trust claim: `DaemonLaneEndpoint`'s own doc already states
/// that it holds no kernel handle on the peer and that every identity
/// claim traces to the daemon's own observation. An ssh child is neither
/// better nor worse placed than a tcp socket on that point.
struct BridgedClient {
    child: std::sync::Mutex<std::process::Child>,
    out: std::fs::File,
    inp: std::fs::File,
    cancelled: AtomicBool,
    /// The child's last non-empty stderr line, kept by a drainer thread
    /// spawned at construction — ssh's own complaint ("Permission
    /// denied", or `unrecognised argument: --host` from a hub whose
    /// `sotd` predates C1) is the diagnosis a caller surfaces on
    /// failure, the same rule `stdio_bridge.rs` already sets for the far
    /// end.
    last_stderr: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl BridgedClient {
    fn spawn(recipe: &crate::topology::ssh_bridge::SshRecipe, gate: &crate::topology::ssh_bridge::LinkGate) -> Result<Self, TransportError> {
        #[allow(clippy::disallowed_methods, reason = "the lane dial's ssh, owned by the window's attach client")]
        let spawned = gate.spawn_sync(recipe);
        match spawned {
            Ok(child) => Self::wrap(child).map_err(TransportError::Unreachable),
            Err(crate::topology::ssh_bridge::SpawnError::LinkDown) => Err(TransportError::LinkDown),
            Err(crate::topology::ssh_bridge::SpawnError::Io(e)) => Err(TransportError::Unreachable(e)),
        }
    }

    /// The shared construction path — real `ssh` child ([`spawn`] above)
    /// or, in tests, any other piped-stdio child that stands in for one
    /// (so `cancel()`'s kill→EOF property is exercised without a real
    /// `ssh` on `PATH`).
    fn wrap(mut child: std::process::Child) -> std::io::Result<Self> {
        let stdin = child.stdin.take().expect("spawned with a piped stdin");
        let stdout = child.stdout.take().expect("spawned with a piped stdout");
        let stderr = child.stderr.take().expect("spawned with a piped stderr");

        let last_stderr = std::sync::Arc::new(std::sync::Mutex::new(None));
        {
            let last_stderr = std::sync::Arc::clone(&last_stderr);
            std::thread::spawn(move || {
                use std::io::BufRead;
                let reader = std::io::BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        if let Ok(mut guard) = last_stderr.lock() {
                            *guard = Some(line);
                        }
                    }
                }
            });
        }

        #[cfg(unix)]
        let (inp, out) = {
            use std::os::fd::OwnedFd;
            (std::fs::File::from(OwnedFd::from(stdin)), std::fs::File::from(OwnedFd::from(stdout)))
        };
        #[cfg(windows)]
        let (inp, out) = {
            use std::os::windows::io::OwnedHandle;
            (std::fs::File::from(OwnedHandle::from(stdin)), std::fs::File::from(OwnedHandle::from(stdout)))
        };

        Ok(Self { child: std::sync::Mutex::new(child), out, inp, cancelled: AtomicBool::new(false), last_stderr })
    }

    /// A short bounded poll for the child's last stderr line (this is the
    /// error path only, never the hot path). Stdout and stderr are
    /// separate pipes with no ordering guarantee between them, so a
    /// child that writes a diagnosis to stderr and closes stdout in the
    /// same instant can otherwise be observed here before its line
    /// lands.
    fn poll_last_stderr(&self) -> Option<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            if let Some(line) = self.last_stderr.lock().ok().and_then(|g| g.clone()) {
                return Some(line);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Wraps a raw io error with the child's last stderr line when one
    /// was captured — that line IS the diagnosis (a dead child's own
    /// "Permission denied" beats the generic "broken pipe" its closed
    /// pipe leaves behind).
    fn diagnose(&self, source: std::io::Error) -> std::io::Error {
        match self.poll_last_stderr() {
            Some(line) => std::io::Error::other(line),
            None => source,
        }
    }
}

impl Client for BridgedClient {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        use std::io::Write;
        (&self.inp).write_all(bytes).map_err(|source| {
            if self.cancelled.load(Ordering::SeqCst) {
                TransportError::Cancelled
            } else {
                TransportError::Io { op: "lane write", source: self.diagnose(source) }
            }
        })
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        use std::io::Read;
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        (&self.out).read(buf).map_err(|source| {
            if self.cancelled.load(Ordering::SeqCst) {
                TransportError::Cancelled
            } else {
                TransportError::Io { op: "lane read", source: self.diagnose(source) }
            }
        })
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

impl Drop for BridgedClient {
    fn drop(&mut self) {
        self.cancel();
        if let Ok(mut child) = self.child.lock() {
            let _ = child.wait();
        }
    }
}

/// The ONE stream adapter every transport this endpoint dials goes
/// through — `Unix`/`Pipe` reuse `sot-log`'s own hardened clients
/// verbatim (real bounded connectors, real `cancel()`s) rather than
/// reimplementing either; `Tcp`/`Bridged` each needed a wrapper of their
/// own (`sot-log` has no loopback-TCP client, and no ssh-child client at
/// all). `DaemonLaneClient` below is nothing more than
/// `{stream: LaneStream, peer}` — every `Client` call delegates straight
/// through.
enum LaneStream {
    Tcp(TcpClient),
    #[cfg(unix)]
    Unix(sot_log::lane::socket_unix::SocketClient),
    #[cfg(windows)]
    Pipe(sot_log::lane::pipe_win::PipeClient),
    Bridged(BridgedClient),
}

impl Client for LaneStream {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        match self {
            LaneStream::Tcp(c) => c.write_all(bytes),
            #[cfg(unix)]
            LaneStream::Unix(c) => c.write_all(bytes),
            #[cfg(windows)]
            LaneStream::Pipe(c) => c.write_all(bytes),
            LaneStream::Bridged(c) => c.write_all(bytes),
        }
    }
    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        match self {
            LaneStream::Tcp(c) => c.read(buf),
            #[cfg(unix)]
            LaneStream::Unix(c) => c.read(buf),
            #[cfg(windows)]
            LaneStream::Pipe(c) => c.read(buf),
            LaneStream::Bridged(c) => c.read(buf),
        }
    }
    fn cancel(&self) {
        match self {
            LaneStream::Tcp(c) => c.cancel(),
            #[cfg(unix)]
            LaneStream::Unix(c) => c.cancel(),
            #[cfg(windows)]
            LaneStream::Pipe(c) => c.cancel(),
            LaneStream::Bridged(c) => c.cancel(),
        }
    }
}

/// The connected, identity-reported lane pipe a `lane.connect` dial
/// produced. `stream`/`peer` are private — a caller drives this only
/// through the `Client`/`Endpoint` trait vocabulary.
pub struct DaemonLaneClient {
    stream: LaneStream,
    peer: PeerAuthenticated,
}

impl Client for DaemonLaneClient {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        self.stream.write_all(bytes)
    }
    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        self.stream.read(buf)
    }
    fn cancel(&self) {
        self.stream.cancel()
    }
}

impl Endpoint for DaemonLaneEndpoint {
    type Client = DaemonLaneClient;
    type Process = BridgedPeer;

    /// `lane` is the row's `session_name` name — `LaneConnectReq::target`
    /// is required for the voyage lane too (a voyage is reached only
    /// through the row that owns it), so no daemon-side change was
    /// needed here: B3 already requires `target` on both lane kinds.
    fn connect_voyage_unchallenged(&self, lane: &str, voyage_id: &str) -> Result<Self::Client, TransportError> {
        self.dial(lane, "voyage", Some(voyage_id.to_string()))
    }

    fn connect_supervisor_unchallenged(&self, lane: &str) -> Result<Self::Client, TransportError> {
        self.dial(lane, "supervisor", None)
    }

    /// Steps 4-5 ONLY, over the already-piped connection — the SAME
    /// `exchange_identity` every platform endpoint's own `challenge()`
    /// runs, bound here against the daemon's own report (`conn.peer`)
    /// rather than a fresh OS-level check this endpoint has no way to
    /// run.
    fn challenge(&self, conn: &Self::Client, exchange: &mut dyn IdentityExchange, reply_deadline: Instant) -> ChallengeOutcome<Self::Process> {
        match sot_log::identity::challenge::exchange_identity(conn, exchange, reply_deadline) {
            Some(Ok((pid, created))) if pid == conn.peer.pid && created == conn.peer.created => {
                ChallengeOutcome::Proven(BridgedPeer { pid, created })
            }
            // A well-formed reply whose OWN pid/creation disagrees with
            // what the daemon reported is exactly as unproven as a
            // `Foreign` `StatusFailure` — the daemon's report is the
            // only authority this endpoint has, and the wire just
            // contradicted it.
            Some(Ok(_)) => ChallengeOutcome::Foreign,
            Some(Err(StatusFailure::Foreign)) => ChallengeOutcome::Foreign,
            Some(Err(StatusFailure::Undetermined)) => ChallengeOutcome::Undetermined,
            None => ChallengeOutcome::Undetermined,
        }
    }

    /// No wire I/O of its own: the daemon already ran steps 1-3 on its
    /// own dial before this client ever existed, so this simply hands
    /// that report back.
    fn authenticate_server(&self, conn: &Self::Client) -> PeerAuthOutcome {
        PeerAuthOutcome::Authenticated(conn.peer)
    }

    /// The host's link gate for an ssh dial; every other dial is local.
    fn link_up(&self) -> bool {
        match &self.dial {
            LaneDial::Ssh(_, gate) => gate.is_up(),
            _ => true,
        }
    }
}

/// The standard error payload `crate::ops::LaneConnectReq`'s own doc
/// promises on refusal, `{error, code}` (plus `kind` for `lane_absent`).
/// `code` is optional at the TYPE level only to catch a daemon that
/// predates the lane bridge entirely: an old daemon's "unknown op"
/// answer is `{"error": "unknown op: lane.connect"}` with no `code`.
#[derive(serde::Deserialize)]
struct WireError {
    error: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

/// The `io::ErrorKind` `Debug` text `lane_bridge::absent_kind` writes,
/// decoded back — the only two kinds `lane_absent` ever actually sends
/// (`TransportError::is_endpoint_absent`'s own predicate), so anything
/// else falls back to `Other`.
fn absent_kind_from_wire(s: Option<&str>) -> std::io::ErrorKind {
    match s {
        Some("NotFound") => std::io::ErrorKind::NotFound,
        Some("ConnectionRefused") => std::io::ErrorKind::ConnectionRefused,
        _ => std::io::ErrorKind::Other,
    }
}

/// `true` iff a wire `unauthenticated` refusal is an OLD daemon's ordinary
/// control-loop auth gate (its "... send a token-valid hello first" text)
/// answering a `lane.connect` it never recognized as a first-frame op. No
/// daemon in this tree sends `unauthenticated`. There is no wire `code`
/// for "predates the bridge", so the daemon's own message text is the only
/// thing that marks the old gate.
fn unauthenticated_is_actually_no_bridge(detail: &str) -> bool {
    detail.contains("token-valid hello")
}

/// Classify one `lane.connect` reply frame into the daemon's own
/// `(pid, created)` report, or a typed refusal/uncertainty. Matched
/// BEFORE any `io::Error` conversion exists to unwrap these into.
fn classify_reply(frame: Frame) -> Result<(u32, u64), TransportError> {
    if frame.kind != Kind::Res || frame.op != op::LANE_CONNECT {
        return Err(TransportError::Refused {
            code: "no_bridge".to_string(),
            detail: format!(
                "unexpected reply (op={:?} kind={:?}) — a daemon that predates the lane bridge (ADR 0045), or a foreign wire protocol",
                frame.op, frame.kind
            ),
        });
    }
    if let Ok(res) = serde_json::from_value::<LaneConnectRes>(frame.payload.clone()) {
        if res.ok {
            return Ok((res.pid, res.created));
        }
    }
    let werr: WireError = match serde_json::from_value(frame.payload) {
        Ok(w) => w,
        Err(_) => {
            return Err(TransportError::Refused {
                code: "no_bridge".to_string(),
                detail: "the daemon's lane.connect reply carried no recognizable result — it predates the lane bridge (ADR 0045)".to_string(),
            });
        }
    };
    match werr.code.as_deref() {
        Some("lane_absent") => Err(TransportError::Io {
            op: "lane.connect",
            source: std::io::Error::new(absent_kind_from_wire(werr.kind.as_deref()), werr.error),
        }),
        // The daemon's OWN dial/authenticate step failed on the far side
        // (any I/O error but absence) — uncertain, not a confirmed dead
        // row, so this retries and clears the health clock exactly like
        // `Unreachable`, never a generic `Io` a caller could charge to
        // the absence window.
        Some("dial_failed") => Err(TransportError::Unreachable(std::io::Error::other(werr.error))),
        Some("undetermined") => Err(TransportError::Undetermined { via: "bridge", detail: werr.error }),
        Some("unauthenticated") if unauthenticated_is_actually_no_bridge(&werr.error) => Err(TransportError::Refused {
            code: "no_bridge".to_string(),
            detail: format!("a daemon that predates the lane bridge (ADR 0045) refused with its ordinary control-loop gate: {}", werr.error),
        }),
        Some(code) => Err(TransportError::Refused { code: code.to_string(), detail: werr.error }),
        None => Err(TransportError::Refused { code: "no_bridge".to_string(), detail: werr.error }),
    }
}

/// `PipeClient`/`SocketClient`/`TcpClient`'s `write_all`/`read` are
/// `&self` methods returning `Result<_, TransportError>`
/// (`sot_log::lane::client::Client`'s own shape), not `std::io::{Read,
/// Write}` — this is the ONE adapter that lets `write_frame_blocking`/
/// `read_frame_blocking` drive ANY of them, so every transport speaks
/// byte-identical framing rather than a per-transport reimplementation.
struct ClientIo<'a>(&'a dyn Client);

impl<'a> std::io::Read for ClientIo<'a> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf).map_err(|e| std::io::Error::other(e.to_string()))
    }
}

impl<'a> std::io::Write for ClientIo<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write_all(buf).map(|()| buf.len()).map_err(|e| std::io::Error::other(e.to_string()))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The ONE deadline helper the handshake shares across all three
/// transports: write the request, read ONE reply, as a SINGLE operation
/// bounded by one absolute deadline — not a per-read socket timeout a
/// trickle of bytes could extend indefinitely, and the write is bounded
/// by the SAME deadline too (a slow/stalled write is no less a hang than
/// a slow read). `on_timeout` is `stream.cancel()` — `shutdown(Both)` for
/// `Tcp`/`Unix`, `PipeClient`'s own OVERLAPPED cancel for `Pipe` — the
/// SAME mechanism `exchange_identity`'s own wire round trip already uses
/// for the POST-handshake attach hello, so the handshake and the hello
/// that immediately follows it are bounded identically.
fn run_handshake(stream: &LaneStream, req: &Frame, deadline: Instant) -> Result<(u32, u64), TransportError> {
    let outcome = sot_log::identity::deadline::run_with_deadline(deadline, || stream.cancel(), || -> Result<Frame, TransportError> {
        let mut io = ClientIo(stream);
        crate::codec::write_frame_blocking(&mut io, req).map_err(|e| TransportError::Unreachable(std::io::Error::other(e.to_string())))?;
        let mut r = std::io::BufReader::new(ClientIo(stream));
        crate::codec::read_frame_blocking(&mut r).map_err(|e| TransportError::Unreachable(std::io::Error::other(e.to_string())))
    });
    match outcome {
        Some(Ok(frame)) => classify_reply(frame),
        Some(Err(e)) => Err(e),
        None => Err(TransportError::Unreachable(std::io::Error::new(std::io::ErrorKind::TimedOut, "lane.connect: handshake timed out"))),
    }
}

impl DaemonLaneEndpoint {
    /// The blocking dial: connect (2 s bound, each transport's own
    /// hardened connector — see [`LaneStream`]'s own doc), write the
    /// `lane.connect` request and read ONE reply under a SEPARATE 2 s
    /// bound ([`run_handshake`]), then classify it.  `row` is the row's
    /// `session_name` name (`LaneConnectReq::target`, required for both
    /// lane kinds); `kind` is `"supervisor"` or `"voyage"`
    /// (`LaneConnectReq::lane`).
    fn dial(&self, row: &str, kind: &str, voyage_id: Option<String>) -> Result<DaemonLaneClient, TransportError> {
        let req = LaneConnectReq {
            target: row.to_string(),
            lane: kind.to_string(),
            voyage_id,
            token: self.token.clone(),
        };
        let frame = Frame::req(1, op::LANE_CONNECT, serde_json::to_value(&req).expect("LaneConnectReq always serializes"));

        let stream = match &self.dial {
            LaneDial::Tcp(addr) => {
                let stream = TcpStream::connect_timeout(addr, CONNECT_BOUND).map_err(TransportError::Unreachable)?;
                LaneStream::Tcp(TcpClient::new(stream)?)
            }
            #[cfg(unix)]
            LaneDial::Local(path) => {
                // `connect_own`: the folder rule (ADR 0049, User
                // isolation), then `sot_log::lane::socket_unix`'s own
                // bounded, non-blocking connector rather than a blocking
                // `UnixStream::connect` under an external deadline: the
                // latter would leak the blocked connect thread past the
                // deadline on a full listen backlog instead of actually
                // stopping — this connector never blocks past
                // `CONNECT_BOUND` in the first place.
                let client = sot_log::identity::connect_own::connect_own(path).map_err(|te| TransportError::Unreachable(unwrap_connect_io(te)))?;
                LaneStream::Unix(client)
            }
            #[cfg(windows)]
            LaneDial::Local(path) => {
                // `connect_own`: `sot_log::lane::pipe_win`'s bounded pipe
                // connector, then the check that this OS account serves the
                // pipe (ADR 0049, User isolation), before the first byte.
                let client = sot_log::identity::connect_own::connect_own(path).map_err(|te| TransportError::Unreachable(unwrap_connect_io(te)))?;
                LaneStream::Pipe(client)
            }
            LaneDial::Ssh(recipe, gate) => {
                let client = BridgedClient::spawn(recipe, gate)?;
                LaneStream::Bridged(client)
            }
        };

        let handshake_deadline = Instant::now() + CONNECT_BOUND;
        let outcome = run_handshake(&stream, &frame, handshake_deadline);
        // A dying ssh child's stdout closes as a clean `Ok(0)` EOF, not
        // an `io::Error` `BridgedClient::read` has anything to wrap — the
        // codec layer above it turns that EOF into its own generic
        // parse-failure text before `Client::read`'s error path (the
        // `diagnose` this same struct otherwise gives `write_all`/`read`)
        // ever gets a look. This is the one place both paths funnel
        // through, so it is where the substitution has to happen for the
        // EOF case: on ANY handshake failure over a `Bridged` stream,
        // prefer the child's last stderr line over whatever codec text
        // resulted, matching `write_all`/`read`'s existing rule.
        if outcome.is_err() {
            if let LaneStream::Bridged(bridged) = &stream {
                if let Some(line) = bridged.poll_last_stderr() {
                    return Err(TransportError::Unreachable(std::io::Error::other(line)));
                }
            }
        }
        let (pid, created) = outcome?;
        Ok(DaemonLaneClient { stream, peer: PeerAuthenticated { pid, created } })
    }
}

/// `connect_own`'s failures (a connector's own, or the rule's refusal) are
/// always [`TransportError::Io`] — this unwraps that
/// (preserving the underlying `io::Error`) so [`DaemonLaneEndpoint::
/// dial`] can rewrap it as [`TransportError::Unreachable`] uniformly;
/// any other variant (unreachable in practice — neither connector
/// nor the rule produces one) still degrades to a generic io error rather than
/// panicking.
#[cfg(any(unix, windows))]
fn unwrap_connect_io(e: TransportError) -> std::io::Error {
    match e {
        TransportError::Io { source, .. } => source,
        other => std::io::Error::other(other.to_string()),
    }
}

#[cfg(test)]
#[path = "lane_client_tests.rs"]
mod tests;
