#![cfg(any(windows, target_os = "linux"))]
//! lane_bridge.rs — ADR 0045 decision 2: `lane.connect`, the daemon-side
//! half of "one attach path: through the daemon, local or remote"
//! (decision 1). A dedicated connection whose first frame is
//! `lane.connect` becomes, after one reply, a raw byte pipe onto the
//! capsule row's supervisor or voyage lane — the `proxy.connect`
//! mechanism (ADR 0035), verbatim, for lane endpoints instead of a
//! loopback port. The daemon's dial is also the recovery trigger: an
//! absent supervisor lane on a resumable row is resumed
//! (`resume_if_absent`, decision 33 of the lifecycle track) before the
//! dial is answered. Once the pipe starts, the daemon never decodes a
//! lane frame again — the frontend runs its own end-to-end `hello`
//! (steps 4-5 of the challenge) over the pipe, bound against the `pid`/
//! `created` this module reports from ITS OWN dial (steps 1-3).
//!
//! Capsule-runtime-gated exactly like `capsule_workspace.rs`'s own `mod
//! runtime`: this whole file, and `server.rs`'s peek call into it, are
//! `#[cfg(any(windows, target_os = "linux"))]` — a host with no capsule
//! runtime at all (macOS) never sees this module; `lane.connect` there
//! falls into the ordinary control loop and gets whatever "unknown op"
//! answer every unrouted op already does, exactly as every other
//! capsule-only surface behaves on that platform.

use std::path::{Path, PathBuf};

use anyhow::Result;
use sot_log::challenge::{PeerAuthOutcome, PeerAuthenticated};
use sot_log::client::{Endpoint, PlatformEndpoint};
use sot_log::state_dir::state_dir_hash;
use sot_log::transport::TransportError;
use sot_protocol::{codec, op, Frame, LaneConnectReq};
use tokio::io::{AsyncBufRead, AsyncWrite};

use crate::workspaces::Workspaces;

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
    match sot_log::pointer::validate(state_dir) {
        sot_log::pointer::PointerState::Valid(current) if current == voyage_id => Ok(()),
        sot_log::pointer::PointerState::Valid(_) => Err(DialFail::VoyageMismatch),
        sot_log::pointer::PointerState::NotFound => Err(DialFail::Absent {
            kind: format!("{:?}", std::io::ErrorKind::NotFound),
            detail: "this row has no published voyage".into(),
        }),
        sot_log::pointer::PointerState::Corrupt => Err(DialFail::Undetermined),
        sot_log::pointer::PointerState::OtherIo(e) => Err(DialFail::Other(e.to_string())),
    }
}

/// The blocking dial: for the voyage lane, first proves ownership
/// (above); for the supervisor lane, resumes an absent lane in place on
/// a resumable row (ADR 0045 decision 2 — the daemon's dial IS the
/// recovery trigger). Then runs `authenticate_server` (steps 1-3 of the
/// challenge — no wire I/O; the daemon never decodes a lane frame).
/// BLOCKING throughout (`phase_of`/`query_status`/a process spawn inside
/// `resume_if_absent`, the pointer read, and the connect/authenticate
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
                if workspaces.is_capsule_terminal(&workspace_id) {
                    return Err(DialFail::Absent { kind: absent_kind(&e), detail: "the row is terminal".into() });
                }
                // ADR 0043 decision 33: resume-only, never `reset`, one
                // launch in flight per row under the per-row guard —
                // `resume_if_absent` takes that guard itself for its
                // whole duration, so a second `lane.connect` racing this
                // one simply waits for the SAME resume rather than
                // spawning a second authority.
                if let Err(detail) = crate::capsule_workspace::resume_if_absent(
                    &root,
                    &workspace_id,
                    &agent_kind,
                    &agent_name,
                    &slug,
                    &project_root,
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

/// Handle a connection whose first frame was `lane.connect` (ADR 0045
/// decision 2). `rx` is the buffered reader that already consumed that
/// first frame; `tx` is the write half; `frame` is the parsed handshake
/// frame; `expected_token` is the daemon's configured token (if any);
/// `workspaces` is the registry `lane.connect` resolves `target`
/// against. Returns when the pipe closes; errors are logged by the
/// caller (mirrors `proxy::handle_proxy_connect`).
pub(crate) async fn handle_lane_connect<R, W>(
    rx: R,
    mut tx: W,
    frame: Frame,
    expected_token: Option<&str>,
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
            return crate::proxy::reject(&mut tx, id, op::LANE_CONNECT, "bad_request", &format!("{e}")).await;
        }
    };

    // Auth mirrors `proxy.connect`'s own gate (`proxy.rs`'s
    // `handle_proxy_connect`): honored only when the daemon has a token
    // configured, and BEFORE any row lookup; the normal local Unix-socket
    // transport has none — filesystem permissions on the socket are the
    // trust boundary.
    if let Some(expected) = expected_token {
        let presented = req.token.clone().unwrap_or_default();
        if !crate::handlers::constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            tracing::warn!(target = %req.target, lane = %req.lane, "lane.connect rejected: bad token");
            return crate::proxy::reject(&mut tx, id, op::LANE_CONNECT, "unauthenticated", "bad or missing token").await;
        }
    }

    let lane = match (req.lane.as_str(), req.voyage_id.clone()) {
        ("supervisor", _) => Lane::Supervisor,
        ("voyage", Some(voyage_id)) => Lane::Voyage(voyage_id),
        _ => {
            return crate::proxy::reject(&mut tx, id, op::LANE_CONNECT, "bad_lane", "lane must be \"supervisor\" or \"voyage\" (voyage requires voyage_id)").await;
        }
    };

    let Some(ws) = workspaces.workspace_for_tmux(&req.target) else {
        return crate::proxy::reject(&mut tx, id, op::LANE_CONNECT, "unknown_workspace", &format!("no workspace targets {:?}", req.target)).await;
    };
    if ws.runtime != "capsule" {
        return crate::proxy::reject(&mut tx, id, op::LANE_CONNECT, "not_capsule", "this row has no lane to bridge (runtime is not capsule)").await;
    }

    let Some(root) = sot_log::state_dir::sot_state_dir() else {
        return crate::proxy::reject(
            &mut tx,
            id,
            op::LANE_CONNECT,
            "dial_failed",
            &format!("could not resolve this machine's state root ({} unset)", crate::capsule_workspace::STATE_ROOT_HINT),
        )
        .await;
    };
    let state_dir = crate::capsule_workspace::state_dir_for(&root, &ws.workspace_id);

    let workspace_id = ws.workspace_id.clone();
    let agent_kind = ws.agent.clone();
    let agent_name = ws.agent_name.clone();
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
            return crate::proxy::reject(&mut tx, id, op::LANE_CONNECT, "voyage_mismatch", "voyage_id is not this row's own current voyage").await;
        }
        Err(DialFail::Foreign) => {
            return crate::proxy::reject(&mut tx, id, op::LANE_CONNECT, "foreign", "the peer behind this lane failed identity authentication").await;
        }
        Err(DialFail::Undetermined) => {
            return crate::proxy::reject(&mut tx, id, op::LANE_CONNECT, "undetermined", "peer identity authentication could not be completed").await;
        }
        Err(DialFail::Other(detail)) => {
            return crate::proxy::reject(&mut tx, id, op::LANE_CONNECT, "dial_failed", &detail).await;
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
/// [`crate::proxy::pipe_bidirectional`] — the SAME pipe body
/// `proxy.connect` uses, generic over the upstream type.
#[cfg(target_os = "linux")]
async fn pipe_upstream<R, W>(rx: R, tx: W, conn: sot_log::socket_unix::SocketClient, what: &str) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let std_stream = conn.into_stream();
    std_stream.set_nonblocking(true)?;
    let upstream = tokio::net::UnixStream::from_std(std_stream)?;
    crate::proxy::pipe_bidirectional(rx, tx, upstream, what).await
}

/// Windows twin of the Linux `pipe_upstream` above: the pipe handle was
/// opened `FILE_FLAG_OVERLAPPED` (`pipe_win.rs`'s own connect), so it is
/// valid for `NamedPipeClient::from_raw_handle` to adopt — `unsafe` only
/// because that constructor trusts the caller's word that the handle is
/// a named pipe opened for overlapped I/O, which it is here by
/// construction.
#[cfg(windows)]
async fn pipe_upstream<R, W>(rx: R, tx: W, conn: sot_log::pipe_win::PipeClient, what: &str) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    use std::os::windows::io::IntoRawHandle;
    let owned = conn.into_handle();
    let upstream = unsafe { tokio::net::windows::named_pipe::NamedPipeClient::from_raw_handle(owned.into_raw_handle())? };
    crate::proxy::pipe_bidirectional(rx, tx, upstream, what).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::Workspace;

    /// A registry with exactly one CAPSULE row, named `tmux_session` —
    /// no real state dir, no real supervisor: these tests never reach
    /// past the token gate (a known target with the WRONG token still
    /// refuses), so nothing here needs to dial anything.
    fn workspaces_with_one_capsule_row(tmux_session: &str) -> Workspaces {
        let workspaces = Workspaces::default();
        let mut ws = Workspace::from_label("token-gate-test", std::env::temp_dir(), false, "none".to_string(), String::new(), String::new());
        ws.runtime = "capsule".to_string();
        ws.tmux_session = tmux_session.to_string();
        workspaces.insert(ws);
        workspaces
    }

    fn connect_frame(target: &str, token: Option<&str>) -> Frame {
        let mut payload = serde_json::json!({ "target": target, "lane": "supervisor" });
        if let Some(t) = token {
            payload["token"] = serde_json::json!(t);
        }
        Frame::req(1, op::LANE_CONNECT, payload)
    }

    async fn read_res<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> serde_json::Value {
        let mut buf = codec::buffered(r);
        let (f, _) = codec::read_frame(&mut buf).await.unwrap();
        f.payload
    }

    /// Codex review SHOULD-FIX (2026-09-11): the printed-SKIPPED
    /// integration test (no `Env` mechanism ever starts a real `sotd`
    /// with a token) is replaced by calling the HANDLER directly with
    /// `expected_token: Some(..)` configured — an existing AND an
    /// unknown target, each with a missing and a wrong token, all four
    /// refuse `unauthenticated` — proving the gate runs BEFORE any row
    /// lookup (the unknown-target cases never surface `unknown_workspace`
    /// instead). A fifth case (existing target, the RIGHT token) proves
    /// the gate is not simply unconditional: it passes through to the
    /// dial attempt, which this in-memory `Workspaces` (no real state
    /// dir) then fails with `dial_failed` — any code OTHER than
    /// `unauthenticated` is enough to prove the token was accepted.
    #[tokio::test]
    async fn token_gate_runs_before_any_row_lookup() {
        let workspaces = workspaces_with_one_capsule_row("sot-be-known-row");

        for target in ["sot-be-known-row", "sot-be-no-such-row"] {
            for token in [None, Some("wrong")] {
                let (client, daemon) = tokio::io::duplex(4096);
                let (dr, dw) = tokio::io::split(daemon);
                let (mut cr, _cw) = tokio::io::split(client);
                let daemon_fut =
                    handle_lane_connect(codec::buffered(dr), dw, connect_frame(target, token), Some("secret"), &workspaces);
                let client_fut = async {
                    let res = read_res(&mut cr).await;
                    assert_eq!(
                        res.get("code").and_then(|v| v.as_str()),
                        Some("unauthenticated"),
                        "target {target:?} token {token:?}: {res:?}"
                    );
                };
                let (dres, _) = tokio::join!(daemon_fut, client_fut);
                dres.unwrap();
            }
        }

        // The right token passes the gate: some code OTHER than
        // `unauthenticated` follows (this in-memory registry has no
        // real state dir to dial, so `dial_failed` is what it settles
        // on — the point is only that it is not the auth refusal).
        let (client, daemon) = tokio::io::duplex(4096);
        let (dr, dw) = tokio::io::split(daemon);
        let (mut cr, _cw) = tokio::io::split(client);
        let daemon_fut = handle_lane_connect(
            codec::buffered(dr),
            dw,
            connect_frame("sot-be-known-row", Some("secret")),
            Some("secret"),
            &workspaces,
        );
        let client_fut = async {
            let res = read_res(&mut cr).await;
            assert_ne!(
                res.get("code").and_then(|v| v.as_str()),
                Some("unauthenticated"),
                "the right token must pass the gate: {res:?}"
            );
        };
        let (dres, _) = tokio::join!(daemon_fut, client_fut);
        dres.unwrap();
    }
}
