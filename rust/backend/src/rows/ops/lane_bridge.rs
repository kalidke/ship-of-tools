//! rows/ops/lane_bridge.rs — ADR 0045 decision 2: `lane.connect`, the daemon-side
//! half of "one attach path: through the daemon, local or remote"
//! (decision 1). A dedicated connection whose hello says `handoff` and whose next frame is
//! `lane.connect` becomes, after one reply, a raw byte pipe onto the
//! capsule row's supervisor or voyage lane — the `proxy.connect`
//! mechanism (ADR 0035), verbatim, for lane endpoints instead of a
//! loopback port. The daemon's dial is also the recovery trigger: an
//! absent supervisor lane on a resumable row is resumed (`ensure_started`
//! with `Reconnect` intent, which may resume but never resets a row) before
//! the dial is answered. Once the pipe starts, the daemon never decodes a
//! lane frame again — the frontend runs its own end-to-end `hello`
//! (steps 4-5 of the challenge) over the pipe, bound against the `pid`/
//! `created` this module reports from ITS OWN dial (steps 1-3).
//!
//! Ungated, exactly like the capsule runtime in `rows/spawn/detach.rs`
//! (macOS wiring lane): the capsule runtime is this daemon's runtime on
//! every host it builds for, so `lane.connect` is answered the same way
//! everywhere. What differs per platform is one leaf — `pipe_upstream`'s
//! `cfg(unix)`/`cfg(windows)` arms below — not whether this surface
//! exists at all. A port to a host with neither arm fails to compile
//! there, loudly, which is the honest answer; it never silently
//! degrades into a workspace with no supervisor.

use std::path::{Path, PathBuf};

use anyhow::Result;
use sot_log::identity::challenge::{PeerAuthOutcome, PeerAuthenticated};
use sot_log::lane::client::{Endpoint, PlatformEndpoint};
use sot_log::host::state_dir::state_dir_hash;
use sot_log::lane::transport::TransportError;
use sot_protocol::{codec, op, Frame, LaneConnectReq};
use tokio::io::{AsyncBufRead, AsyncWrite};

use crate::rows::Workspaces;

/// The two lanes `lane.connect` can name. The voyage id rides the
/// variant itself (Codex review, 2026-09-11) rather than a second
/// `Option<String>` field next to it — a `Lane::Voyage` with no id is
/// then unrepresentable, closing the `expect` an out-of-band optional
/// used to need.
enum Lane {
    Supervisor,
    Voyage(String),
}

/// Why the blocking dial+resume+authenticate step (below) could not hand
/// back a piped connection. Kept separate from `TransportError` itself
/// (rather than reusing it directly) because `Foreign`/`Undetermined`
/// come from [`Endpoint::authenticate_server`]'s own [`PeerAuthOutcome`],
/// not a `TransportError` at all; `VoyageMismatch` comes from neither (a
/// LOCAL pointer read, never a dial); and `Absent`'s `kind` is a derived
/// `io::ErrorKind` string, not the error itself.
enum DialFail {
    /// The endpoint is not there right now — for the supervisor lane,
    /// only after a resume was tried (or refused because the row is
    /// terminal). `kind` is the observing error's own `io::ErrorKind`
    /// `Debug` text.
    Absent { kind: String, detail: String },
    /// The requested `voyage_id` is not the TARGET row's own current
    /// voyage (Codex review BLOCKER, 2026-09-11: both platform endpoints
    /// ignore the lane-namespace argument, so an unchecked voyage id
    /// would pipe whichever row happens to own that id, not the row the
    /// caller named). Discovered by a LOCAL pointer read, before ever
    /// dialing anything.
    VoyageMismatch,
    Foreign,
    Undetermined,
    /// Any other I/O error dialing, authenticating, or reading the
    /// row's own voyage pointer.
    Other(String),
}

/// `TransportError::is_endpoint_absent()`'s own `io::ErrorKind` — only
/// ever called on an error already known to satisfy that predicate (a
/// Windows `NotFound` or a Linux `ConnectionRefused`/`NotFound`), so the
/// `Io` arm is the only reachable one; the fallback exists only so this
/// stays total.
fn absent_kind(e: &TransportError) -> String {
    if let TransportError::Io { source, .. } = e {
        format!("{:?}", source.kind())
    } else {
        format!("{e:?}")
    }
}

/// Ownership check for the VOYAGE lane (Codex review BLOCKER,
/// 2026-09-11): a `voyage_id` is meaningful only as the TARGET row's OWN
/// current voyage — `Endpoint::connect_voyage_unchallenged`'s `lane`
/// argument is the daemon-lane endpoint's own namespace, ignored by both
/// platform endpoints (`client.rs`'s own doc), so nothing about the
/// DIAL itself ties a voyage id to any particular row. This reads
/// `<state_dir>/drawer.voyage` (the SAME durable pointer `phase_of`/
/// `resume_locked` already trust) and compares it to what the caller
/// asked for, entirely BEFORE any dial — a mismatch never reaches the
/// network. Typed, never a panic: `NotFound` (never launched, or torn
/// down) and `Corrupt` (can't tell) are exactly the two cases ADR 0045
/// decision 2 already names for an absent/undetermined voyage lane.
fn check_voyage_ownership(state_dir: &Path, voyage_id: &str) -> Result<(), DialFail> {
    match sot_log::supervisor::journal::pointer::validate(state_dir) {
        sot_log::supervisor::journal::pointer::PointerState::Valid(current) if current == voyage_id => Ok(()),
        sot_log::supervisor::journal::pointer::PointerState::Valid(_) => Err(DialFail::VoyageMismatch),
        sot_log::supervisor::journal::pointer::PointerState::NotFound => Err(DialFail::Absent {
            kind: format!("{:?}", std::io::ErrorKind::NotFound),
            detail: "this row has no published voyage".into(),
        }),
        sot_log::supervisor::journal::pointer::PointerState::Corrupt => Err(DialFail::Undetermined),
        sot_log::supervisor::journal::pointer::PointerState::OtherIo(e) => Err(DialFail::Other(e.to_string())),
    }
}

/// The blocking dial: for the voyage lane, first proves ownership
/// (above); for the supervisor lane, resumes an absent lane in place on
/// a resumable row (ADR 0045 decision 2 — the daemon's dial IS the
/// recovery trigger). Then runs `authenticate_server` (steps 1-3 of the
/// challenge — no wire I/O; the daemon never decodes a lane frame).
/// BLOCKING throughout (`phase_of`/`query_status`/a process spawn inside
/// `ensure_started`, the pointer read, and the connect/authenticate
/// calls themselves): the caller runs this via `spawn_blocking`.
fn dial_and_authenticate(
    root: PathBuf,
    state_dir: PathBuf,
    workspace_id: String,
    agent_kind: String,
    agent_name: String,
    slug: String,
    project_root: PathBuf,
    lane: Lane,
    workspaces: Workspaces,
) -> Result<(<PlatformEndpoint as Endpoint>::Client, PeerAuthenticated), DialFail> {
    let h = state_dir_hash(&state_dir);
    let ep = PlatformEndpoint::default();

    let conn = match lane {
        Lane::Voyage(voyage_id) => {
            check_voyage_ownership(&state_dir, &voyage_id)?;
            // Decision 2: an absent VOYAGE endpoint is `lane_absent` at
            // once — the supervisor owns leg respawn, never this dial.
            match ep.connect_voyage_unchallenged(&h, &voyage_id) {
                Ok(c) => c,
                Err(e) if e.is_endpoint_absent() => {
                    return Err(DialFail::Absent { kind: absent_kind(&e), detail: "the voyage lane is not there".into() });
                }
                Err(e) => return Err(DialFail::Other(e.to_string())),
            }
        }
        Lane::Supervisor => match ep.connect_supervisor_unchallenged(&h) {
            Ok(c) => c,
            Err(e) if e.is_endpoint_absent() => {
                let is_terminal = workspaces
                    .resolve(Some(workspace_id.as_str()))
                    .map(|ws| ws.phase() == crate::rows::workspace::Phase::Terminal)
                    .unwrap_or(false);
                if is_terminal {
                    return Err(DialFail::Absent { kind: absent_kind(&e), detail: "the row is terminal".into() });
                }
                // `ensure_started` holds the row's own guard for its whole
                // duration, so a racing `lane.connect` waits for this same attempt.
                if let Err(detail) = crate::rows::run::activation::ensure_started(
                    &root,
                    &workspace_id,
                    &agent_kind,
                    &agent_name,
                    &slug,
                    &project_root,
                    crate::rows::run::activation::ActivationIntent::Reconnect,
                    workspaces.clone(),
                ) {
                    return Err(DialFail::Absent { kind: absent_kind(&e), detail: format!("resume failed: {detail}") });
                }
                match ep.connect_supervisor_unchallenged(&h) {
                    Ok(c) => c,
                    Err(e2) if e2.is_endpoint_absent() => {
                        return Err(DialFail::Absent {
                            kind: absent_kind(&e2),
                            detail: "resumed, but the supervisor lane still did not answer".into(),
                        });
                    }
                    Err(e2) => return Err(DialFail::Other(e2.to_string())),
                }
            }
            Err(e) => return Err(DialFail::Other(e.to_string())),
        },
    };

    match ep.authenticate_server(&conn) {
        PeerAuthOutcome::Authenticated(a) => Ok((conn, a)),
        PeerAuthOutcome::Foreign => Err(DialFail::Foreign),
        PeerAuthOutcome::Undetermined => Err(DialFail::Undetermined),
    }
}

/// Handle a connection whose frame behind its `handoff` hello was `lane.connect` (ADR 0045
/// decision 2). `rx` is the buffered reader that already consumed that
/// frame; `tx` is the write half; `frame` is the parsed handshake
/// frame; `workspaces` is the registry `lane.connect` resolves `target`
/// against. Returns when the pipe closes; errors are logged by the
/// caller (mirrors `proxy::handle_proxy_connect`).
pub(crate) async fn handle_lane_connect<R, W>(
    rx: R,
    mut tx: W,
    frame: Frame,
    workspaces: &Workspaces,
) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let id = frame.id;
    let req: LaneConnectReq = match serde_json::from_value(frame.payload) {
        Ok(r) => r,
        Err(e) => {
            return crate::server::pipe::reject(&mut tx, id, op::LANE_CONNECT, "bad_request", &format!("{e}")).await;
        }
    };

    let lane = match (req.lane.as_str(), req.voyage_id.clone()) {
        ("supervisor", _) => Lane::Supervisor,
        ("voyage", Some(voyage_id)) => Lane::Voyage(voyage_id),
        _ => {
            return crate::server::pipe::reject(&mut tx, id, op::LANE_CONNECT, "bad_lane", "lane must be \"supervisor\" or \"voyage\" (voyage requires voyage_id)").await;
        }
    };

    let Some(ws) = workspaces.workspace_for_tmux(&req.target) else {
        return crate::server::pipe::reject(&mut tx, id, op::LANE_CONNECT, "unknown_workspace", &format!("no workspace targets {:?}", req.target)).await;
    };

    let Some(root) = sot_log::host::state_dir::sot_state_dir() else {
        return crate::server::pipe::reject(
            &mut tx,
            id,
            op::LANE_CONNECT,
            "dial_failed",
            &format!("could not resolve this machine's state root ({} unset)", crate::rows::spawn::state_root::STATE_ROOT_HINT),
        )
        .await;
    };
    let state_dir = crate::rows::spawn::state_root::state_dir_for(&root, &ws.workspace_id);

    let workspace_id = ws.workspace_id.clone();
    let agent_kind = ws.agent();
    let agent_name = ws.agent_name();
    let slug = ws.slug.clone();
    let project_root = ws.project_root.clone();
    let workspaces_for_dial = workspaces.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        dial_and_authenticate(
            root,
            state_dir,
            workspace_id,
            agent_kind,
            agent_name,
            slug,
            project_root,
            lane,
            workspaces_for_dial,
        )
    })
    .await
    .unwrap_or_else(|join_err| Err(DialFail::Other(format!("lane dial task panicked: {join_err}"))));

    let (conn, peer) = match outcome {
        Ok(pair) => pair,
        Err(DialFail::Absent { kind, detail }) => {
            return reject_lane_absent(&mut tx, id, &kind, &detail).await;
        }
        Err(DialFail::VoyageMismatch) => {
            return crate::server::pipe::reject(&mut tx, id, op::LANE_CONNECT, "voyage_mismatch", "voyage_id is not this row's own current voyage").await;
        }
        Err(DialFail::Foreign) => {
            return crate::server::pipe::reject(&mut tx, id, op::LANE_CONNECT, "foreign", "the peer behind this lane failed identity authentication").await;
        }
        Err(DialFail::Undetermined) => {
            return crate::server::pipe::reject(&mut tx, id, op::LANE_CONNECT, "undetermined", "peer identity authentication could not be completed").await;
        }
        Err(DialFail::Other(detail)) => {
            return crate::server::pipe::reject(&mut tx, id, op::LANE_CONNECT, "dial_failed", &detail).await;
        }
    };

    let res = Frame::res(
        id,
        op::LANE_CONNECT,
        serde_json::json!({ "ok": true, "pid": peer.pid, "created": peer.created }),
    );
    codec::write_frame(&mut tx, &res, None).await?; // flushes internally

    let what = format!("target={} lane={}", req.target, req.lane);
    tracing::info!(target = %req.target, lane = %req.lane, pid = peer.pid, "lane.connect established — piping");

    let result = pipe_upstream(rx, tx, conn, &what).await;
    tracing::debug!(target = %req.target, lane = %req.lane, ?result, "lane.connect closed");
    result
}

/// Write the `lane_absent` refusal — the one code that carries an extra
/// field beyond `proxy::reject`'s `{error, code}` shape: `kind`, the
/// `io::ErrorKind` `Debug` text of the absence this dial (or the resume
/// attempt made on top of it, or the missing voyage pointer) actually
/// observed, so a caller can tell a never-started row (`NotFound`) from
/// a bound-but-unlistened one (`ConnectionRefused`) without re-deriving
/// it.
async fn reject_lane_absent<W>(tx: &mut W, id: u64, kind: &str, detail: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::json!({ "error": detail, "code": "lane_absent", "kind": kind });
    let f = Frame::res(id, op::LANE_CONNECT, payload);
    codec::write_frame(tx, &f, None).await // flushes internally
}

/// Convert the blocking client this dial produced into an async duplex
/// stream on the daemon's own Tokio runtime, then hand off to
/// [`crate::server::pipe::pipe_bidirectional`] — the SAME pipe body
/// `proxy.connect` uses, generic over the upstream type. `cfg(unix)`,
/// not `cfg(target_os = "linux")`: `socket_unix::SocketClient` is one
/// implementation for every Unix (`client.rs`'s own `PlatformEndpoint`
/// alias picks it for Linux and macOS alike), and a Unix stream adopted
/// from a raw fd is the same operation on both.
#[cfg(unix)]
async fn pipe_upstream<R, W>(rx: R, tx: W, conn: sot_log::lane::socket_unix::SocketClient, what: &str) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let std_stream = conn.into_stream();
    std_stream.set_nonblocking(true)?;
    let upstream = tokio::net::UnixStream::from_std(std_stream)?;
    crate::server::pipe::pipe_bidirectional(rx, tx, upstream, what).await
}

/// Windows twin of the Unix `pipe_upstream` above: the pipe handle was
/// opened `FILE_FLAG_OVERLAPPED` (`lane/pipe_win/`'s own connect), so it is
/// valid for `NamedPipeClient::from_raw_handle` to adopt — `unsafe` only
/// because that constructor trusts the caller's word that the handle is
/// a named pipe opened for overlapped I/O, which it is here by
/// construction.
#[cfg(windows)]
async fn pipe_upstream<R, W>(rx: R, tx: W, conn: sot_log::lane::pipe_win::PipeClient, what: &str) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    use std::os::windows::io::IntoRawHandle;
    let owned = conn.into_handle();
    let upstream = unsafe { tokio::net::windows::named_pipe::NamedPipeClient::from_raw_handle(owned.into_raw_handle())? };
    crate::server::pipe::pipe_bidirectional(rx, tx, upstream, what).await
}
