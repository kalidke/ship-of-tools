//! One connection: its read-deadline reaper, its first-frame peek (`handle_connection`) and its control loop
//! (`serve_control`, with `select_once` for one pass of its select).

use super::dispatch::dispatch;
use super::events::{
    recv_agent_msg, recv_agent_receipt, recv_fe_command, recv_monitor, recv_repl_frame, recv_topo_changed,
    recv_watcher, recv_ws_events, write_agent_message, write_agent_receipt, write_fe_command, write_monitor_tick,
    write_preview_changed, write_repl_frame, write_topology_changed, write_workspace_changed,
};
use super::reply::{write_reply, HandlerOutput, OutTx, OFFLOOP_CONCURRENCY};
use super::*;

/// Read deadline for a connection whose declared role is `fe` or `bridge`
/// (topology plan §F step 2, D10 — the half-open-roster fix). Since 0.4.0
/// the daemon has had no keepalive, and `WRITE_TIMEOUT` above never fires
/// for a tunnelled peer (its writes always drain into sshd's Unix-socket
/// side) — a closed laptop stayed in the roster for 15 min to 2 h. No
/// frame AT ALL from the peer within this long means treat the connection
/// as dead: drop it and reap it through the same `ClientGuard::drop` path
/// as a clean exit (`clients.rs:329`). Three times the client-side `ping`
/// interval (30s — the frontend transport): one missed tick is noise, three in a row is a dead peer.
/// `cli`/`agent` (one-shot) connections never gate on this — see
/// `is_long_lived_role` at its declaration site.
const PING_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(90);

/// `SOT_TEST_PING_READ_DEADLINE_MS` overrides [`PING_READ_DEADLINE`] for
/// tests — same `OnceLock`-cached-once-per-process convention as
/// `test_slow_concept_read_delay` (read once, before any connection can
/// have started, never something a client controls per-request). Unset in
/// every real deployment.
pub(super) fn ping_read_deadline() -> std::time::Duration {
    static OVERRIDE_MS: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let override_ms = *OVERRIDE_MS.get_or_init(|| {
        std::env::var("SOT_TEST_PING_READ_DEADLINE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
    });
    override_ms
        .map(std::time::Duration::from_millis)
        .unwrap_or(PING_READ_DEADLINE)
}

/// Test-only knob (switch-latency Phase 1): an integration test needs ONE
/// request it can make deterministically slow, through the real wire
/// protocol, to prove a later cheap request's reply doesn't wait behind it
/// on the same connection — no existing op is slow on demand without
/// something a CI sandbox can't assume (a real Julia kernel, a large file
/// already on disk). Reads `SOT_TEST_SLOW_CONCEPT_READ_MS` ONCE per process
/// via `OnceLock`, so it can only ever be a fixed value a test harness set
/// before spawning the daemon — never something a client controls
/// per-request. Unset (or unparseable) is `0`, a no-op sleep: production
/// never sets this, so every real deployment gets zero delay.
pub(super) fn test_slow_concept_read_delay() -> std::time::Duration {
    static DELAY_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let ms = *DELAY_MS.get_or_init(|| {
        std::env::var("SOT_TEST_SLOW_CONCEPT_READ_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    });
    std::time::Duration::from_millis(ms)
}

/// Read one frame while *owning* the buffered reader, handing it back with
/// the result. Lets the per-connection select! loop keep a single in-flight
/// read future across iterations so a cancelled select! *pauses* a mid-blob
/// read rather than dropping it — `codec::read_frame` is NOT cancellation-safe
/// (it reads the `\n` envelope then `read_exact`s the blob tail across two
/// awaits). Mirrors the frontend transport fix (commit 8746b74).
async fn read_owned<R: AsyncRead + Unpin>(
    mut rx: tokio::io::BufReader<R>,
) -> (tokio::io::BufReader<R>, Result<(Frame, Option<Vec<u8>>)>) {
    let res = codec::read_frame(&mut rx).await;
    (rx, res)
}

/// Stamp this connection's `last_person_input_at` if it has registered.
/// Call ONLY from the `fe.presence` op arm (2026-09-08 review rework,
/// design point A) — that op alone is trustworthy evidence of a person,
/// because the frontend sends it from its own real keyboard/mouse input
/// handlers, throttled there, never from a command-file or other
/// automated path. An EARLIER design stamped this from ordinary
/// navigation ops (`tree.root`, `preview.get`, `pty.write`, a
/// `workspace.activate` with `read: true`); review found every one of
/// them had an automated producer too (reconnect re-announces tree/preview
/// requests, a badge-consuming `goto` fires them, an autostarted agent
/// writes into its own pane, and a command-file `cycle_ws` could forge
/// `read: true`) — deleted rather than patched. A no-op pre-hello, when
/// `client_guard` is still `None`.
pub(super) fn touch_person_input(clients: &Clients, guard: &Option<ClientGuard>) {
    if let Some(g) = guard {
        clients.touch_person_input(g.serial());
    }
}

pub(super) async fn handle_connection<R, W>(
    rx: R,
    tx: W,
    session: Session,
    mathjax: MathJax,
    pluto: Pluto,
    files_mode: Arc<FilesMode>,
    preview_changed_tx: broadcast::Sender<PreviewChanged>,
    label: Arc<Option<String>>,
    workspaces: Workspaces,
    ws_events_tx: broadcast::Sender<WorkspaceChanged>,
    agent_events_tx: broadcast::Sender<AgentMessage>,
    agent_receipt_tx: broadcast::Sender<AgentReceipt>,
    fe_command_tx: broadcast::Sender<FeCommandEvt>,
    repl_frame_tx: broadcast::Sender<ReplFrameMsg>,
    clients: Clients,
    topology_store: Arc<crate::topology::store::TopologyStore>,
    topo_changed_tx: broadcast::Sender<crate::topology::store::TopologyChanged>,
    peer_identity: sot_log::identity::challenge::PeerAuthOutcome,
    leases: Arc<crate::lifecycle::lease::Leases>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // CANCEL-SAFETY: `read_frame` is NOT cancellation-safe (it reads the `\n`
    // envelope then `read_exact`s the blob tail across two awaits). Polling
    // `codec::read_frame(&mut rx)` directly as a select! arm meant that when
    // another arm (pty bytes / watcher change) completed mid-blob, select!
    // dropped the half-read future: the envelope was consumed but the blob
    // tail was not, so the next read parsed leftover binary as a JSON envelope
    // and failed, dropping the connection. Hold one read future across loop
    // iterations and poll it by `&mut`, so a cancelled select! pauses it (it
    // resumes mid-blob next iteration). The future owns the reader (via
    // `read_owned`) and hands it back on completion. Mirrors frontend 8746b74;
    // lower-risk here (frontend→backend reqs rarely carry blob tails) but the
    // same latent footgun. Flagged from the Windows side 2026-05-26.
    let mut buffered = codec::buffered(rx);

    // ADR 0035: a proxy connection announces itself with `proxy.connect` as
    // its VERY FIRST frame and then becomes a raw byte pipe — it must never
    // enter the multiplexed control loop below (where it would be
    // hello-gated and could head-of-line-block pty/repl traffic). Peek that
    // one frame here, before the persistent read future is armed. On
    // proxy.connect, hand the (buffered) reader + write half straight to the
    // pipe and return. Otherwise, remember the frame and feed it to the loop
    // as its first dispatched frame (`pending_first`), so the peek costs the
    // control path nothing.
    let pending_first: Option<(Frame, Option<Vec<u8>>)> = match codec::read_frame(&mut buffered)
        .await
    {
        Ok((f, blob)) => {
            if f.kind == Kind::Req && f.op == op::PROXY_CONNECT {
                tracing::info!("proxy.connect — leaving control loop for a raw pipe");
                return crate::pages::proxy::handle_proxy_connect(
                    buffered,
                    tx,
                    f,
                )
                .await;
            }
            // ADR 0045 decision 2: peeked on every host, exactly like
            // `rows/ops/lane_bridge.rs` itself is compiled on every host (macOS
            // wiring lane) — one attach path, local or remote, with no
            // platform where `lane.connect` silently falls through to
            // the "unknown op" answer instead.
            if f.kind == Kind::Req && f.op == op::LANE_CONNECT {
                tracing::info!("lane.connect — leaving control loop for a raw pipe");
                return crate::rows::ops::lane_bridge::handle_lane_connect(
                    buffered,
                    tx,
                    f,
                    &workspaces,
                )
                .await;
            }
            // A lease (1.2) is a connection of its own: it never enters
            // the hello-gated loop, the reaper or any handler.
            if f.kind == Kind::Req && f.op == op::FE_LEASE {
                tracing::info!(?peer_identity, "fe.lease — a lease connection");
                let state_root = sot_log::host::state_dir::sot_state_dir();
                return crate::lifecycle::lease::hold(
                    buffered,
                    tx,
                    f,
                    peer_identity,
                    &leases,
                    state_root.as_deref(),
                )
                .await;
            }
            Some((f, blob))
        }
        Err(e) => {
            tracing::debug!(error = %e, "first read failed before any frame; closing");
            return Ok(());
        }
    };

    serve_control(
        tx, buffered, pending_first, session, mathjax, pluto, files_mode, preview_changed_tx, label, workspaces,
        ws_events_tx, agent_events_tx, agent_receipt_tx, fe_command_tx, repl_frame_tx, clients, topology_store,
        topo_changed_tx, leases,
    )
    .await
}

/// Runs one control session: the per-connection state, then the frame loop and its op table.
async fn serve_control<R, W>(
    mut tx: W, buffered: tokio::io::BufReader<R>, mut pending_first: Option<(Frame, Option<Vec<u8>>)>,
    session: Session, mathjax: MathJax, pluto: Pluto, files_mode: Arc<FilesMode>,
    preview_changed_tx: broadcast::Sender<PreviewChanged>, label: Arc<Option<String>>, workspaces: Workspaces,
    ws_events_tx: broadcast::Sender<WorkspaceChanged>, agent_events_tx: broadcast::Sender<AgentMessage>,
    agent_receipt_tx: broadcast::Sender<AgentReceipt>, fe_command_tx: broadcast::Sender<FeCommandEvt>,
    repl_frame_tx: broadcast::Sender<ReplFrameMsg>, clients: Clients,
    topology_store: Arc<crate::topology::store::TopologyStore>,
    topo_changed_tx: broadcast::Sender<crate::topology::store::TopologyChanged>, leases: Arc<crate::lifecycle::lease::Leases>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut read_fut = Some(Box::pin(read_owned(buffered)));
    tracing::debug!("connection ready");


    // Connected-client registry entry (ADR 0010/0013). Registered on the
    // first `hello` (when this connection's client_id is known) and held
    // for the connection's lifetime; the guard deregisters on any exit
    // path (clean EOF, error, task drop). `None` until hello arrives.
    let mut client_guard: Option<crate::clients::ClientGuard> = None;
    // This connection's own declared host (`HelloReq.host`, ADR 0046
    // decision 1), captured at hello — `topology.set`'s "can't remove
    // yourself" refusal reads it (`server/dispatch.rs`, `op::TOPOLOGY_SET`).
    let mut hello_host: Option<String> = None;
    // This connection's own declared sot-comm name (`HelloReq.name`, the
    // same value `Clients::receivers_for` reports), captured at hello:
    // `agent.filed` stamps it as the `filer` (ADR 0048). Read from the
    // hello, never from a request body — that is what makes a receipt
    // unforgeable. `None` for a connection that declared no name, which
    // therefore cannot vouch for anything.
    let mut hello_name: Option<String> = None;

    // Half-open long-lived-role reaper (topology plan §F step 2). A
    // tunnelled `fe`/`bridge` connection reaches this daemon as sshd's
    // Unix-socket side, so `WRITE_TIMEOUT` above never fires for it — a
    // half-open peer's death is otherwise noticed only when the OS-level
    // TCP side gives up (15 min to 2 h). `is_long_lived_role` flips true on
    // hello, exactly when this connection's declared role resolves to
    // `fe`/`bridge` (`cli`/`agent`, one-shot roles, stay false forever and
    // are never deadline-gated) — it is only ELIGIBILITY, not the gate
    // itself.
    //
    // Opt-in by ping (manager compatibility fix, post-review): the gate is
    // `deadline_armed`, which flips true only on this connection's FIRST
    // `ping` — never at hello. An `fe`/`bridge` peer too old to send ping
    // (a frontend box or a comm bridge that hasn't converged from main
    // yet) is eligible but never arms, so it keeps TODAY's behaviour
    // exactly: never reaped by this path. Arming at hello instead would
    // have dropped every such peer every 90s forever (endless reconnect
    // churn, a message-loss window each cycle) — the stale-roster defect
    // this step fixes then persists only for clients too old to ping,
    // which is correct and self-healing as they upgrade.
    //
    // Once armed, `read_deadline` is a fixed point in time — bumped
    // forward to `now + ping_read_deadline()` every time ANY frame is read
    // from this connection (not only `ping`; ordinary traffic is just as
    // much proof of life), never recomputed relative to "now" at each
    // select! poll, so re-entering select! every loop iteration doesn't
    // itself push it out. The initial `Instant::now()` here is never
    // acted on: the corresponding select! arm and the bump below are both
    // `deadline_armed`-gated, and that starts false.
    let mut is_long_lived_role = false;
    let mut deadline_armed = false;
    let mut read_deadline = tokio::time::Instant::now();

    // This connection's active workspace, made EXPLICIT via `workspace.activate`
    // (`op::WORKSPACE_ACTIVATE`) — the frontend's single "switch chrome" entry
    // point (`switch_to_workspace`, ui/session/switch.rs) fires it UNCONDITIONALLY as the
    // very first wire action of any switch, including a UI-cache hit that
    // fires no other request at all, and a switch back to the default
    // workspace. Also fired once on reconnect, right after `hello` succeeds.
    // A prior revision of this fix INFERRED the active workspace from
    // whichever op's `workspace_id` happened to arrive next — dropped
    // (Codex review): a cache hit fired no op, so the daemon could sit on a
    // stale workspace indefinitely, and a client reconnect's initial
    // default-workspace fetch could clobber a resumed non-default workspace
    // before the frontend got around to re-requesting it.
    //
    // Stores ONLY the canonical `workspace_id` (never a cached slug/root
    // tuple): resolving fresh at every use (`preview_changed_visible`) is
    // what makes a same-slug reinsertion, an uncanonical stored root, or a
    // stale slug match after a destroy-then-recreate impossible to leak —
    // there is no cache left to go stale.
    //
    // `None` = never activated (a fresh connection, before its first
    // `workspace.activate`) — keeps seeing every `preview.changed` event,
    // exactly as before this filter existed. `Some(id)` where `id` no longer
    // RESOLVES (the workspace was destroyed since activation, or the
    // activate itself named something that never resolved) is a DIFFERENT
    // state — `preview_changed_visible` drops every event in that case
    // rather than falling back to "send everything": the connection told us
    // it was viewing a specific workspace, so there is no view left to serve
    // traffic to until the next successful activate.
    let mut active_workspace: Option<String> = None;

    // One file-watcher subscription per connection. Each connection writes
    // its own preview.changed evt frame; the broadcast channel's per-
    // receiver lag detection lets us notice and log if this connection is
    // falling behind editor saves.
    let mut watcher_rx = Some(preview_changed_tx.subscribe());

    // One workspace-lifecycle subscription per connection. Always present
    // (the channel is created unconditionally in `run`), unlike the file
    // watcher which can fail to start. Each connection writes its own
    // `workspace.changed` evt frame; the broadcast's per-receiver lag
    // detection surfaces a connection that fell behind.
    let mut ws_events_rx = ws_events_tx.subscribe();

    // One topology subscription per connection, same shape as the
    // workspace-lifecycle bus above (plan §B): each connection writes its
    // own `topology.changed` evt frame so Hosts mode refreshes live.
    let mut topo_changed_rx = topo_changed_tx.subscribe();

    // One agent-relay subscription per connection. Like the workspace bus
    // it's always present (channel created unconditionally in `run`). Each
    // connection writes its own `agent.message` evt frame; the broadcast's
    // per-receiver lag detection surfaces a connection that fell behind.
    let mut agent_events_rx = agent_events_tx.subscribe();

    // One receipt subscription per connection (ADR 0048), beside the agent
    // bus and created the same unconditional way. Every connection gets
    // every receipt; the sender recognizes its own by `id` and ignores the
    // rest — the daemon holds no delivery state to route by.
    let mut agent_receipt_rx = agent_receipt_tx.subscribe();

    // One FE-command subscription per connection (ADR 0025). Like the agent
    // bus it's always present (channel created unconditionally in `run`). Each
    // connection writes its own `fe.command` evt frame; the broadcast's
    // per-receiver lag detection surfaces a connection that fell behind.
    let mut fe_command_rx = fe_command_tx.subscribe();

    // One REPL-frame subscription per connection. Streamed eval frames arrive
    // here (published by the per-workspace REPL supervisor); each connection
    // writes its own `repl.frame` evt frame. Like the agent bus, always
    // present; the broadcast's per-receiver lag detection surfaces a
    // connection that fell behind a fast-printing eval.
    let mut repl_frame_rx = repl_frame_tx.subscribe();

    // Monitor tick bus (ADR 0020). Sampling is always-on; this connection only
    // forwards ticks while its drawer is open — `monitor.subscribe` flips the
    // flag, `monitor.unsubscribe` clears it. The receiver is still polled while
    // unsubscribed so it never lags (the client gets fresh ticks on subscribe).
    // Optional like `watcher_rx`: `None` if the hub wasn't installed.
    let mut monitor_rx = workspaces.monitor_hub().map(|h| h.subscribe());
    let mut monitor_subscribed = false;

    // Off-loop jobs (switch-latency Phase 1, joined by `kernel.request` in
    // the kernel-dead-pane-starvation fix): `preview.get` / `concept.read`
    // / `image.crop` / `kernel.request` run as their own tasks in `jobs`,
    // bounded to `OFFLOOP_CONCURRENCY` concurrent (the semaphore is acquired INSIDE
    // each job, never here, so queuing one never blocks reading the next
    // frame). `tx` stays this loop's alone — a job has no way to reach the
    // socket, so it hands its finished reply to `out_tx` and the loop
    // (`out_rx`, drained in the select below) writes it exactly like an
    // inline reply. Dropping `jobs` (this function returning, any path)
    // aborts whatever's still running — no leaked tasks.
    let (out_tx, mut out_rx): (OutTx, _) = mpsc::channel(OFFLOOP_CONCURRENCY);
    let mut jobs: JoinSet<()> = JoinSet::new();
    let job_sem = Arc::new(Semaphore::new(OFFLOOP_CONCURRENCY));

    loop {
        // ADR 0035: the peeked first frame (any non-proxy first frame, e.g.
        // hello) is dispatched here before the first socket read, so the
        // proxy detection above costs the control path nothing.
        let frame = if let Some((f, _blob)) = pending_first.take() {
            f
        } else {
            match select_once(
                &mut tx, &mut read_fut, &mut out_rx, &mut watcher_rx, &mut ws_events_rx, &mut topo_changed_rx,
                &mut agent_events_rx, &mut agent_receipt_rx, &mut fe_command_rx, &mut repl_frame_rx, &mut monitor_rx,
                &mut jobs, read_deadline, deadline_armed, monitor_subscribed, &active_workspace, &workspaces,
                &client_guard,
            )
            .await?
            {
                Woke::Read(done) => {
                    let (rx_back, wire) = done;
                    read_fut = Some(Box::pin(read_owned(rx_back)));
                    match wire {
                        Ok((f, _blob)) => f,
                        Err(e) => {
                            tracing::debug!(error = %e, "read_frame returned; closing");
                            return Ok(());
                        }
                    }
                }
                Woke::Again => continue,
                Woke::Reap => return Ok(()),
            }
        };

        // Any frame from an ARMED connection is proof of life — push the
        // reaper deadline back out (topology plan §F step 2). Deliberately
        // unconditional on the op: ordinary traffic counts exactly as much
        // as a `ping`, so a busy connection never needs one. Before this
        // connection's first `ping` (`deadline_armed` still false) this is
        // a no-op.
        if deadline_armed {
            read_deadline = tokio::time::Instant::now() + ping_read_deadline();
        }

        if frame.kind != Kind::Req {
            tracing::debug!(?frame.kind, op = %frame.op, "ignoring non-req frame");
            continue;
        }

        dispatch(
            &mut tx, frame, &session, &mathjax, &pluto, &files_mode, &label, &workspaces, &ws_events_tx,
            &agent_events_tx, &agent_receipt_tx, &fe_command_tx, &clients, &topology_store, &topo_changed_tx, &leases,
            &mut client_guard, &mut hello_host, &mut hello_name, &mut is_long_lived_role, &mut deadline_armed,
            &mut read_deadline, &mut active_workspace, &mut monitor_subscribed, &mut jobs, &job_sem, &out_tx,
        )
        .await?;
    }
}

/// What one pass of a control session's select produced.
enum Woke<R> {
    /// The read future finished: the reader back, and what it read.
    Read((tokio::io::BufReader<R>, Result<(Frame, Option<Vec<u8>>)>)),
    /// An event or a job reply was handled; wait again.
    Again,
    /// The read deadline passed: end the connection.
    Reap,
}

/// Waits for the next thing a control session must act on and handles every event but a read.
async fn select_once<R, W, F>(
    tx: &mut W, read_fut: &mut Option<std::pin::Pin<Box<F>>>,
    out_rx: &mut mpsc::Receiver<(Frame, Option<Vec<u8>>)>,
    watcher_rx: &mut Option<broadcast::Receiver<PreviewChanged>>,
    ws_events_rx: &mut broadcast::Receiver<WorkspaceChanged>,
    topo_changed_rx: &mut broadcast::Receiver<crate::topology::store::TopologyChanged>,
    agent_events_rx: &mut broadcast::Receiver<AgentMessage>, agent_receipt_rx: &mut broadcast::Receiver<AgentReceipt>,
    fe_command_rx: &mut broadcast::Receiver<FeCommandEvt>, repl_frame_rx: &mut broadcast::Receiver<ReplFrameMsg>,
    monitor_rx: &mut Option<broadcast::Receiver<HostLatest>>, jobs: &mut JoinSet<()>,
    read_deadline: tokio::time::Instant, deadline_armed: bool, monitor_subscribed: bool,
    active_workspace: &Option<String>, workspaces: &Workspaces, client_guard: &Option<crate::clients::ClientGuard>,
) -> Result<Woke<R>>
where
    W: AsyncWrite + Unpin,
    F: std::future::Future<Output = (tokio::io::BufReader<R>, Result<(Frame, Option<Vec<u8>>)>)>,
{
    tokio::select! {
        biased;
        Some((frame, blob)) = out_rx.recv() => {
            write_reply(tx, frame, blob).await?;
            Ok(Woke::Again)
        }
        done = read_fut.as_mut().expect("read_fut is always Some at loop top") => Ok(Woke::Read(done)),
        change = recv_watcher(watcher_rx) => {
            write_preview_changed(
                tx,
                change,
                active_workspace.as_deref(),
                &workspaces,
            )
            .await?;
            Ok(Woke::Again)
        }
        wsc = recv_ws_events(ws_events_rx) => {
            write_workspace_changed(tx, wsc).await?;
            Ok(Woke::Again)
        }
        tpc = recv_topo_changed(topo_changed_rx) => {
            write_topology_changed(tx, tpc).await?;
            Ok(Woke::Again)
        }
        msg = recv_agent_msg(agent_events_rx) => {
            write_agent_message(tx, msg).await?;
            Ok(Woke::Again)
        }
        rcp = recv_agent_receipt(agent_receipt_rx) => {
            write_agent_receipt(tx, rcp).await?;
            Ok(Woke::Again)
        }
        fc = recv_fe_command(fe_command_rx) => {
            write_fe_command(tx, fc, client_guard.as_ref().map(|g| g.serial())).await?;
            Ok(Woke::Again)
        }
        rf = recv_repl_frame(repl_frame_rx) => {
            write_repl_frame(tx, rf).await?;
            Ok(Woke::Again)
        }
        tick = recv_monitor(monitor_rx) => {
            if monitor_subscribed {
                write_monitor_tick(tx, tick).await?;
            }
            Ok(Woke::Again)
        }
        Some(res) = jobs.join_next(), if !jobs.is_empty() => {
            if let Err(e) = res {
                tracing::error!(error = %e, "off-loop job panicked");
            }
            Ok(Woke::Again)
        }
        // Topology plan §F step 2: an ARMED `fe`/`bridge` connection
        // (has sent at least one `ping`) that has since sent no
        // frame at all within `ping_read_deadline()` is dead — reap
        // it exactly like a clean EOF. Guarded on `deadline_armed`,
        // not merely `is_long_lived_role`, so this arm stays inert
        // — never even polled — before hello, for `cli`/`agent`
        // connections, AND for an `fe`/`bridge` peer that has never
        // sent a `ping` at all (opt-in by ping: see the arming
        // comment above `is_long_lived_role`'s declaration).
        () = tokio::time::sleep_until(read_deadline), if deadline_armed => {
            tracing::info!("no frame within the read deadline; reaping half-open connection");
            Ok(Woke::Reap)
        }
    }
}

/// `ping` (topology plan §F step 2): a bare liveness ack, no side effect
/// beyond answering. Resetting this connection's read deadline is done in
/// `server/dispatch.rs`'s dispatch loop, ON EVERY frame it reads from an `fe`/
/// `bridge` connection (not only `ping` ones) — this handler stays a pure
/// echo so it needs no registry access, unlike `fe.presence`.
pub async fn handle_ping(req_id: u64) -> Result<HandlerOutput> {
    Ok(vec![(
        Frame::res(
            req_id,
            op::PING,
            serde_json::to_value(sot_protocol::PingRes { ok: true })?,
        ),
        None,
    )])
}
