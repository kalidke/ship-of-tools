// net/transport — local frontend ↔ backend over a local socket (Unix
// socket / Windows named pipe), or over channel 0 of the host's link.
//
// Per ADR 0010, as replaced by isolation-plan.md §3 C3 (amended by
// dev/output/c3-second-connection-amendment.md):
//   - Reaching a daemon that is not on this box means the host's link
//     (`sot_topology::link`): one `ssh <target> '<PATH prelude>; sotd
//     stdio-bridge [--host <host>]'` login, or one connection to the hub's
//     relay socket, whose far `sotd stdio-bridge` routes the link's channels
//     to its daemon — never a port, on either box: the daemon has had no TCP
//     listener since 0.4.0. The control connection is channel 0; the host's
//     pages and rows open channels on the same link. A dead login or a dead
//     `sotd` on the far end is the link's start failing; the login's last
//     stderr line IS the diagnosis (no per-cause exit codes to invent).
//   - Connect handshake carries (session_id, client_id, last_seen_revision);
//     backend either replays missed events or sends a snapshot on reconnect.
//
// Transport selection: `spawn` takes a pipe, an ssh recipe or a relay path. The
// protocol code is generic over `AsyncRead` / `AsyncWrite` so it runs
// identically on a local socket and on a link's channel 0.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender as StdSender;
use std::sync::Arc;

use crate::net::dial::HostKey;
use anyhow::{Context, Result};
use base64::Engine;
use interprocess::local_socket::tokio::prelude::*;
use sot_topology::link::{Link, LinkRoute, LinkStartError, LocalStream};
use serde_json::Value;
use sot_protocol::{
    codec, op, AgentSendReq, ConceptReadReq, ConceptReadRes, ConceptWriteReq, ConceptWriteRes,
    DirCreateReq, DirCreateRes, DocsOpenReq, DocsOpenRes, FePresenceReq, FileChunk, FileDeleteReq, FileDeleteRes,
    FileDownloadReq, FileReadReq, FileReadRes, FileUploadAck, FileUploadReq, FileWriteReq,
    FileWriteRes, Frame,
    HelloReq, HelloRes, ImageCropReq, ImageCropRes, KernelRequestReq, MathRenderReq, MathRenderRes,
    MonitorHistoryReq, MonitorHistoryRes, MonitorSubscribeRes, MonitorTickEvt, PageReplyReq, PageReplyRes, PingReq, PlutoOpenReq,
    PlutoOpenRes, PreviewGetReq, PreviewGetRes, PtyOpenReq,
    QuartoOpenReq, ReplEvalReq, ReplEvalRes, ReplFrame, ReplFrameEvt, ReplRunFileReq,
    ReplRunFileRes, ToggleHiddenReq, TreeChildrenReq, TreeChildrenRes, TreeNode, TreeRootReq,
    TreeRootRes, VideoOpenReq, VideoOpenRes, WorkspaceActivateReq, WorkspaceListReq,
    WorkspaceListRes,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{self as tmpsc, UnboundedReceiver, UnboundedSender};

mod event;
mod hello;
mod ops;
mod preamble;
mod reply;
mod request;
mod steady;

#[cfg(all(test, feature = "test-window-progress"))]
pub(crate) use reply::observe_native_fan_in;

use crate::net::state::{note_revision, SessionState, StateSaveGate};
use hello::{accept_hello, read_hello, send_hello, HelloRefused};
use ops::*;
use preamble::{preamble_preview, preamble_tree_root};
use reply::{eval_running, handle_response_frame, PendingGuard, PendingKind};
use request::send_request;

// The transport's interface: what code outside this folder names.
pub(crate) use self::{
    hello::read_connect_reply,
    event::{IncomingEvt, ResultTreeReply},
    ops::{
        AccountInfo, ConceptWriteResult, DefinitionInfo, DirCreateResult, DirEntry,
        FileDeleteResult, FileWriteResult, MarkdownToken, MethodInfo, ReplRunFileInfo,
        ScanModule, ScanType, WorkspaceCreatedInfo, WorkspaceDestroyedInfo, WorkspaceInfo,
    },
    request::{OutgoingReq, ResultAttemptId, ResultTreeRequest},
};

#[cfg(test)]
pub(crate) struct ResultTreeTestDriver<'a> {
    pending: PendingGuard<'a>,
}

#[cfg(test)]
impl<'a> ResultTreeTestDriver<'a> {
    pub(crate) fn new(host: HostKey, evt_tx: &'a StdSender<(HostKey, IncomingEvt)>) -> Self {
        Self {
            pending: PendingGuard {
                map: HashMap::new(),
                evt_tx,
                host,
            },
        }
    }

    pub(crate) async fn send<W: AsyncWrite + Unpin>(
        &mut self,
        writer: W,
        id: u64,
        request: OutgoingReq,
    ) -> Result<()> {
        send_request(writer, &mut self.pending, id, request).await
    }

    pub(crate) fn reply(&mut self, frame: Frame) {
        let host = self.pending.host.clone();
        let evt_tx = self.pending.evt_tx;
        handle_response_frame(frame, None, &mut self.pending, evt_tx, &host);
    }
}

/// What the transport task should dial: a local socket/named pipe, or the
/// host's link over an ssh login or a hub relay (C3). Exactly one, never neither and never both —
/// replaces the former `pipe: Option<PathBuf>` / `ssh: Option<SshRecipe>`
/// pair, whose "at least one must be set" was a doc caveat callers had to
/// honor by convention (the CLI never actually built one with neither set;
/// the only construction with both was a `#[cfg(test)]` state the CLI
/// cannot produce) rather than a fact the type itself enforced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dial {
    Pipe(PathBuf),
    /// A generated hub relay, remote for policy: the host's link over the protected local connector.
    Relay(PathBuf),
    Ssh(sot_topology::ssh_bridge::SshRecipe),
}

#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub dial: Dial,
    pub token: Option<String>,
}

/// ADR 0045 decision 1 (Codex review, lane B5 discharge); reshaped by C3 as
/// amended: which transport a host's CONTROL connection actually resolved
/// to — `Local` (the pipe/socket connected), or the route of the host's link
/// (`Ssh` with the exact recipe its login ran, `Relay` with the relay path). Recorded from every
/// `Connected` evt (`State::host_resolved_dial`) so `spawn_pane_attach_term`
/// dials the SAME endpoint the control connection is already talking to,
/// rather than an independent preference guess that could reach a
/// DIFFERENT daemon than the one actually running this host.
///
/// Lives here, beside `TransportConfig`, because `IncomingEvt::Connected`
/// carries it — moved out of `ui/mod.rs`, which names it
/// `crate::net::transport::ResolvedDial`. No longer `Copy` (`SshRecipe` isn't):
/// every former `.copied()` reader became `.cloned()` (the amendment's own
/// site list, C3's commit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedDial {
    Local,
    /// The exact generated relay path selected by the control connection; never a local lease target.
    Relay(PathBuf),
    Ssh(sot_topology::ssh_bridge::SshRecipe),
}

/// Create the outgoing-request channel paired with the transport task. The
/// sender lives on the GPU thread; the receiver gets handed to `spawn`. Both
/// sides drop their handle on shutdown — that's how the writer half of the
/// select loop terminates.
pub fn outgoing_channel() -> (UnboundedSender<OutgoingReq>, UnboundedReceiver<OutgoingReq>) {
    tmpsc::unbounded_channel()
}

/// The reconnect wait for `dial`: from 200 ms, doubling to a cap that depends on the dial, and back to 200 ms only
/// after a session that lasted `STABLE`. A local socket costs nothing to probe, so it keeps 5 s. Each start of a
/// remote host's link is a login on the hub plus one on the far host, so the cap is 30 s: once the wait reaches it, a
/// down remote host costs the hub at most two ssh logins a minute per frontend (about nine in the first minute, while
/// the wait doubles), and that is every login the window makes to it: its pages and rows are channels on the link.
fn redial_for(dial: &Dial) -> sot_platform::host::redial::Redial {
    let cap = match dial {
        Dial::Pipe(_) => std::time::Duration::from_secs(5),
        // Connecting to a relay socket always succeeds; the hub then opens an ssh login to the far host.
        Dial::Relay(_) | Dial::Ssh(_) => std::time::Duration::from_secs(30),
    };
    sot_platform::host::redial::Redial::new(std::time::Duration::from_millis(200), cap)
}

/// Spawn the transport task on `rt`. Returns once spawned; the task runs
/// until the connection drops or the runtime shuts down. The task asks the
/// window to redraw whenever a new IncomingEvt is published so the GPU
/// thread sees state updates without polling.
pub(crate) fn spawn<Wn: Redraw + Clone + Send + Sync + 'static>(
    rt: &tokio::runtime::Runtime,
    host: HostKey,
    config: TransportConfig,
    evt_tx: StdSender<(HostKey, IncomingEvt)>,
    out_rx: UnboundedReceiver<OutgoingReq>,
    window: Wn,
    reconnect_now: Arc<tokio::sync::Notify>,
    link: sot_topology::link::LinkHandle,
    leases: Arc<crate::lease::Leases>,
) {
    rt.spawn(async move {
        // Reconnect loop with exponential backoff, capped at 5s on a local
        // socket and 30s on an ssh dial (each ssh probe is a login on the
        // hub and another on the far host). The
        // out_rx channel survives across attempts; any OutgoingReq the
        // user queued while disconnected gets sent once the next
        // connection is up. Per ADR 0010 the backend's session-id +
        // last-seen-revision handshake on each connect carries the
        // resume protocol, so missed events replay automatically.
        //
        // We never give up — the user can quit the window to terminate
        // the task. The wait starts over only after a session that lasted
        // `STABLE` (`redial_for`), or at F5: an answered hello alone does not prove a
        // working connection, so a daemon that answers and drops keeps
        // the wait doubling. The F5 `reconnect_now` notify lets the user
        // collapse the current sleep — useful when wifi flickers and
        // the user knows it's back before the wait elapses.
        let mut out_rx = out_rx;
        let mut redial = redial_for(&config.dial);
        loop {
            let began = tokio::time::Instant::now();
            match connect_and_run(
                host.clone(),
                config.clone(),
                evt_tx.clone(),
                &mut out_rx,
                window.clone(),
                &link,
                &leases,
            )
            .await
            {
                Ok(()) => {
                    tracing::info!(%host, "transport task exited cleanly");
                    return;
                }
                Err(e) => {
                    let wait = redial.after(began.elapsed());
                    let wait_ms = wait.as_millis();
                    tracing::warn!(
                        %host,
                        error = %format_args!("{e:#}"),
                        wait_ms,
                        "transport task ended; reconnecting"
                    );
                    let _ = evt_tx.send((
                        host.clone(),
                        IncomingEvt::Disconnected {
                            reason: format!("{e:#} — retry in {wait_ms}ms (F5 to retry now)"),
                            refused: e.is::<HelloRefused>(),
                        },
                    ));
                    window.request_redraw();
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = reconnect_now.notified() => {
                            tracing::info!("manual reconnect requested — collapsing backoff");
                            redial.reset();
                        }
                    }
                }
            }
        }
    });
}

/// Dial whichever transport `config.dial` names. Once a connection is
/// established we hand off to `run_protocol`; any error from there is
/// *not* retried via the other transport — that's a runtime disconnect,
/// not a startup-time choose-your-transport decision. There is no
/// fallback between the two arms: pre-C3 both could be configured and a
/// pipe failure fell through to ssh, but `Dial` makes that unconstructible
/// now, so each arm either connects and hands off or returns its own
/// `Err`.
async fn connect_and_run<Wn: Redraw>(
    host: HostKey,
    config: TransportConfig,
    evt_tx: StdSender<(HostKey, IncomingEvt)>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: Wn,
    link: &sot_topology::link::LinkHandle,
    leases: &crate::lease::Leases,
) -> Result<()> {
    match &config.dial {
        Dial::Pipe(pipe_path) => {
            // A sent lease request is never dropped (its end would read as this window closing, ADR 0050): past its
            // bound the wait is named on the status line, and the host connects when the daemon answers.
            let unanswered = |waited: std::time::Duration| {
                let reason =
                    format!("this computer's backend has not answered the window lease in {waited:?}; waiting for its answer");
                let _ = evt_tx.send((host.clone(), IncomingEvt::Disconnected { reason, refused: false }));
                window.request_redraw();
            };
            let not_ended = leases
                .before_data_connection(&host, pipe_path, config.token.as_deref(), unanswered)
                .await?;
            // The grant recorded the count (`Leases::owed`); the next frame shows it.
            if not_ended > 0 {
                window.request_redraw();
            }
            let stream = connect_pipe(pipe_path).await?;
            // Pre-hello: the daemon hasn't declared its host yet, so
            // `host` here is only this connection's DIAL key, not a
            // claim about identity (ADR 0046 decision 1) — label it
            // plainly so it's never misread as the declared value the
            // later `"connected"` line's `declared` field carries.
            tracing::info!(dial = %host, ?pipe_path, "connected via local socket");
            let (rx, tx) = stream.split();
            let rx = codec::buffered(rx);
            run_protocol(
                host,
                rx,
                tx,
                config.token.as_deref(),
                &evt_tx,
                out_rx,
                &window,
                ResolvedDial::Local,
                None,
            )
            .await
        }
        Dial::Relay(_) | Dial::Ssh(_) => {
            let (route, resolved) = match &config.dial {
                Dial::Relay(path) => (LinkRoute::Relay(path.clone()), ResolvedDial::Relay(path.clone())),
                Dial::Ssh(recipe) => (LinkRoute::Ssh(recipe.clone()), ResolvedDial::Ssh(recipe.clone())),
                Dial::Pipe(_) => unreachable!("matched above"),
            };
            // The window's one login to this computer, or its one connection to the hub's relay socket for it: the
            // control session, the host's pages and its rows are channels on it. No window lease: the daemon is remote.
            #[allow(clippy::disallowed_methods, reason = "the window's one login per computer, its link, owned by this task")]
            let started = Link::start(&route).await;
            let mut started = match started {
                Ok(started) => started,
                Err(LinkStartError::Refused { code, message }) => {
                    // A far end the person must update (a bridge from before the link, another link number): a refusal,
                    // shown as a hello refusal is.
                    tracing::error!(code, "link refused: {message}");
                    let _ = evt_tx.send((host.clone(), IncomingEvt::HelloRefused { message: message.clone() }));
                    window.request_redraw();
                    return Err(HelloRefused(format!("link refused: {message} (code={code})")).into());
                }
                Err(e) => return Err(anyhow::Error::new(e).context(format!("link {route}"))),
            };
            // Pre-hello, same labeling rule as the pipe branch above.
            tracing::info!(dial = %host, %route, build = started.build(), "connected via the host's link");
            let control = started.take_control().expect("a new link's channel 0");
            let (rx, tx) = control.into_split();
            // The host's handle goes down inside `run_protocol`'s own wrapper, before the stderr wait below, so no
            // channel opens while that wait runs.
            let result = run_protocol(
                host,
                codec::buffered(rx),
                tx,
                config.token.as_deref(),
                &evt_tx,
                out_rx,
                &window,
                resolved,
                Some((link, &started)),
            )
            .await;
            // The link drops when this arm returns, ending its login (`kill_on_drop`) and every channel on it before
            // the reconnect loop's next attempt starts a fresh one.
            if let Err(e) = &result {
                if let Some(line) = started.last_stderr().await {
                    return Err(anyhow::anyhow!("{e:#} (ssh: {line})"));
                }
            }
            result
        }
    }
}

/// Connect through `connect_own` on a blocking thread, then adopt the returned socket or pipe as the window's stream
/// (`sot_topology::link::connect_local`, which the far router's channels use too). The listener's account is checked
/// before client I/O (ADR 0049, User isolation). The fixed `CONNECT_BOUND` retry budget permits an attempt or wait in
/// progress to finish (Unix's 20 ms retry sleep, the 200 ms named-pipe wait); it is not an exact elapsed-time limit.
pub(crate) async fn connect_pipe(path: &std::path::Path) -> Result<LocalStream> {
    sot_topology::link::connect_local(path).await.with_context(|| format!("connect {path:?}"))
}

/// How often this connection sends `ping` (topology plan §F step 2) — a
/// third of the daemon's own `PING_READ_DEADLINE` (90s, `server/conn.rs`), so a
/// missed tick or two is noise and three in a row is what actually trips
/// the daemon's reaper. `SOT_TEST_PING_INTERVAL_MS` overrides it for tests
/// (same `OnceLock`-cached-once-per-process convention the backend uses
/// for its own deadline override); unset in every real deployment.
fn ping_interval_duration() -> std::time::Duration {
    static OVERRIDE_MS: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let override_ms = *OVERRIDE_MS.get_or_init(|| {
        std::env::var("SOT_TEST_PING_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse().ok())
    });
    override_ms
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_secs(30))
}

/// What `run_protocol` needs of the window: a redraw request. A trait so a
/// test can run the protocol without a real window.
pub(crate) trait Redraw {
    fn request_redraw(&self);
}

/// Never a winit call from here: on macOS it would wait for the window's thread (`MainWake`).
impl Redraw for crate::ui::MainWake {
    fn request_redraw(&self) {
        self.wake();
    }
}


/// Run the session and write the host's link handle: up at an accepted hello
/// (inside [`run_session`]), down when the session ends for any reason. `link`
/// is `None` for a local connection, which has no link.
async fn run_protocol<R, W, Wn>(
    host: HostKey,
    rx: tokio::io::BufReader<R>,
    tx: W,
    token: Option<&str>,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: &Wn,
    resolved: ResolvedDial,
    link: Option<(&sot_topology::link::LinkHandle, &Link)>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Wn: Redraw,
{
    let result = run_session(host, rx, tx, token, evt_tx, out_rx, window, resolved, link).await;
    if let Some((handle, _)) = link {
        handle.set_down();
    }
    result
}

/// Drive the wire protocol over an already-connected stream's halves. Generic
/// over the read/write types so the same code path serves the local-socket
/// transport and a link's channel 0 — C3 as amended §0: adding this
/// transport inside the protocol CRATE is not a wire-protocol change; the
/// frames this function reads/writes are untouched.
async fn run_session<R, W, Wn>(
    host: HostKey,
    mut rx: tokio::io::BufReader<R>,
    mut tx: W,
    token: Option<&str>,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: &Wn,
    // C3 as amended §5: which transport `connect_and_run` actually
    // connected — `ResolvedDial::Local` for the pipe, `ResolvedDial::Ssh`
    // or `Relay` for the host's link, carrying its exact route. The proxy
    // arms only when NOT `Local` — keyed on the transport that CONNECTED,
    // not the CLI shape.
    resolved: ResolvedDial,
    link: Option<(&sot_topology::link::LinkHandle, &Link)>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Wn: Redraw,
{
    let mut next_id: u64 = 1;
    // PendingGuard, not a bare HashMap: its Drop flushes any surviving
    // FigureGet entries as FigureGetFailed on every exit path this
    // function has (see the type's doc comment) — the fix for a
    // connection dropping between a figure.get and its reply.
    let mut pending = PendingGuard {
        map: HashMap::new(),
        evt_tx,
        host: host.clone(),
    };
    // Tags every run_protocol-level send with `host` — ADR 0042 L2a: the
    // app's receiver is `Receiver<(HostKey, IncomingEvt)>`, tagged at each
    // send (this closure), not by a separate forwarding task.
    // `handle_response_frame` (a separate fn, most of the actual sends)
    // carries its own copy of the same pattern.
    let emit = {
        let host = host.clone();
        move |ev: IncomingEvt| {
            let _ = evt_tx.send((host.clone(), ev));
        }
    };

    // Reconnect memory: client_id stays stable across runs; session_id +
    // last_seen_revision feed the backend's replay path. First-ever launch
    // produces fresh values and the backend assigns a session_id we'll
    // remember for next time.
    // `session` bundles the memory with its `StateSaveGate` (see
    // `StateSaveGate`'s doc: throttles `crate::net::state::save` so a burst of
    // `rev`-bearing replies can't stall this task's read future on disk
    // I/O) and, via its `Drop`, flushes whatever the throttle held back the
    // moment this connection ends — see `SessionState`.
    let mut session = SessionState {
        host: host.clone(),
        memory: crate::net::state::load(&host),
        gate: StateSaveGate::new(),
    };
    tracing::info!(
        %host,
        client_id = %session.memory.client_id,
        ?session.memory.session_id,
        last_seen_revision = session.memory.last_seen_revision,
        token_set = token.is_some(),
        "loaded session memory"
    );

    // hello
    let hello_id = take_id(&mut next_id);
    send_hello(&mut tx, hello_id, &session, token).await?;
    let frame = read_hello(&mut rx, hello_id, &mut session, &emit, window).await?;
    // The daemon admitted this session: the host's pages and rows may open channels on its link from now on.
    if let Some((handle, link)) = link {
        handle.set_up(link);
    }
    // The window's REPL runs the daemon names still running: this connection waits on each as on any run, so its
    // `done` or this connection's end closes the entry the window reopens for it.
    for eval_id in accept_hello(frame, &host, &mut session, resolved, &emit, window)? {
        eval_running(&mut pending, eval_id);
    }
    // A frame the daemon writes before a preamble reply goes where the steady loop sends it, and wakes the window as there.
    let mut other = |frame, blob| {
        handle_response_frame(frame, blob, &mut pending, evt_tx, &host);
        window.request_redraw();
    };

    // tree.root — initial fetch on connect uses the default workspace
    // (no workspace_id). Once the chrome resumes a saved Sessions-mode
    // active_workspace_id it will re-fire this with the id set.
    let tree_id = take_id(&mut next_id);
    let root_node_id =
        preamble_tree_root(&mut tx, &mut rx, tree_id, &host, &mut session, &emit, window, &mut other).await?;

    // preview.get against whatever the backend just reported as the root.
    // For the spike that's enough to prove blob round-trip; real previews
    // follow user navigation.
    let prev_id = take_id(&mut next_id);
    preamble_preview(&mut tx, &mut rx, prev_id, root_node_id, &host, &mut session, &emit, window, &mut other).await?;

    steady::steady_loop(
        rx,
        &mut tx,
        next_id,
        &mut pending,
        &mut session,
        host,
        evt_tx,
        out_rx,
        window,
        ping_interval_duration(),
    )
    .await
}

#[cfg(all(test, feature = "test-window-progress"))]
pub(crate) async fn run_native_progress_transport<R, W, Wn>(
    rx: tokio::io::BufReader<R>,
    tx: W,
    host: HostKey,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: &Wn,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Wn: Redraw,
{
    let mut pending = PendingGuard {
        map: HashMap::new(),
        evt_tx,
        host: host.clone(),
    };
    let mut session = SessionState {
        host: host.clone(),
        memory: crate::net::state::SessionMemory::fresh(),
        gate: StateSaveGate::new(),
    };
    steady::steady_loop(
        rx,
        tx,
        1,
        &mut pending,
        &mut session,
        host,
        evt_tx,
        out_rx,
        window,
        ping_interval_duration(),
    )
    .await
}

fn take_id(next_id: &mut u64) -> u64 {
    let id = *next_id;
    *next_id += 1;
    id
}

#[cfg(test)]
mod golden_tests;

#[cfg(test)]
mod tests;
