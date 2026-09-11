//! ADR 0045 decision 3: `DaemonLaneEndpoint`, the attach client's own
//! `sot_log::client::Endpoint` for the lane bridge — a capsule row's
//! supervisor or voyage lane, piped through the row's OWN daemon
//! (`lane.connect`, `crate::ops::LANE_CONNECT`) instead of a loopback
//! named pipe or Unix socket the platform endpoints dial directly. Lives
//! in `sot-protocol`, not `sot-log`, because it is a WIRE CLIENT of this
//! crate's own `LaneConnectReq`/`LaneConnectRes` — `sot-log` has no
//! dependency the other way.
//!
//! # The split identity proof (decision 3)
//!
//! A `lane.connect` dial gets its OWN peer-identity report for free: the
//! daemon it asked ran steps 1-3 of the challenge on ITS dial and
//! returned the observed `(pid, created)` in [`crate::ops::
//! LaneConnectRes`]. This module's own [`DaemonLaneEndpoint::
//! authenticate_server`] simply hands that report back — no OS-level
//! check of its own is possible from here, reaching the peer only
//! through a bridged pipe. [`DaemonLaneEndpoint::challenge`] then runs
//! `sot_log::challenge::exchange_identity` (steps 4-5, the SAME wire
//! round trip every platform endpoint's own `challenge()` runs) over
//! that pipe and accepts the result ONLY when it equals the daemon's own
//! report — never on its own say-so.
//!
//! # Refusals and uncertainty are typed (decision 4)
//!
//! [`dial`](DaemonLaneEndpoint::dial) never lets `lane.connect`'s wire
//! outcome collapse into a bare `io::Error`: a `Refused` reply is
//! terminal, `Unreachable`/`sot_log::transport::TransportError::
//! BridgeUndetermined` are retried by the caller (`sot_log::
//! fe_client_io`'s own three connect sites classify them), and only
//! `lane_absent` decodes onto [`sot_log::transport::TransportError::Io`]
//! with a `NotFound`/`ConnectionRefused` kind — the one case
//! `is_endpoint_absent()` recognizes.
//!
//! # No process-control authority (decision 3)
//!
//! [`BridgedPeer`] implements ONLY `sot_log::client::PeerIdentity` (bare
//! `pid`/`created`) — never `PeerProcess` (`reverify`/`wait`/
//! `terminate`): this endpoint cannot itself wait on or terminate a
//! process it only ever reaches through the daemon's own pipe.

use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use sot_log::challenge::{ChallengeOutcome, PeerAuthOutcome, PeerAuthenticated, StatusFailure};
use sot_log::client::{Client, Endpoint, PeerIdentity};
use sot_log::exchange::IdentityExchange;
use sot_log::transport::{TransportError, CONNECT_BOUND};

use crate::{op, Frame, Kind, LaneConnectReq, LaneConnectRes};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

/// How to reach a row's daemon — the loopback tunnel (`Tcp`, matching
/// `proxy.connect`'s own transport for a remote host) or a local Unix
/// socket / Windows named pipe (`Local`, matching the platform
/// endpoints' own transport for this host's daemon). Carries no row: the
/// row rides `Endpoint`'s own `lane` argument, named exactly once
/// (decision 3, decision 5) — never duplicated onto the dial value
/// itself.
pub enum LaneDial {
    Tcp(SocketAddr),
    Local(PathBuf),
}

/// An `Endpoint` value naming one daemon connection, never a row
/// (decision 3, decision 5). `token` mirrors `ProxyConnectReq::token` /
/// `LaneConnectReq::token` — present only on a token-configured daemon.
pub struct DaemonLaneEndpoint {
    pub dial: LaneDial,
    pub token: Option<String>,
}

/// The lane peer's identity, exactly as the DAEMON'S OWN dial observed
/// it (steps 1-3, run by the daemon, never by this endpoint) —
/// deliberately the same two fields as `sot_log::challenge::
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

/// One of the three concrete streams a `lane.connect` dial can hand
/// back — `Tcp`/`Unix` are raw `std` sockets (this module's own
/// `Client` impl below does its own `shutdown(Both)`-based cancel for
/// them); `Pipe` reuses `sot_log::pipe_win::PipeClient` verbatim,
/// inheriting its real OVERLAPPED-I/O cancel rather than reimplementing
/// one — Windows has no socket-shutdown equivalent for a named pipe.
enum LaneStream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
    #[cfg(windows)]
    Pipe(sot_log::pipe_win::PipeClient),
}

/// The connected, identity-reported lane pipe a `lane.connect` dial
/// produced. `stream`/`peer` are private — a caller drives this only
/// through the `Client`/`Endpoint` trait vocabulary, exactly like
/// `PipeClient`/`SocketClient`.
pub struct DaemonLaneClient {
    stream: LaneStream,
    peer: PeerAuthenticated,
    cancelled: AtomicBool,
}

impl Client for DaemonLaneClient {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        use std::io::Write;
        match &self.stream {
            LaneStream::Tcp(s) => (&*s).write_all(bytes).map_err(|source| TransportError::Io { op: "lane write", source }),
            #[cfg(unix)]
            LaneStream::Unix(s) => (&*s).write_all(bytes).map_err(|source| TransportError::Io { op: "lane write", source }),
            #[cfg(windows)]
            LaneStream::Pipe(p) => p.write_all(bytes),
        }
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        use std::io::Read;
        match &self.stream {
            LaneStream::Tcp(s) => (&*s).read(buf).map_err(|source| TransportError::Io { op: "lane read", source }),
            #[cfg(unix)]
            LaneStream::Unix(s) => (&*s).read(buf).map_err(|source| TransportError::Io { op: "lane read", source }),
            #[cfg(windows)]
            LaneStream::Pipe(p) => p.read(buf),
        }
    }

    /// `shutdown(Both)` for `Tcp`/`Unix` (unblocks a pending read/write
    /// the same way any other socket cancel does); `PipeClient::cancel`
    /// for `Pipe` — its own OVERLAPPED-I/O cancel, not a shutdown this
    /// stream kind has no equivalent of. Property 34: a cancelled client
    /// refuses every FURTHER call with `TransportError::Cancelled`,
    /// checked at the top of `write_all`/`read` above.
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        match &self.stream {
            LaneStream::Tcp(s) => {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
            #[cfg(unix)]
            LaneStream::Unix(s) => {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
            #[cfg(windows)]
            LaneStream::Pipe(p) => p.cancel(),
        }
    }
}

impl Endpoint for DaemonLaneEndpoint {
    type Client = DaemonLaneClient;
    type Process = BridgedPeer;

    /// `lane` is the row's `tmux_session` name — `LaneConnectReq::target`
    /// is required for the voyage lane too (a voyage is reached only
    /// through the row that owns it; the daemon dials by `voyage_id` but
    /// authorizes by row), so no daemon-side change was needed here: B3
    /// already requires `target` on both lane kinds.
    fn connect_voyage_unchallenged(&self, lane: &str, voyage_id: &str) -> Result<Self::Client, TransportError> {
        self.dial(lane, "voyage", Some(voyage_id.to_string()))
    }

    fn connect_supervisor_unchallenged(&self, lane: &str) -> Result<Self::Client, TransportError> {
        self.dial(lane, "supervisor", None)
    }

    /// Steps 4-5 ONLY, over the already-piped connection — the SAME
    /// `exchange_identity` every platform endpoint's own `challenge()`
    /// runs, bound here against the daemon's own report (`conn.peer`,
    /// steps 1-3, already proven before this endpoint ever saw the
    /// connection) rather than a fresh OS-level check this endpoint has
    /// no way to run.
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

    /// No wire I/O of its own (decision 3): the daemon already ran
    /// steps 1-3 on its own dial before this client ever existed, so
    /// this simply hands that report back.
    fn authenticate_server(&self, conn: &Self::Client) -> PeerAuthOutcome {
        PeerAuthOutcome::Authenticated(conn.peer)
    }
}

/// The standard error payload `crate::ops::LaneConnectReq`'s own doc
/// promises on refusal, `{error, code}` (plus `kind` for `lane_absent`)
/// — the same shape `proxy::reject`/`lane_bridge::reject_lane_absent`
/// write on the daemon side. `code` is optional at the TYPE level only
/// to catch a daemon that predates the lane bridge entirely: an old
/// daemon's "unknown op" answer is `{"error": "unknown op: lane.connect"}`
/// with no `code` at all.
#[derive(serde::Deserialize)]
struct WireError {
    error: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

/// The `io::ErrorKind` `Debug` text `lane_bridge::absent_kind` writes,
/// decoded back — the only two kinds that daemon ever actually sends
/// (`TransportError::is_endpoint_absent`'s own predicate), so anything
/// else (including an absent `kind` field on `dial_failed`, which never
/// carries one) safely falls back to `Other`: a caller that cares about
/// absence at all only ever tests THOSE two kinds.
fn absent_kind_from_wire(s: Option<&str>) -> std::io::ErrorKind {
    match s {
        Some("NotFound") => std::io::ErrorKind::NotFound,
        Some("ConnectionRefused") => std::io::ErrorKind::ConnectionRefused,
        _ => std::io::ErrorKind::Other,
    }
}

/// Classify one `lane.connect` reply frame into the daemon's own
/// `(pid, created)` report, or the typed refusal/uncertainty decision 4
/// names. Matched BEFORE any `io::Error` conversion exists to unwrap
/// these into (`sot_log::client::transport_error_to_io` only ever sees
/// the RESULT of this function, never the wire payload directly).
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
        Some("dial_failed") => Err(TransportError::Io {
            op: "lane.connect",
            source: std::io::Error::new(absent_kind_from_wire(werr.kind.as_deref()), werr.error),
        }),
        Some("undetermined") => Err(TransportError::BridgeUndetermined(werr.error)),
        Some(code) => Err(TransportError::Refused { code: code.to_string(), detail: werr.error }),
        None => Err(TransportError::Refused { code: "no_bridge".to_string(), detail: werr.error }),
    }
}

/// Write one frame then read one reply, over any stream kind that
/// implements `std::io::{Read, Write}` BY SHARED REFERENCE — `TcpStream`
/// and `UnixStream` both do (the standard concurrent-use-by-reference
/// impls), so this one function serves both without owning the stream:
/// `dial` still needs it afterward to build the `LaneStream` value.
fn send_and_receive<S>(stream: &S, frame: &Frame) -> Result<Frame, TransportError>
where
    for<'a> &'a S: std::io::Read + std::io::Write,
{
    let mut w = stream;
    crate::codec::write_frame_blocking(&mut w, frame).map_err(|e| TransportError::Unreachable(std::io::Error::other(e.to_string())))?;
    let mut r = std::io::BufReader::new(stream);
    crate::codec::read_frame_blocking(&mut r).map_err(|e| TransportError::Unreachable(std::io::Error::other(e.to_string())))
}

/// `PipeClient`'s `write_all`/`read` are `&self` methods returning
/// `Result<_, TransportError>` (crate::client::Client's own shape), not
/// `std::io::{Read, Write}` — this small adapter is what lets the SAME
/// `write_frame_blocking`/`read_frame_blocking` codec functions drive a
/// pipe too, so every platform speaks byte-identical framing (decision
/// 7) rather than a pipe-specific reimplementation of it.
#[cfg(windows)]
struct PipeIo<'a>(&'a sot_log::pipe_win::PipeClient);

#[cfg(windows)]
impl<'a> std::io::Read for PipeIo<'a> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf).map_err(|e| std::io::Error::other(e.to_string()))
    }
}

#[cfg(windows)]
impl<'a> std::io::Write for PipeIo<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write_all(buf).map(|()| buf.len()).map_err(|e| std::io::Error::other(e.to_string()))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The pipe twin of [`send_and_receive`]: `PipeClient` has no socket-level
/// read timeout to set/clear, so the whole write+read round trip runs
/// under ONE `sot_log::deadline::run_with_deadline` bound instead,
/// `on_timeout` calling `PipeClient::cancel` to unblock it — the same
/// mechanism `exchange_identity`'s own wire round trip already uses.
#[cfg(windows)]
fn send_and_receive_pipe(client: &sot_log::pipe_win::PipeClient, frame: &Frame, deadline: Instant) -> Result<Frame, TransportError> {
    let outcome = sot_log::deadline::run_with_deadline(deadline, || client.cancel(), || -> Result<Frame, TransportError> {
        let mut io = PipeIo(client);
        crate::codec::write_frame_blocking(&mut io, frame).map_err(|e| TransportError::Unreachable(std::io::Error::other(e.to_string())))?;
        let mut r = std::io::BufReader::new(PipeIo(client));
        crate::codec::read_frame_blocking(&mut r).map_err(|e| TransportError::Unreachable(std::io::Error::other(e.to_string())))
    });
    match outcome {
        Some(inner) => inner,
        None => Err(TransportError::Unreachable(std::io::Error::new(std::io::ErrorKind::TimedOut, "lane.connect: handshake timed out"))),
    }
}

impl DaemonLaneEndpoint {
    /// The blocking dial: connect with a 2 s bound
    /// (`sot_log::transport::CONNECT_BOUND`, reused rather than a second
    /// magic number), write the `lane.connect` request, read ONE reply
    /// frame under a separate 2 s bound, then classify it (decision 4).
    /// `row` is the row's `tmux_session` name (`LaneConnectReq::target`,
    /// required for both lane kinds); `kind` is `"supervisor"` or
    /// `"voyage"` (`LaneConnectReq::lane`).
    fn dial(&self, row: &str, kind: &str, voyage_id: Option<String>) -> Result<DaemonLaneClient, TransportError> {
        let req = LaneConnectReq {
            target: row.to_string(),
            lane: kind.to_string(),
            voyage_id,
            token: self.token.clone(),
        };
        let frame = Frame::req(1, op::LANE_CONNECT, serde_json::to_value(&req).expect("LaneConnectReq always serializes"));

        match &self.dial {
            LaneDial::Tcp(addr) => {
                let stream = TcpStream::connect_timeout(addr, CONNECT_BOUND).map_err(TransportError::Unreachable)?;
                stream.set_read_timeout(Some(CONNECT_BOUND)).map_err(TransportError::Unreachable)?;
                let reply = send_and_receive(&stream, &frame)?;
                let (pid, created) = classify_reply(reply)?;
                stream.set_read_timeout(None).map_err(TransportError::Unreachable)?;
                Ok(DaemonLaneClient {
                    stream: LaneStream::Tcp(stream),
                    peer: PeerAuthenticated { pid, created },
                    cancelled: AtomicBool::new(false),
                })
            }
            #[cfg(unix)]
            LaneDial::Local(path) => {
                let deadline = Instant::now() + CONNECT_BOUND;
                let path_owned = path.clone();
                let stream = match sot_log::deadline::run_with_deadline(deadline, || {}, move || UnixStream::connect(&path_owned)) {
                    Some(Ok(s)) => s,
                    Some(Err(e)) => return Err(TransportError::Unreachable(e)),
                    None => return Err(TransportError::Unreachable(std::io::Error::new(std::io::ErrorKind::TimedOut, "lane.connect: dial timed out"))),
                };
                stream.set_read_timeout(Some(CONNECT_BOUND)).map_err(TransportError::Unreachable)?;
                let reply = send_and_receive(&stream, &frame)?;
                let (pid, created) = classify_reply(reply)?;
                stream.set_read_timeout(None).map_err(TransportError::Unreachable)?;
                Ok(DaemonLaneClient {
                    stream: LaneStream::Unix(stream),
                    peer: PeerAuthenticated { pid, created },
                    cancelled: AtomicBool::new(false),
                })
            }
            #[cfg(windows)]
            LaneDial::Local(path) => {
                let path_str = path.to_str().ok_or_else(|| {
                    TransportError::Unreachable(std::io::Error::new(std::io::ErrorKind::InvalidInput, "lane pipe path is not valid Unicode"))
                })?;
                let client = sot_log::pipe_win::connect_pipe_path_unchallenged(path_str).map_err(|te| {
                    let io = match te {
                        TransportError::Io { source, .. } => source,
                        other => std::io::Error::other(other.to_string()),
                    };
                    TransportError::Unreachable(io)
                })?;
                let deadline = Instant::now() + CONNECT_BOUND;
                let reply = send_and_receive_pipe(&client, &frame, deadline)?;
                let (pid, created) = classify_reply(reply)?;
                Ok(DaemonLaneClient {
                    stream: LaneStream::Pipe(client),
                    peer: PeerAuthenticated { pid, created },
                    cancelled: AtomicBool::new(false),
                })
            }
        }
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
        for code in ["unknown_workspace", "not_capsule", "bad_lane", "unauthenticated", "foreign"] {
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
        assert!(matches!(result, Err(TransportError::BridgeUndetermined(_))), "got {result:?}");
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

    #[test]
    fn cancel_unblocks_a_pending_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let res = crate::Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": 4242u32, "created": 99u64 }));
            let mut line = serde_json::to_vec(&res).unwrap();
            line.push(b'\n');
            conn.write_all(&line).unwrap();
            // Hold the connection open — no further bytes, no close —
            // so the read below genuinely blocks on nothing rather than
            // racing an EOF this test does not mean to exercise.
            std::thread::sleep(std::time::Duration::from_millis(500));
        });
        let endpoint = DaemonLaneEndpoint { dial: LaneDial::Tcp(addr), token: None };
        let client = endpoint.dial("row-1", "supervisor", None).expect("handshake succeeds");

        let client = std::sync::Arc::new(client);
        let reader = std::sync::Arc::clone(&client);
        let read_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 16];
            reader.read(&mut buf)
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        client.cancel();
        let result = read_thread.join().unwrap();
        handle.join().unwrap();
        // `shutdown(Both)` on a Tcp/Unix stream unblocks a pending LOCAL
        // read as ordered EOF (`Ok(0)`) — it marks this end fully closed,
        // it does not raise an error the way `PipeClient::cancel`'s own
        // OVERLAPPED cancel does. Either outcome proves the read did not
        // stay blocked forever; `Ok(n > 0)` would not (that would mean
        // the daemon, not the cancel, produced the completion).
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
