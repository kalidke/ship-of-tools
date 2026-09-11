//! ADR 0045 decision 3: `DaemonLaneEndpoint`, the attach client's own
//! `sot_log::client::Endpoint` for the lane bridge — a capsule row's
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
//! [`DaemonLaneEndpoint::challenge`] then runs `sot_log::challenge::
//! exchange_identity` (steps 4-5, the SAME wire round trip every
//! platform endpoint's own `challenge()` runs) over that pipe and
//! accepts the result ONLY when it equals the daemon's own report.
//!
//! # One bounded, cancellable dial+handshake, one stream adapter
//!
//! [`LaneStream`] is the ONE adapter every transport (`Tcp`/`Unix`/
//! `Pipe`) goes through, implementing `sot_log::client::Client`
//! directly — [`DaemonLaneClient`] is just `{stream: LaneStream, peer}`,
//! delegating every `Client` call straight to `stream`. `Unix` reuses
//! `sot_log::socket_unix::SocketClient` and `Pipe` reuses `sot_log::
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
//! `Undetermined` are retried by the caller (`sot_log::fe_client_io`'s
//! three connect sites), and only `lane_absent` decodes onto
//! `TransportError::Io` with a `NotFound`/`ConnectionRefused` kind — the
//! one case `is_endpoint_absent()` recognizes.
//!
//! # No process-control authority; no independent trust
//!
//! [`BridgedPeer`] implements ONLY `sot_log::client::PeerIdentity` (bare
//! `pid`/`created`) — never `PeerProcess`: this endpoint cannot wait on
//! or terminate a process it only ever reaches through the daemon's own
//! pipe. And [`DaemonLaneEndpoint`] itself holds no kernel handle on
//! that process at all — see its own doc for the trust this implies.

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sot_log::challenge::{ChallengeOutcome, PeerAuthOutcome, PeerAuthenticated, StatusFailure};
use sot_log::client::{Client, Endpoint, PeerIdentity};
use sot_log::exchange::IdentityExchange;
use sot_log::transport::{TransportError, CONNECT_BOUND};

use crate::{op, Frame, Kind, LaneConnectReq, LaneConnectRes};

/// How to reach a row's daemon — the loopback tunnel (`Tcp`, matching
/// `proxy.connect`'s own transport for a remote host) or a local Unix
/// socket / Windows named pipe (`Local`, matching the platform
/// endpoints' own transport for this host's daemon). Carries no row: the
/// row rides `Endpoint`'s own `lane` argument, named exactly once —
/// never duplicated onto the dial value itself.
pub enum LaneDial {
    Tcp(SocketAddr),
    Local(PathBuf),
}

/// An `Endpoint` value naming one daemon connection, never a row. `token`
/// mirrors `ProxyConnectReq::token` / `LaneConnectReq::token` — present
/// only on a token-configured daemon.
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
/// it — deliberately the same two fields as `sot_log::challenge::
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

/// The `Tcp` twin of `sot_log::socket_unix::SocketClient`/`pipe_win::
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

/// The ONE stream adapter every transport this endpoint dials goes
/// through — `Unix`/`Pipe` reuse `sot-log`'s own hardened clients
/// verbatim (real bounded connectors, real `cancel()`s) rather than
/// reimplementing either; only `Tcp` needed a wrapper of its own
/// (`sot-log` has no loopback-TCP client). `DaemonLaneClient` below is
/// nothing more than `{stream: LaneStream, peer}` — every `Client` call
/// delegates straight through.
enum LaneStream {
    Tcp(TcpClient),
    #[cfg(unix)]
    Unix(sot_log::socket_unix::SocketClient),
    #[cfg(windows)]
    Pipe(sot_log::pipe_win::PipeClient),
}

impl Client for LaneStream {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        match self {
            LaneStream::Tcp(c) => c.write_all(bytes),
            #[cfg(unix)]
            LaneStream::Unix(c) => c.write_all(bytes),
            #[cfg(windows)]
            LaneStream::Pipe(c) => c.write_all(bytes),
        }
    }
    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        match self {
            LaneStream::Tcp(c) => c.read(buf),
            #[cfg(unix)]
            LaneStream::Unix(c) => c.read(buf),
            #[cfg(windows)]
            LaneStream::Pipe(c) => c.read(buf),
        }
    }
    fn cancel(&self) {
        match self {
            LaneStream::Tcp(c) => c.cancel(),
            #[cfg(unix)]
            LaneStream::Unix(c) => c.cancel(),
            #[cfg(windows)]
            LaneStream::Pipe(c) => c.cancel(),
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

    /// `lane` is the row's `tmux_session` name — `LaneConnectReq::target`
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
        match sot_log::challenge::exchange_identity(conn, exchange, reply_deadline) {
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

/// `true` iff a wire `unauthenticated` refusal is actually an OLD
/// daemon's ordinary control-loop auth gate (`server.rs`'s exact "...
/// send a token-valid hello first" text) answering a `lane.connect` it
/// never recognized as a first-frame op — rather than the BRIDGE's own
/// token check (`lane_bridge.rs`'s "bad or missing token", a daemon
/// that DOES speak `lane.connect` but rejected THIS dial's `token`).
/// There is no wire `code` for "predates the bridge" — `unauthenticated`
/// is genuinely shared between the two cases — so the daemon's own
/// message text is the only thing that tells them apart.
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
/// (`sot_log::client::Client`'s own shape), not `std::io::{Read,
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
    let outcome = sot_log::deadline::run_with_deadline(deadline, || stream.cancel(), || -> Result<Frame, TransportError> {
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
    /// `tmux_session` name (`LaneConnectReq::target`, required for both
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
                // Reuses `sot_log::socket_unix`'s own bounded, non-
                // blocking connector rather than a blocking
                // `UnixStream::connect` under an external deadline: the
                // latter would leak the blocked connect thread past the
                // deadline on a full listen backlog instead of actually
                // stopping — this connector never blocks past
                // `CONNECT_BOUND` in the first place.
                let client = sot_log::socket_unix::connect_unix_socket_unchallenged(path).map_err(|te| TransportError::Unreachable(unwrap_connect_io(te)))?;
                LaneStream::Unix(client)
            }
            #[cfg(windows)]
            LaneDial::Local(path) => {
                let path_str = path.to_str().ok_or_else(|| {
                    TransportError::Unreachable(std::io::Error::new(std::io::ErrorKind::InvalidInput, "lane pipe path is not valid Unicode"))
                })?;
                // A fresh, per-dial cancel flag: `connect_pipe_path_
                // unchallenged`'s own bounded poll loop checks it between
                // every already-bounded `WaitNamedPipeW` wait — the only
                // mid-dial cancellation a synchronous `CreateFileW`/
                // `WaitNamedPipeW` pair admits (neither has an OS-level
                // cancellation handle the way an OVERLAPPED read/write on
                // an already-open handle does). Nothing external sets it
                // today (this dial has no caller that cancels one in
                // flight yet) — the hook exists so one can.
                let dial_cancel = AtomicBool::new(false);
                let client = sot_log::pipe_win::connect_pipe_path_unchallenged(path_str, &dial_cancel).map_err(|te| TransportError::Unreachable(unwrap_connect_io(te)))?;
                LaneStream::Pipe(client)
            }
        };

        let handshake_deadline = Instant::now() + CONNECT_BOUND;
        let (pid, created) = run_handshake(&stream, &frame, handshake_deadline)?;
        Ok(DaemonLaneClient { stream, peer: PeerAuthenticated { pid, created } })
    }
}

/// `connect_unix_socket_unchallenged`/`connect_pipe_path_unchallenged`'s
/// own failures are always [`TransportError::Io`] — this unwraps that
/// (preserving the underlying `io::Error`) so [`DaemonLaneEndpoint::
/// dial`] can rewrap it as [`TransportError::Unreachable`] uniformly;
/// any other variant (unreachable in practice — neither connector
/// produces one) still degrades to a generic io error rather than
/// panicking.
#[cfg(any(unix, windows))]
fn unwrap_connect_io(e: TransportError) -> std::io::Error {
    match e {
        TransportError::Io { source, .. } => source,
        other => std::io::Error::other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A `std::net::TcpListener` on loopback plays the daemon — portable,
    /// no daemon process needed — for every refusal/uncertainty case
    /// `dial` must classify (ADR 0045 decision 4).
    fn dial_against<F>(respond: F) -> Result<(u32, u64), TransportError>
    where
        F: FnOnce(std::net::TcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            respond(conn);
        });
        let endpoint = DaemonLaneEndpoint { dial: LaneDial::Tcp(addr), token: None };
        let result = endpoint.dial("row-1", "supervisor", None).map(|c| (c.peer.pid, c.peer.created));
        handle.join().unwrap();
        result
    }

    fn respond_with(mut conn: std::net::TcpStream, payload: serde_json::Value) {
        // Drain the request frame so the client's write doesn't block.
        let mut buf = [0u8; 4096];
        let _ = conn.read(&mut buf);
        let res = crate::Frame::res(1, op::LANE_CONNECT, payload);
        let mut line = serde_json::to_vec(&res).unwrap();
        line.push(b'\n');
        conn.write_all(&line).unwrap();
    }

    #[test]
    fn refusal_codes_map_to_typed_errors() {
        for code in ["unknown_workspace", "not_capsule", "bad_lane", "foreign", "voyage_mismatch"] {
            let code = code.to_string();
            let result = dial_against(move |conn| {
                respond_with(conn, serde_json::json!({ "error": "refused", "code": code }));
            });
            match result {
                Err(TransportError::Refused { code: got, .. }) => assert_ne!(got, "no_bridge"),
                other => panic!("expected Refused, got {other:?}"),
            }
        }
    }

    /// The bridge's OWN token check (a daemon that DOES speak
    /// `lane.connect` but rejected this dial's `token`) stays
    /// `unauthenticated` — distinct from the old-daemon case below, which
    /// shares the same wire code but a different message.
    #[test]
    fn a_bridge_daemons_own_bad_token_stays_unauthenticated() {
        let result = dial_against(|conn| {
            respond_with(conn, serde_json::json!({ "error": "bad or missing token", "code": "unauthenticated" }));
        });
        match result {
            Err(TransportError::Refused { code, .. }) => assert_eq!(code, "unauthenticated"),
            other => panic!("expected Refused{{code: unauthenticated}}, got {other:?}"),
        }
    }

    /// An OLD daemon's ordinary control-loop auth gate answers
    /// `lane.connect` with the SAME `unauthenticated` code but its own
    /// "send a token-valid hello first" text — this must be recognized
    /// as `no_bridge`, not confused with a real bridge's bad-token
    /// refusal (ADR 0045 lane B4a Codex review blocker).
    #[test]
    fn an_old_daemons_control_loop_unauthenticated_is_no_bridge() {
        let result = dial_against(|conn| {
            respond_with(
                conn,
                serde_json::json!({ "error": "authentication required: send a token-valid hello first", "code": "unauthenticated" }),
            );
        });
        match result {
            Err(TransportError::Refused { code, .. }) => assert_eq!(code, "no_bridge"),
            other => panic!("expected Refused{{code: no_bridge}}, got {other:?}"),
        }
    }

    /// `dial_failed` is the daemon's OWN dial/authenticate step failing
    /// on the far side — uncertain transport, not a confirmed absence,
    /// so it must classify as `Unreachable` (retried, clock cleared),
    /// never a generic `Io` a caller's absence-window accounting could
    /// charge (ADR 0045 lane B4a Codex review blocker).
    #[test]
    fn dial_failed_is_unreachable_not_generic_io() {
        let result = dial_against(|conn| {
            respond_with(conn, serde_json::json!({ "error": "connection refused dialing the voyage socket", "code": "dial_failed" }));
        });
        assert!(matches!(result, Err(TransportError::Unreachable(_))), "got {result:?}");
    }

    #[test]
    fn lane_absent_decodes_to_endpoint_absent() {
        let result = dial_against(|conn| {
            respond_with(
                conn,
                serde_json::json!({ "error": "the row is terminal", "code": "lane_absent", "kind": "ConnectionRefused" }),
            );
        });
        match result {
            Err(e @ TransportError::Io { .. }) => assert!(e.is_endpoint_absent()),
            other => panic!("expected an absent Io error, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_op_reply_is_no_bridge() {
        let result = dial_against(|conn| {
            respond_with(conn, serde_json::json!({ "error": "unknown op: lane.connect" }));
        });
        match result {
            Err(TransportError::Refused { code, .. }) => assert_eq!(code, "no_bridge"),
            other => panic!("expected Refused{{code: no_bridge}}, got {other:?}"),
        }
    }

    #[test]
    fn an_undetermined_reply_is_undetermined() {
        let result = dial_against(|conn| {
            respond_with(conn, serde_json::json!({ "error": "could not authenticate", "code": "undetermined" }));
        });
        assert!(matches!(result, Err(TransportError::Undetermined { via: "bridge", .. })), "got {result:?}");
    }

    #[test]
    fn a_silent_daemon_is_unreachable_within_two_seconds() {
        // Case 1: nothing ever accepts the connect — the dial bound.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener); // nothing listens; the OS refuses the connect immediately
        let endpoint = DaemonLaneEndpoint { dial: LaneDial::Tcp(addr), token: None };
        let started = Instant::now();
        // `.map(|_| ())`: `DaemonLaneClient` carries no `Debug` impl (a
        // live socket/pipe handle has no useful one), so the success
        // side is dropped before the failure assertion formats `result`.
        let result = endpoint.dial("row-1", "supervisor", None).map(|_| ());
        assert!(matches!(result, Err(TransportError::Unreachable(_))), "got {result:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(3));

        // Case 2: accepts, then never answers — the handshake bound.
        let started = Instant::now();
        let result = dial_against(|conn| {
            // Hold the connection open, answering nothing, until the
            // client's own read timeout gives up.
            std::thread::sleep(std::time::Duration::from_millis(2500));
            drop(conn);
        });
        assert!(matches!(result, Err(TransportError::Unreachable(_))), "got {result:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
    }

    /// A cancel mid-read must be what unblocks it, not an eventual peer
    /// close racing ahead of the cancel — so the peer stays open for up
    /// to 10 s (far past any real cancel latency) while the test asserts
    /// the read actually completed in well under 2 s (ADR 0045 lane B4a
    /// Codex review SHOULD-FIX: the previous version's peer closed after
    /// 500 ms, so the test could pass even if `cancel()` did nothing).
    #[test]
    fn cancel_unblocks_a_pending_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let res = crate::Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": 4242u32, "created": 99u64 }));
            let mut line = serde_json::to_vec(&res).unwrap();
            line.push(b'\n');
            conn.write_all(&line).unwrap();
            // Held open until the test says the cancelled read already
            // completed -- see this test's own doc.
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
        });
        let endpoint = DaemonLaneEndpoint { dial: LaneDial::Tcp(addr), token: None };
        let client = endpoint.dial("row-1", "supervisor", None).expect("handshake succeeds");

        let client = std::sync::Arc::new(client);
        let reader = std::sync::Arc::clone(&client);
        let read_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 16];
            let started = Instant::now();
            let result = reader.read(&mut buf);
            (result, started.elapsed())
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        client.cancel();
        let (result, elapsed) = read_thread.join().unwrap();
        let _ = release_tx.send(()); // only now may the peer close
        handle.join().unwrap();

        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "cancel() itself must unblock the read (peer stayed open 10s) -- took {elapsed:?}"
        );
        // `shutdown(Both)` on a Tcp/Unix stream unblocks a pending LOCAL
        // read as ordered EOF (`Ok(0)`) — it marks this end fully closed,
        // it does not raise an error the way `PipeClient::cancel`'s own
        // OVERLAPPED cancel does. Either outcome proves cancel() (not
        // the still-open peer) produced the completion.
        match result {
            Ok(0) | Err(TransportError::Cancelled) | Err(TransportError::Io { .. }) => {}
            other => panic!("expected cancel to unblock the read as EOF or an error, got {other:?}"),
        }
    }

    #[test]
    fn a_wire_identity_that_differs_from_the_reported_peer_is_foreign() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let res = crate::Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": 111u32, "created": 1u64 }));
            let mut line = serde_json::to_vec(&res).unwrap();
            line.push(b'\n');
            conn.write_all(&line).unwrap();
            // The wire hello: read whatever `exchange_identity` sends,
            // answer with anything — `FixedExchange::feed` below ignores
            // the bytes and always decodes pid=222, deliberately NOT the
            // 111 just reported, which is the mismatch this test proves.
            let mut hello_buf = [0u8; 256];
            let _ = conn.read(&mut hello_buf);
            let _ = conn.write_all(b"irrelevant");
        });

        let endpoint = DaemonLaneEndpoint { dial: LaneDial::Tcp(addr), token: None };
        let client = endpoint.dial("row-1", "supervisor", None).expect("handshake succeeds");

        struct FixedExchange;
        impl IdentityExchange for FixedExchange {
            fn encode_request(&self) -> Vec<u8> {
                b"status".to_vec()
            }
            fn feed(&mut self, _bytes: &[u8]) -> sot_log::exchange::ExchangeDecode {
                sot_log::exchange::ExchangeDecode::Identity { pid: 222, created: 1 }
            }
        }
        let mut exchange = FixedExchange;
        let outcome = endpoint.challenge(&client, &mut exchange, Instant::now() + std::time::Duration::from_secs(1));
        handle.join().unwrap();
        assert!(matches!(outcome, ChallengeOutcome::Foreign), "a mismatched pid must never be Proven");
    }
}
