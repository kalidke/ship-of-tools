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
//! Capsule-runtime-gated, like `capsule_workspace.rs`'s own `mod
//! runtime`: the dial logic lives in `imp`, `#[cfg(any(windows,
//! target_os = "linux"))]`. `handle_lane_connect` itself is ungated (so
//! `server.rs`'s peek compiles on every host) — parsing and the token
//! check run everywhere; a host with no capsule runtime at all (macOS)
//! answers `runtime_not_available` without ever touching a state dir.

use anyhow::Result;
use sot_protocol::{op, Frame, LaneConnectReq};
// `codec`/`AsyncWriteExt` are used only by `reject_lane_absent` below,
// which is itself gated to the capsule runtime's own availability
// (`#[cfg(any(windows, target_os = "linux"))]`) — an unguarded import
// here would warn unused on any other host (macOS).
#[cfg(any(windows, target_os = "linux"))]
use sot_protocol::codec;
use tokio::io::{AsyncBufRead, AsyncWrite};
#[cfg(any(windows, target_os = "linux"))]
use tokio::io::AsyncWriteExt;

use crate::workspaces::Workspaces;

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
    let req: LaneConnectReq = match serde_json::from_value(frame.payload) {
        Ok(r) => r,
        Err(e) => {
            return crate::proxy::reject(&mut tx, frame.id, op::LANE_CONNECT, "bad_request", &format!("{e}")).await;
        }
    };

    // Auth mirrors `proxy.connect`'s own gate (`proxy.rs`'s
    // `handle_proxy_connect`): honored only when the daemon has a token
    // configured; the normal local Unix-socket transport has none —
    // filesystem permissions on the socket are the trust boundary.
    if let Some(expected) = expected_token {
        let presented = req.token.clone().unwrap_or_default();
        if !crate::handlers::constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            tracing::warn!(target = %req.target, lane = %req.lane, "lane.connect rejected: bad token");
            return crate::proxy::reject(&mut tx, frame.id, op::LANE_CONNECT, "unauthenticated", "bad or missing token").await;
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    {
        imp::handle_lane_connect(rx, tx, frame.id, req, workspaces).await
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (rx, workspaces, req);
        crate::proxy::reject(
            &mut tx,
            frame.id,
            op::LANE_CONNECT,
            "runtime_not_available",
            "the capsule runtime is not available on this host",
        )
        .await
    }
}

/// Write the `lane_absent` refusal — the one code that carries an extra
/// field beyond `proxy::reject`'s `{error, code}` shape: `kind`, the
/// `io::ErrorKind` `Debug` text of the absence this dial (or the resume
/// attempt made on top of it) actually observed, so a caller can tell a
/// never-started row (`NotFound`) from a bound-but-unlistened one
/// (`ConnectionRefused`) without re-deriving it.
#[cfg(any(windows, target_os = "linux"))]
async fn reject_lane_absent<W>(tx: &mut W, id: u64, kind: &str, detail: &str) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::json!({ "error": detail, "code": "lane_absent", "kind": kind });
    let f = Frame::res(id, op::LANE_CONNECT, payload);
    codec::write_frame(tx, &f, None).await?;
    tx.flush().await?;
    Ok(())
}

#[cfg(any(windows, target_os = "linux"))]
mod imp {
    use std::path::PathBuf;

    use anyhow::Result;
    use sot_log::challenge::PeerAuthOutcome;
    use sot_log::client::{Endpoint, PlatformEndpoint};
    use sot_log::state_dir::state_dir_hash;
    use sot_log::transport::TransportError;
    use sot_protocol::{codec, op, Frame, LaneConnectReq};
    use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};

    use crate::workspaces::Workspaces;

    /// The two lanes `lane.connect` can name — parsed once, up front, so
    /// every later match is exhaustive over two variants rather than
    /// re-testing a string.
    enum Lane {
        Supervisor,
        Voyage,
    }

    /// Why the blocking dial+resume+authenticate step (below) could not
    /// hand back a piped connection. Kept separate from `TransportError`
    /// itself (rather than reusing it directly) because two of these
    /// variants — `Foreign`/`Undetermined` — come from
    /// [`Endpoint::authenticate_server`]'s own [`PeerAuthOutcome`], not a
    /// `TransportError` at all, and `Absent`'s `kind` is a derived
    /// `io::ErrorKind` string, not the error itself.
    enum DialFail {
        /// The endpoint is not there right now — for the supervisor
        /// lane, only after a resume was tried (or refused because the
        /// row is terminal). `kind` is the observing error's own
        /// `io::ErrorKind` `Debug` text.
        Absent { kind: String, detail: String },
        Foreign,
        Undetermined,
        /// Any other I/O error dialing or authenticating.
        Other(String),
    }

    /// `TransportError::is_endpoint_absent()`'s own `io::ErrorKind` —
    /// only ever called on an error already known to satisfy that
    /// predicate (a Windows `NotFound` or a Linux `ConnectionRefused`/
    /// `NotFound`), so the `Io` arm is the only reachable one; the
    /// fallback exists only so this stays total.
    fn absent_kind(e: &TransportError) -> String {
        if let TransportError::Io { source, .. } = e {
            format!("{:?}", source.kind())
        } else {
            format!("{e:?}")
        }
    }

    /// The blocking dial: connect the named lane, resuming an absent
    /// SUPERVISOR lane in place on a resumable row (ADR 0045 decision 2
    /// — the daemon's dial IS the recovery trigger), then run
    /// `authenticate_server` (steps 1-3 of the challenge — no wire I/O;
    /// the daemon never decodes a lane frame). BLOCKING throughout
    /// (`phase_of`/`query_status`/a process spawn inside `resume_if_
    /// absent`, and the connect/authenticate calls themselves): the
    /// caller runs this via `spawn_blocking`.
    fn dial_and_authenticate(
        root: PathBuf,
        state_dir: PathBuf,
        workspace_id: String,
        agent_kind: String,
        agent_name: String,
        slug: String,
        project_root: PathBuf,
        lane: Lane,
        voyage_id: Option<String>,
        workspaces: Workspaces,
    ) -> Result<(<PlatformEndpoint as Endpoint>::Client, sot_log::challenge::PeerAuthenticated), DialFail> {
        let h = state_dir_hash(&state_dir);
        let ep = PlatformEndpoint::default();

        let conn = match lane {
            Lane::Voyage => {
                // Decision 2: an absent VOYAGE endpoint is `lane_absent`
                // at once — the supervisor owns leg respawn, never this
                // dial.
                let voyage_id = voyage_id.expect("bad_lane already refused a voyage dial with no id");
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
                    // ADR 0043 decision 33: resume-only, never `reset`,
                    // one launch in flight per row under the per-row
                    // guard — `resume_if_absent` takes that guard
                    // itself for its whole duration, so a second
                    // `lane.connect` racing this one simply waits for
                    // the SAME resume rather than spawning a second
                    // authority.
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

    pub(super) async fn handle_lane_connect<R, W>(
        rx: R,
        mut tx: W,
        id: u64,
        req: LaneConnectReq,
        workspaces: &Workspaces,
    ) -> Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let lane = match req.lane.as_str() {
            "supervisor" => Lane::Supervisor,
            "voyage" => Lane::Voyage,
            _ => {
                return crate::proxy::reject(&mut tx, id, op::LANE_CONNECT, "bad_lane", "lane must be \"supervisor\" or \"voyage\"").await;
            }
        };
        if matches!(lane, Lane::Voyage) && req.voyage_id.is_none() {
            return crate::proxy::reject(&mut tx, id, op::LANE_CONNECT, "bad_lane", "the voyage lane requires voyage_id").await;
        }

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
        let voyage_id = req.voyage_id.clone();
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
                voyage_id,
                workspaces_for_dial,
            )
        })
        .await
        .unwrap_or_else(|join_err| Err(DialFail::Other(format!("lane dial task panicked: {join_err}"))));

        let (conn, peer) = match outcome {
            Ok(pair) => pair,
            Err(DialFail::Absent { kind, detail }) => {
                return super::reject_lane_absent(&mut tx, id, &kind, &detail).await;
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
        codec::write_frame(&mut tx, &res, None).await?;
        tx.flush().await?;

        let what = format!("target={} lane={}", req.target, req.lane);
        tracing::info!(target = %req.target, lane = %req.lane, pid = peer.pid, "lane.connect established — piping");

        let result = pipe_upstream(rx, tx, conn, &what).await;
        tracing::debug!(target = %req.target, lane = %req.lane, ?result, "lane.connect closed");
        result
    }

    /// Convert the blocking client this dial produced into an async
    /// duplex stream on the daemon's own Tokio runtime, then hand off to
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

    /// Windows twin of the Linux `pipe_upstream` above: the pipe handle
    /// was opened `FILE_FLAG_OVERLAPPED` (`pipe_win.rs`'s own connect),
    /// so it is valid for `NamedPipeClient::from_raw_handle` to adopt —
    /// `unsafe` only because that constructor trusts the caller's word
    /// that the handle is a named pipe opened for overlapped I/O, which
    /// it is here by construction.
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
}
