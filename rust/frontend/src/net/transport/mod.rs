// net/transport — local frontend ↔ remote backend over a local socket (Unix
// socket / Windows named pipe) or an ssh child's stdio.
//
// Per ADR 0010, as replaced by isolation-plan.md §3 C3 (amended by
// dev/output/c3-second-connection-amendment.md):
//   - Reaching a daemon that is not on this box means spawning
//     `ssh <target> '<PATH prelude>; sotd stdio-bridge [--host <host>]'`
//     (`sot_protocol::topology::ssh_bridge`) and speaking the protocol over its piped
//     stdin/stdout — never a port, on either box: the daemon has had no TCP
//     listener since 0.4.0. A dead login or a dead `sotd` on the far end is
//     the child exiting before the first frame; its last stderr line IS the
//     diagnosis (no per-cause exit codes to invent).
//   - Connect handshake carries (session_id, client_id, last_seen_revision);
//     backend either replays missed events or sends a snapshot on reconnect.
//
// Transport selection: `spawn` takes a pipe or an ssh recipe. The protocol
// code is generic over `AsyncRead` / `AsyncWrite` so it runs identically on
// either transport — an ssh child is a third `AsyncRead`/`AsyncWrite` pair,
// not a protocol change (the amendment's own §0).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender as StdSender;
use std::sync::Arc;

use crate::net::dial::HostKey;
use anyhow::{Context, Result};
use base64::Engine;
use interprocess::local_socket::{
    tokio::{prelude::*, Stream as LocalStream},
    GenericFilePath,
};
use serde_json::Value;
use sot_protocol::{
    codec, op, AgentSendReq, ConceptReadReq, ConceptReadRes, ConceptWriteReq, ConceptWriteRes,
    DirCreateReq, DirCreateRes, DocsOpenReq, DocsOpenRes, FePresenceReq, FileChunk, FileDeleteReq, FileDeleteRes,
    FileDownloadReq, FileReadReq, FileReadRes, FileUploadAck, FileUploadReq, FileWriteReq,
    FileWriteRes, Frame,
    HelloReq, HelloRes, ImageCropReq, ImageCropRes, KernelRequestReq, MathRenderReq, MathRenderRes,
    MonitorHistoryReq, MonitorHistoryRes, MonitorSubscribeRes, MonitorTickEvt, PingReq, PlutoOpenReq,
    PlutoOpenRes, PreviewGetReq, PreviewGetRes, PtyOpenReq,
    QuartoOpenReq, ReplEvalReq, ReplEvalRes, ReplFrame, ReplFrameEvt, ReplRunFileReq,
    ReplRunFileRes, ToggleHiddenReq, TreeChildrenReq, TreeChildrenRes, TreeNode, TreeRootReq,
    TreeRootRes, VideoOpenReq, VideoOpenRes, WorkspaceActivateReq, WorkspaceListReq,
    WorkspaceListRes,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc::{self as tmpsc, UnboundedReceiver, UnboundedSender};

use winit::window::Window;

mod event;
mod hello;
mod ops;
mod preamble;
mod reply;
mod request;

use crate::net::state::{note_revision, SessionState, StateSaveGate};
use hello::{accept_hello, read_hello, send_hello, HelloRefused};
use ops::*;
use preamble::{preamble_preview, preamble_tree_root};
use reply::{handle_response_frame, PendingGuard, PendingKind};
use request::send_request;

// The transport's interface: what code outside this folder names.
pub(crate) use self::{
    event::IncomingEvt,
    ops::{
        AccountInfo, ConceptWriteResult, DefinitionInfo, DirCreateResult, DirEntry,
        FileDeleteResult, FileWriteResult, MarkdownToken, MethodInfo, ReplRunFileInfo,
        ScanModule, ScanType, WorkspaceCreatedInfo, WorkspaceDestroyedInfo, WorkspaceInfo,
    },
    request::OutgoingReq,
};

/// What the transport task should dial: a local socket/named pipe, or an
/// ssh child's stdio (C3). Exactly one, never neither and never both —
/// replaces the former `pipe: Option<PathBuf>` / `ssh: Option<SshRecipe>`
/// pair, whose "at least one must be set" was a doc caveat callers had to
/// honor by convention (the CLI never actually built one with neither set;
/// the only construction with both was a `#[cfg(test)]` state the CLI
/// cannot produce) rather than a fact the type itself enforced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dial {
    Pipe(PathBuf),
    Ssh(sot_protocol::topology::ssh_bridge::SshRecipe),
}

#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub dial: Dial,
    pub token: Option<String>,
}

/// ADR 0045 decision 1 (Codex review, lane B5 discharge); reshaped by C3 as
/// amended: which transport a host's CONTROL connection actually resolved
/// to — `Local` (the pipe/socket connected) or `Ssh` (the ssh child
/// connected, carrying the exact recipe it spawned). Recorded from every
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
    Ssh(sot_protocol::topology::ssh_bridge::SshRecipe),
}

/// Create the outgoing-request channel paired with the transport task. The
/// sender lives on the GPU thread; the receiver gets handed to `spawn`. Both
/// sides drop their handle on shutdown — that's how the writer half of the
/// select loop terminates.
pub fn outgoing_channel() -> (UnboundedSender<OutgoingReq>, UnboundedReceiver<OutgoingReq>) {
    tmpsc::unbounded_channel()
}

/// Next reconnect wait after a failed attempt: double, up to a cap that
/// depends on the dial. A local socket costs nothing to probe, so it keeps
/// 5 s. Each ssh probe is a login on the hub plus one on the far host, so
/// the cap is 30 s: once the wait reaches it, a down remote host costs the hub
/// at most two ssh logins a minute per frontend (about nine in the first minute,
/// while the wait doubles).
fn next_backoff_ms(current: u64, dial: &Dial) -> u64 {
    let cap = match dial {
        Dial::Pipe(_) => 5_000,
        Dial::Ssh(_) => 30_000,
    };
    current.saturating_mul(2).min(cap)
}

/// Spawn the transport task on `rt`. Returns once spawned; the task runs
/// until the connection drops or the runtime shuts down. The task asks the
/// window to redraw whenever a new IncomingEvt is published so the GPU
/// thread sees state updates without polling.
pub fn spawn(
    rt: &tokio::runtime::Runtime,
    host: HostKey,
    config: TransportConfig,
    evt_tx: StdSender<(HostKey, IncomingEvt)>,
    out_rx: UnboundedReceiver<OutgoingReq>,
    window: Arc<Window>,
    reconnect_now: Arc<tokio::sync::Notify>,
    gate: sot_protocol::topology::ssh_bridge::LinkGate,
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
        // the task. Backoff resets to the floor after `connect_and_run`
        // reaches `hello_res` (signalling a real round-trip succeeded)
        // OR after a clean Ok return; mid-handshake failures keep
        // walking the backoff up so a thrashing backend doesn't get
        // hammered. The F5 `reconnect_now` notify lets the user
        // collapse the current sleep — useful when wifi flickers and
        // the user knows it's back before the 5s cap elapses.
        let mut out_rx = out_rx;
        let mut backoff_ms: u64 = 200;
        const BACKOFF_FLOOR_MS: u64 = 200;
        loop {
            match connect_and_run(
                host.clone(),
                config.clone(),
                evt_tx.clone(),
                &mut out_rx,
                window.clone(),
                &mut backoff_ms,
                &gate,
                &leases,
            )
            .await
            {
                Ok(()) => {
                    tracing::info!(%host, "transport task exited cleanly");
                    return;
                }
                Err(e) => {
                    tracing::warn!(
                        %host,
                        error = %format_args!("{e:#}"),
                        backoff_ms,
                        "transport task ended; reconnecting"
                    );
                    let _ = evt_tx.send((
                        host.clone(),
                        IncomingEvt::Disconnected {
                            reason: format!("{e:#} — retry in {backoff_ms}ms (F5 to retry now)"),
                        },
                    ));
                    window.request_redraw();
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {}
                        _ = reconnect_now.notified() => {
                            tracing::info!("manual reconnect requested — collapsing backoff");
                            backoff_ms = BACKOFF_FLOOR_MS;
                            continue;
                        }
                    }
                    backoff_ms = next_backoff_ms(backoff_ms, &config.dial);
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
async fn connect_and_run(
    host: HostKey,
    config: TransportConfig,
    evt_tx: StdSender<(HostKey, IncomingEvt)>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: Arc<Window>,
    backoff_ms: &mut u64,
    gate: &sot_protocol::topology::ssh_bridge::LinkGate,
    leases: &crate::lease::Leases,
) -> Result<()> {
    match &config.dial {
        Dial::Pipe(pipe_path) => {
            let not_ended = leases
                .before_data_connection(&host, pipe_path, config.token.as_deref())
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
                backoff_ms,
                ResolvedDial::Local,
                None,
            )
            .await
        }
        Dial::Ssh(recipe) => {
            let mut child = sot_protocol::topology::ssh_bridge::LinkGate::probe(recipe)
                .with_context(|| format!("spawn ssh {recipe}"))?;
            let stdin = child.stdin.take().expect("spawned with a piped stdin");
            let stdout = child.stdout.take().expect("spawned with a piped stdout");
            let stderr = child.stderr.take().expect("spawned with a piped stderr");
            let last_stderr = spawn_stderr_drain(stderr);
            // Pre-hello, same labeling rule as the pipe branch above.
            tracing::info!(dial = %host, %recipe, "connected via ssh child");
            let rx = codec::buffered(stdout);
            // The gate goes down inside `run_protocol`'s own wrapper, before
            // the stderr wait below, so no lane dials while that wait runs.
            let result = run_protocol(
                host,
                rx,
                stdin,
                config.token.as_deref(),
                &evt_tx,
                out_rx,
                &window,
                backoff_ms,
                ResolvedDial::Ssh(recipe.clone()),
                Some(gate),
            )
            .await;
            // `child` is dropped when this arm returns (`kill_on_drop`), after the
            // stderr read below, ending the ssh login this attempt owns before the
            // reconnect loop's next attempt spawns a fresh one.
            if let Err(e) = &result {
                if let Some(line) = sot_protocol::topology::ssh_bridge::last_stderr_after_failure(&last_stderr).await {
                    return Err(anyhow::anyhow!("{e:#} (ssh: {line})"));
                }
            }
            result
        }
    }
}

/// Connect to the local socket at `path`, only when this OS account serves it (ADR 0049, User isolation): the socket's
/// folder is checked before the connect.
#[cfg(unix)]
pub(crate) async fn connect_pipe(path: &std::path::Path) -> Result<LocalStream> {
    let path_str = path.to_str().context("socket path must be valid UTF-8")?;
    let name = path_str
        .to_fs_name::<GenericFilePath>()
        .with_context(|| format!("interpret {path_str:?} as local-socket name"))?;
    sot_log::identity::connect_own::own_socket(path).with_context(|| format!("connect {path:?}"))?;
    LocalStream::connect(name)
        .await
        .with_context(|| format!("connect {path:?}"))
}

/// Connect to the named pipe at `path`, only when this OS account serves it (ADR 0049, User isolation): the pipe is
/// opened at identification level, so whatever serves it can never act as this account, and the serving process is
/// checked before any byte is written. Opened here rather than by interprocess, which has no way to ask for that level;
/// the open waits out `ERROR_PIPE_BUSY` as interprocess does, bounded.
#[cfg(windows)]
pub(crate) async fn connect_pipe(path: &std::path::Path) -> Result<LocalStream> {
    use interprocess::os::windows::named_pipe::local_socket::tokio::Stream as PipeStream;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::{AsHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, SECURITY_IDENTIFICATION};

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let file = loop {
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(path);
        match opened {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            other => break other.with_context(|| format!("connect {path:?}"))?,
        }
    };
    sot_log::identity::connect_own::own_pipe(file.as_handle(), path).with_context(|| format!("connect {path:?}"))?;
    let stream = PipeStream::try_from(OwnedHandle::from(file))
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("connect {path:?}"))?;
    Ok(LocalStream::from(stream))
}

/// Read exactly one frame while *owning* the reader, handing it back with the
/// result. This lets the steady-state select! loop keep a single in-flight
/// read future across iterations (cancel-safe: a cancelled select! pauses it
/// rather than dropping it mid-blob) without the borrow checker objecting to a
/// stored future that re-borrows `rx` each loop. See the CANCEL-SAFETY note in
/// `run_protocol`'s steady-state loop.
async fn read_owned<R: AsyncRead + Unpin>(
    mut rx: tokio::io::BufReader<R>,
) -> (tokio::io::BufReader<R>, Result<(Frame, Option<Vec<u8>>)>) {
    let res = codec::read_frame(&mut rx).await;
    (rx, res)
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
trait Redraw {
    fn request_redraw(&self);
}

impl Redraw for Arc<Window> {
    fn request_redraw(&self) {
        Window::request_redraw(self);
    }
}


/// Run the session and write the link gate: up at any hello reply (inside
/// [`run_session`]), down when the session ends for any reason except a
/// refusal of the hello itself. `gate` is `None` for a local connection,
/// which has no ssh link to gate.
async fn run_protocol<R, W, Wn>(
    host: HostKey,
    rx: tokio::io::BufReader<R>,
    tx: W,
    token: Option<&str>,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: &Wn,
    backoff_ms: &mut u64,
    resolved: ResolvedDial,
    gate: Option<&sot_protocol::topology::ssh_bridge::LinkGate>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Wn: Redraw,
{
    let result = run_session(host, rx, tx, token, evt_tx, out_rx, window, backoff_ms, resolved, gate).await;
    if let Some(gate) = gate {
        if !matches!(&result, Err(e) if e.is::<HelloRefused>()) {
            gate.set_up(false);
        }
    }
    result
}

/// Drive the wire protocol over an already-connected stream's halves. Generic
/// over the read/write types so the same code path serves the local-socket
/// transport and an ssh child's stdio — C3 as amended §0: adding this
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
    backoff_ms: &mut u64,
    // C3 as amended §5: which transport `connect_and_run` actually
    // connected — `ResolvedDial::Local` for the pipe, `ResolvedDial::Ssh`
    // for the ssh child, carrying the exact recipe it spawned. The proxy
    // arms only when NOT `Local` — keyed on the transport that CONNECTED,
    // not the CLI shape.
    resolved: ResolvedDial,
    gate: Option<&sot_protocol::topology::ssh_bridge::LinkGate>,
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
    let frame = read_hello(&mut rx, hello_id, gate, &mut session, &emit, window).await?;
    accept_hello(frame, &host, &mut session, backoff_ms, resolved, &emit, window)?;

    // tree.root — initial fetch on connect uses the default workspace
    // (no workspace_id). Once the chrome resumes a saved Sessions-mode
    // active_workspace_id it will re-fire this with the id set.
    let tree_id = take_id(&mut next_id);
    let root_node_id = preamble_tree_root(&mut tx, &mut rx, tree_id, &host, &mut session, &emit, window).await?;

    // preview.get against whatever the backend just reported as the root.
    // For the spike that's enough to prove blob round-trip; real previews
    // follow user navigation.
    let prev_id = take_id(&mut next_id);
    preamble_preview(&mut tx, &mut rx, prev_id, root_node_id, &host, &mut session, &emit, window).await?;

    steady_loop(rx, &mut tx, next_id, &mut pending, &mut session, host, evt_tx, out_rx, window).await
}

/// The steady-state loop: replies and pushed events, the ping, and the window's requests.
async fn steady_loop<R, W, Wn>(
    rx: tokio::io::BufReader<R>,
    mut tx: W,
    mut next_id: u64,
    pending: &mut HashMap<u64, PendingKind>,
    session: &mut SessionState,
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
    // Steady-state loop. `tokio::select!` lets us simultaneously read frames
    // arriving from the backend (replays, future server-pushed evts, replies
    // to outgoing requests) and accept new requests from the GPU thread. The
    // request id is allocated on the writer side and stashed in `pending`;
    // the reader matches incoming response frames against it to route
    // deserialization. Unsolicited events (no id in pending) fall through to
    // the catch-all `Event` evt the same way the old idle loop handled them.
    //
    // CANCEL-SAFETY: `read_frame` is NOT cancellation-safe — it reads the
    // `\n`-terminated envelope and then `read_exact`s the blob tail across
    // two separate awaits. If we polled `codec::read_frame(&mut rx)` directly
    // as a select! arm, an outgoing request arriving while a blob was still
    // mid-flight would make select! drop the half-read future: the envelope
    // bytes were already consumed but the blob tail was not, so the next read
    // parsed leftover binary blob bytes as a JSON envelope, failed, and forced
    // a reconnect — the spurious-reconnect → tree-collapse → nav-reset bug.
    // Fix: hold one read future across iterations and poll it by `&mut`, so a
    // cancelled select! merely *pauses* it; it resumes mid-blob next iteration
    // instead of being recreated from a desynced stream offset. The future
    // *owns* the reader (via `read_owned`) and hands it back on completion, so
    // the borrow checker never sees an external `&mut rx` re-borrowed across
    // iterations.
    let mut read_fut = Some(Box::pin(read_owned(rx)));
    // Topology plan §F step 2 (the half-open-roster fix): this connection
    // is always `fe`-declared (see `hello` above), one of the daemon's two
    // long-lived roles, so it always pings — no role check needed here,
    // unlike the daemon side which also has to let `cli`/`agent` through
    // ungated. `Interval`, not a plain `sleep_until` recomputed each loop:
    // it owns its own next-tick state and its `tick()` is cancellation-
    // safe, so a `select!` iteration that takes another arm just leaves it
    // armed for next time instead of losing the schedule.
    let mut ping_interval = tokio::time::interval(ping_interval_duration());
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping_interval.tick().await; // first tick fires immediately; consume it
    loop {
        tokio::select! {
            // Bias to reads so an avalanche of GPU-thread requests can't
            // starve replies. Spike-grade — revisit if it ever matters.
            biased;

            done = read_fut.as_mut().expect("read_fut is always Some at loop top") => {
                // Completed: reclaim the reader and arm the next read.
                let (rx_back, read) = done;
                read_fut = Some(Box::pin(read_owned(rx_back)));
                let (frame, blob) = read?;
                note_revision(frame.rev, &mut session.memory, &host, &mut session.gate);
                handle_response_frame(frame, blob, pending, evt_tx, &host);
                window.request_redraw();
            }

            // Topology plan §F step 2: prove this connection's read half is
            // alive to the daemon even when the person is idle (no other
            // outgoing traffic). Fire-and-forget, same idiom as
            // `OutgoingReq::FePresence` below — no `PendingKind`, the reply
            // is silently ignored by the unmatched-id fallthrough.
            _ = ping_interval.tick() => {
                let id = take_id(&mut next_id);
                tracing::debug!(id, "→ ping");
                codec::write_frame(
                    &mut tx,
                    &Frame::req(id, op::PING, serde_json::to_value(PingReq {})?),
                    None,
                )
                .await?;
            }

            req = out_rx.recv() => {
                let Some(req) = req else {
                    // Sender side dropped — the app is shutting down. Drain
                    // the reader by falling back to a plain read loop until
                    // the connection closes.
                    tracing::debug!("outgoing channel closed; draining reads until disconnect");
                    // Shutdown path: no more outgoing requests can race the
                    // reader, so cancel-safety no longer matters. Resume the
                    // in-flight read (reclaiming the reader), then fall back to
                    // plain sequential reads until the connection closes.
                    let fut = read_fut.take().expect("read_fut is always Some here");
                    let (mut rx, read) = fut.await;
                    let (frame, blob) = read?;
                    note_revision(frame.rev, &mut session.memory, &host, &mut session.gate);
                    handle_response_frame(frame, blob, pending, evt_tx, &host);
                    window.request_redraw();
                    loop {
                        let (frame, blob) = codec::read_frame(&mut rx).await?;
                        note_revision(frame.rev, &mut session.memory, &host, &mut session.gate);
                        handle_response_frame(frame, blob, pending, evt_tx, &host);
                        window.request_redraw();
                    }
                };
                let id = take_id(&mut next_id);
                send_request(&mut tx, pending, id, req).await?;
            }
        }
    }
}

fn take_id(next_id: &mut u64) -> u64 {
    let id = *next_id;
    *next_id += 1;
    id
}

/// The child's last non-empty stderr line, drained on its own task
/// for as long as `child` lives — ssh's own complaint ("Permission
/// denied", or `unrecognised argument: --host` from a hub whose
/// `sotd` predates C1) is the diagnosis a dead child leaves behind,
/// the same rule `stdio_bridge.rs` already sets for the far end.
pub(crate) fn spawn_stderr_drain<R>(stderr: R) -> Arc<std::sync::Mutex<Option<String>>>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let last_stderr = Arc::new(std::sync::Mutex::new(None::<String>));
    {
        let last_stderr = Arc::clone(&last_stderr);
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    if let Ok(mut guard) = last_stderr.lock() {
                        *guard = Some(line);
                    }
                }
            }
        });
    }
    last_stderr
}

#[cfg(test)]
mod golden_tests;

#[cfg(test)]
mod tests;
