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
//! [`LaneStream`] is the ONE adapter every transport (`Unix`/`Pipe`/
//! `Bridged`) goes through, implementing `sot_log::lane::client::Client`
//! directly — [`DaemonLaneClient`] is just `{stream: LaneStream, peer}`,
//! delegating every `Client` call straight to `stream`. `Unix` reuses
//! `sot_log::lane::socket_unix::SocketClient` and `Pipe` reuses `sot_log::
//! pipe_win::PipeClient` verbatim (both already bounded, cancellable
//! connectors with real `cancel()`s); `Bridged` is the ssh child.
//! [`DaemonLaneEndpoint::dial`] then runs in two ABSOLUTE-deadline phases sharing this one
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

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use sot_log::identity::challenge::{ChallengeOutcome, PeerAuthOutcome, PeerAuthenticated, StatusFailure};
use sot_log::lane::client::{Client, Endpoint, PeerIdentity};
use sot_log::identity::exchange::IdentityExchange;
use sot_log::lane::transport::{TransportError, CONNECT_BOUND};

use crate::{op, Frame, Kind, LaneConnectReq, LaneConnectRes};

/// How to reach a row's daemon — a local Unix socket / Windows named
/// pipe (`Local`, matching the platform endpoints' own transport for
/// this host's daemon), or an ssh child (`Ssh`, C3 as amended — the
/// transport for every OTHER host, since the daemon has no TCP listener
/// and the control plane dials no TCP port, ADR 0049 `## User isolation`).
/// Carries no row: the row rides `Endpoint`'s own `lane` argument, named
/// exactly once — never duplicated onto the dial value itself.
pub enum LaneDial {
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
    spare: std::sync::Mutex<VoyageSpare>,
    #[cfg(any(test, feature = "test-handshake-bound"))]
    test_ssh_spawner: Option<TestSshSpawner>,
    #[cfg(any(test, feature = "test-handshake-bound"))]
    test_handshake_bound: Option<std::time::Duration>,
}

enum VoyageSpare {
    Unused,
    Parked(BridgedClient),
    Spent,
}

#[cfg(any(test, feature = "test-handshake-bound"))]
type TestSshSpawner = std::sync::Arc<dyn Fn(&crate::topology::ssh_bridge::SshRecipe, &crate::topology::ssh_bridge::LinkGate) -> Result<std::process::Child, crate::topology::ssh_bridge::SpawnError> + Send + Sync>;

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

/// The `Ssh` dial's client: a spawned `ssh … sotd stdio-bridge`
/// child whose stdin/stdout carry the lane bridge's own frames.
/// `ChildStdout`/`ChildStdin` are converted to `File` through `OwnedFd`
/// (unix) / `OwnedHandle` (windows) at construction, so `read`/
/// `write_all` go through `&self`.
///
/// A pipe has no `set_read_timeout`, so `cancel()` sets the flag and
/// **kills the child**; the kill closes the child's stdout, which EOFs a
/// parked read on both platforms. `Drop` kills and waits, so no ssh child
/// outlives its client.
///
/// No new trust claim: `DaemonLaneEndpoint`'s own doc already states
/// that it holds no kernel handle on the peer and that every identity
/// claim traces to the daemon's own observation.
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

    fn exited(&self) -> bool {
        match self.child.lock() {
            Ok(mut child) => !matches!(child.try_wait(), Ok(None)),
            Err(_) => true,
        }
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

    /// The io error, with the child's last stderr line after it when one was captured: a dead child's own "Permission
    /// denied" explains the generic "broken pipe" its closed pipe leaves behind, and the error keeps its own words.
    fn diagnose(&self, source: std::io::Error) -> std::io::Error {
        match self.poll_last_stderr() {
            Some(line) => std::io::Error::new(source.kind(), format!("{source}: {line}")),
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
/// reimplementing either; `Bridged` needed a wrapper of its own (`sot-log`
/// has no ssh-child client). `DaemonLaneClient` below is nothing more than
/// `{stream: LaneStream, peer}` — every `Client` call delegates straight
/// through.
enum LaneStream {
    #[cfg(unix)]
    Unix(sot_log::lane::socket_unix::SocketClient),
    #[cfg(windows)]
    Pipe(sot_log::lane::pipe_win::PipeClient),
    Bridged(BridgedClient),
}

impl Client for LaneStream {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        match self {
            #[cfg(unix)]
            LaneStream::Unix(c) => c.write_all(bytes),
            #[cfg(windows)]
            LaneStream::Pipe(c) => c.write_all(bytes),
            LaneStream::Bridged(c) => c.write_all(bytes),
        }
    }
    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        match self {
            #[cfg(unix)]
            LaneStream::Unix(c) => c.read(buf),
            #[cfg(windows)]
            LaneStream::Pipe(c) => c.read(buf),
            LaneStream::Bridged(c) => c.read(buf),
        }
    }
    fn cancel(&self) {
        match self {
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

    fn drop_spare(&self) {
        let mut state = self.spare.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*state, VoyageSpare::Parked(_)) {
            *state = VoyageSpare::Unused;
        }
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
        Some(code) => Err(TransportError::Refused { code: code.to_string(), detail: werr.error }),
        None => Err(TransportError::Refused { code: "no_bridge".to_string(), detail: werr.error }),
    }
}

/// `PipeClient`/`SocketClient`/`BridgedClient`'s `write_all`/`read` are
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

/// The hello's reply, or why it is none: an `{error, code}` payload is the daemon refusing this client (an older
/// protocol, an unnamed host or account, a second account on the host), typed `Refused` with the daemon's own code
/// and message; anything that is not the hello's reply is a daemon that predates the lane bridge.
fn classify_hello_reply(frame: Frame) -> Result<(), TransportError> {
    if frame.kind != Kind::Res || frame.op != op::HELLO {
        return Err(TransportError::Refused {
            code: "no_bridge".to_string(),
            detail: format!(
                "unexpected reply to the hello (op={:?} kind={:?}) — a daemon that predates the lane bridge (ADR 0045), or a foreign wire protocol",
                frame.op, frame.kind
            ),
        });
    }
    match serde_json::from_value::<WireError>(frame.payload) {
        Ok(WireError { error, code, .. }) => Err(TransportError::Refused { code: code.unwrap_or_else(|| "hello_refused".to_string()), detail: error }),
        Err(_) => Ok(()),
    }
}

/// The ONE deadline helper the handshake shares across all
/// transports: write the hello and the `lane.connect` request in ONE write
/// (the handoff pipelines, so the hello costs no round trip), read the
/// hello's reply and then the request's through ONE reader, as a SINGLE
/// operation bounded by one absolute deadline — not a per-read socket
/// timeout a trickle of bytes could extend indefinitely, and the write is
/// bounded by the SAME deadline too (a slow/stalled write is no less a
/// hang than a slow read). `on_timeout` is `stream.cancel()` —
/// `shutdown(Both)` for `Unix`, `PipeClient`'s own OVERLAPPED cancel for
/// `Pipe` — the SAME mechanism `exchange_identity`'s own wire round trip
/// already uses for the POST-handshake attach hello, so the handshake and
/// the hello that immediately follows it are bounded identically.
fn run_handshake(stream: &LaneStream, hello: &Frame, req: &Frame, deadline: Instant) -> Result<(u32, u64), TransportError> {
    let outcome = sot_log::identity::deadline::run_with_deadline(deadline, || stream.cancel(), || -> Result<Frame, TransportError> {
        use std::io::Write as _;
        let unreachable = |e: &dyn std::fmt::Display| TransportError::Unreachable(std::io::Error::other(e.to_string()));
        let mut both = Vec::new();
        crate::codec::write_frame_blocking(&mut both, hello).map_err(|e| unreachable(&e))?;
        crate::codec::write_frame_blocking(&mut both, req).map_err(|e| unreachable(&e))?;
        ClientIo(stream).write_all(&both).map_err(TransportError::Unreachable)?;
        let mut r = std::io::BufReader::new(ClientIo(stream));
        let hello_reply = read_reply(stream, &mut r)?;
        classify_hello_reply(hello_reply)?;
        read_reply(stream, &mut r)
    });
    // A refused hello and every reply's own classification are the daemon's answer and are returned as it gave them
    // (BLOCKER 2 of round 1: a stderr line used to replace them). The ssh child's last stderr line is added once: by
    // `diagnose` to the client's own write or read error, by `read_reply` to a reply that ended early, ran over the cap
    // or did not parse, and here to the bound.
    match outcome {
        Some(Ok(frame)) => classify_reply(frame),
        Some(Err(e)) => Err(e),
        None => Err(TransportError::Unreachable(with_ssh_line(
            stream,
            std::io::Error::new(std::io::ErrorKind::TimedOut, "lane.connect: handshake timed out"),
        ))),
    }
}

/// One reply of the handshake. A read the client failed is its own io error, whose text names the ssh child's line
/// already (`diagnose`); a read that ended before a whole frame (the child's stdout closed, an over-long or unparsable
/// line) went through no client error, so the line is added here. The error's whole chain is kept (`{:#}`).
fn read_reply(stream: &LaneStream, r: &mut std::io::BufReader<ClientIo<'_>>) -> Result<Frame, TransportError> {
    crate::codec::read_frame_blocking(r).map_err(|e| {
        let text = std::io::Error::other(format!("{e:#}"));
        if e.downcast_ref::<std::io::Error>().is_some() {
            TransportError::Unreachable(text)
        } else {
            TransportError::Unreachable(with_ssh_line(stream, text))
        }
    })
}

/// `e`, with the ssh child's last stderr line after it when `stream` is one: a dying child's stdout closes as a clean
/// `Ok(0)` EOF that the codec turns into its own generic text, so the child's own complaint is the diagnosis to add.
fn with_ssh_line(stream: &LaneStream, e: std::io::Error) -> std::io::Error {
    if let LaneStream::Bridged(bridged) = stream {
        if let Some(line) = bridged.poll_last_stderr() {
            return std::io::Error::new(e.kind(), format!("{e}: {line}"));
        }
    }
    e
}

/// The handshake over a connected stream: the hello and the `lane.connect` request, the replies classified, and the
/// client that holds the stream and the peer the daemon reported.
fn handshake(stream: LaneStream, hello: &Frame, req: &Frame, bound: std::time::Duration) -> Result<DaemonLaneClient, TransportError> {
    let (pid, created) = run_handshake(&stream, hello, req, Instant::now() + bound)?;
    Ok(DaemonLaneClient { stream, peer: PeerAuthenticated { pid, created } })
}

impl DaemonLaneEndpoint {
    pub fn new(dial: LaneDial, token: Option<String>) -> Self {
        Self {
            dial,
            token,
            spare: std::sync::Mutex::new(VoyageSpare::Unused),
            #[cfg(any(test, feature = "test-handshake-bound"))]
            test_ssh_spawner: None,
            #[cfg(any(test, feature = "test-handshake-bound"))]
            test_handshake_bound: None,
        }
    }

    #[cfg(any(test, feature = "test-handshake-bound"))]
    pub fn with_test_ssh_spawner(mut self, spawner: TestSshSpawner) -> Self {
        self.test_ssh_spawner = Some(spawner);
        self
    }

    #[cfg(any(test, feature = "test-handshake-bound"))]
    pub fn with_test_handshake_bound(mut self, bound: std::time::Duration) -> Self {
        self.test_handshake_bound = Some(bound);
        self
    }

    fn spawn_ssh(&self, recipe: &crate::topology::ssh_bridge::SshRecipe, gate: &crate::topology::ssh_bridge::LinkGate) -> Result<BridgedClient, TransportError> {
        if !gate.is_up() {
            return Err(TransportError::LinkDown);
        }
        #[cfg(any(test, feature = "test-handshake-bound"))]
        if let Some(spawn) = &self.test_ssh_spawner {
            return match spawn(recipe, gate) {
                Ok(child) => BridgedClient::wrap(child).map_err(TransportError::Unreachable),
                Err(crate::topology::ssh_bridge::SpawnError::LinkDown) => Err(TransportError::LinkDown),
                Err(crate::topology::ssh_bridge::SpawnError::Io(e)) => Err(TransportError::Unreachable(e)),
            };
        }
        BridgedClient::spawn(recipe, gate)
    }

    fn ssh_client(&self, kind: &str, recipe: &crate::topology::ssh_bridge::SshRecipe, gate: &crate::topology::ssh_bridge::LinkGate) -> Result<BridgedClient, TransportError> {
        if kind == "voyage" {
            if let Some(spare) = self.take_spare() {
                if gate.is_up() {
                    return Ok(spare);
                }
            }
            return self.spawn_ssh(recipe, gate);
        }
        let client = self.spawn_ssh(recipe, gate)?;
        if kind == "supervisor" {
            self.start_spare(|| self.spawn_ssh(recipe, gate));
        }
        Ok(client)
    }

    fn start_spare(&self, spawn: impl FnOnce() -> Result<BridgedClient, TransportError>) {
        let mut state = self.spare.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*state, VoyageSpare::Unused) {
            if let Ok(client) = spawn() {
                *state = VoyageSpare::Parked(client);
            }
        }
    }

    fn take_spare(&self) -> Option<BridgedClient> {
        let mut state = self.spare.lock().unwrap_or_else(|e| e.into_inner());
        match std::mem::replace(&mut *state, VoyageSpare::Spent) {
            VoyageSpare::Parked(client) if !client.exited() => Some(client),
            _ => None,
        }
    }

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
        let hello = crate::HelloReq::this_process("sot-lane-dial", crate::HANDOFF_ROLE, sot_log::host::state_dir::host_name().ok())
            .map_err(|e| TransportError::Unreachable(std::io::Error::other(e.to_string())))?;
        let hello = Frame::req(1, op::HELLO, serde_json::to_value(&hello).expect("HelloReq always serializes"));
        let frame = Frame::req(2, op::LANE_CONNECT, serde_json::to_value(&req).expect("LaneConnectReq always serializes"));

        let stream = match &self.dial {
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
                let client = self.ssh_client(kind, recipe, gate)?;
                LaneStream::Bridged(client)
            }
        };

        #[cfg(any(test, feature = "test-handshake-bound"))]
        let bound = self.test_handshake_bound.unwrap_or(CONNECT_BOUND);
        #[cfg(not(any(test, feature = "test-handshake-bound")))]
        let bound = CONNECT_BOUND;
        let result = handshake(stream, &hello, &frame, bound);
        if kind == "supervisor" && result.is_err() {
            self.drop_spare();
        }
        result
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
