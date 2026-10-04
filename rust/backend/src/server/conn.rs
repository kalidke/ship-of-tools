//! One connection: its read-deadline reaper, its first-frame peek and its control loop (`handle_connection`).

use super::events::{
    recv_agent_msg, recv_agent_receipt, recv_fe_command, recv_monitor, recv_repl_frame, recv_topo_changed,
    recv_watcher, recv_ws_events, write_agent_message, write_agent_receipt, write_fe_command, write_monitor_tick,
    write_preview_changed, write_repl_frame, write_topology_changed, write_workspace_changed,
};
use super::reply::{
    canonicalize_workspace_id, finish_dispatch, spawn_job, write_reply, HandlerOutput, OutTx, OFFLOOP_CONCURRENCY,
};
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
fn ping_read_deadline() -> std::time::Duration {
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
fn test_slow_concept_read_delay() -> std::time::Duration {
    static DELAY_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let ms = *DELAY_MS.get_or_init(|| {
        std::env::var("SOT_TEST_SLOW_CONCEPT_READ_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    });
    std::time::Duration::from_millis(ms)
}

/// Writes one marker file per arrival/completion/wait-for-settle-cycle
/// under `<barrier path>.<kind>/`, so a test can poll an exact count
/// instead of inferring one from timing. `pub(crate)` -- also called
/// from `capsule_workspace::ensure_started`'s own reprobe loop (kind
/// `"waitforsettle"`), which is a different module but shares this
/// exact barrier-path convention. No-op unless `SOT_TEST_ACTIVATION_
/// BARRIER` is set.
pub(crate) fn record_test_activation_marker(kind: &str) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Ok(barrier_path) = std::env::var("SOT_TEST_ACTIVATION_BARRIER") else {
        return;
    };
    let dir = std::path::PathBuf::from(format!("{barrier_path}.{kind}"));
    let _ = std::fs::create_dir_all(&dir);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _ = std::fs::write(dir.join(format!("{}-{seq}", std::process::id())), b"");
}

/// Test-only barrier at the top of `pty.open`'s activation task: when
/// `SOT_TEST_ACTIVATION_BARRIER` names a path, blocks until the test
/// creates that file (not a guessed sleep), giving up past a 30s bound.
/// No-op in production.
async fn wait_for_test_activation_barrier() {
    let Ok(path) = std::env::var("SOT_TEST_ACTIVATION_BARRIER") else {
        return;
    };
    record_test_activation_marker("arrivals");
    let path = std::path::PathBuf::from(path);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !path.is_file() {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(path = ?path, "capsule activation test barrier: released by timeout, not by the test");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Read one frame while *owning* the buffered reader, handing it back with
/// the result. Lets the per-connection select! loop keep a single in-flight
/// read future across iterations so a cancelled select! *pauses* a mid-blob
/// read rather than dropping it — `codec::read_frame` is NOT cancellation-safe
/// (it reads the `\n` envelope then `read_exact`s the blob tail across two
/// awaits). Mirrors the frontend transport fix (commit 8746b74). See the
/// CANCEL-SAFETY note in `handle_connection`'s loop.
async fn read_owned<R: AsyncRead + Unpin>(
    mut rx: tokio::io::BufReader<R>,
) -> (tokio::io::BufReader<R>, Result<(Frame, Option<Vec<u8>>)>) {
    let res = codec::read_frame(&mut rx).await;
    (rx, res)
}

/// Generic over the AsyncRead/AsyncWrite halves (a relic of the two-transport
/// era that keeps this testable against in-memory duplex streams).

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
fn touch_person_input(clients: &Clients, guard: &Option<ClientGuard>) {
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
    topology_store: Arc<crate::topology_store::TopologyStore>,
    topo_changed_tx: broadcast::Sender<crate::topology_store::TopologyChanged>,
    peer_identity: sot_log::challenge::PeerAuthOutcome,
    leases: Arc<crate::lease::Leases>,
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
                return crate::proxy::handle_proxy_connect(
                    buffered,
                    tx,
                    f,
                    None,
                )
                .await;
            }
            // ADR 0045 decision 2: peeked on every host, exactly like
            // `lane_bridge.rs` itself is compiled on every host (macOS
            // wiring lane) — one attach path, local or remote, with no
            // platform where `lane.connect` silently falls through to
            // the "unknown op" answer instead.
            if f.kind == Kind::Req && f.op == op::LANE_CONNECT {
                tracing::info!("lane.connect — leaving control loop for a raw pipe");
                return crate::lane_bridge::handle_lane_connect(
                    buffered,
                    tx,
                    f,
                    None,
                    &workspaces,
                )
                .await;
            }
            // A lease (1.2) is a connection of its own: it never enters
            // the hello-gated loop, the reaper or any handler.
            if f.kind == Kind::Req && f.op == op::FE_LEASE {
                tracing::info!(?peer_identity, "fe.lease — a lease connection");
                let state_root = sot_log::state_dir::sot_state_dir();
                return crate::lease::hold(
                    buffered,
                    tx,
                    f,
                    peer_identity,
                    None,
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
    topology_store: Arc<crate::topology_store::TopologyStore>,
    topo_changed_tx: broadcast::Sender<crate::topology_store::TopologyChanged>, leases: Arc<crate::lease::Leases>,
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
    // yourself" refusal reads it (server.rs, `op::TOPOLOGY_SET`).
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
    // point (`switch_to_workspace`, gpu.rs) fires it UNCONDITIONALLY as the
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
            tokio::select! {
                biased;
                Some((frame, blob)) = out_rx.recv() => {
                    write_reply(&mut tx, frame, blob).await?;
                    continue;
                }
                done = read_fut.as_mut().expect("read_fut is always Some at loop top") => {
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
                change = recv_watcher(&mut watcher_rx) => {
                    write_preview_changed(
                        &mut tx,
                        change,
                        active_workspace.as_deref(),
                        &workspaces,
                    )
                    .await?;
                    continue;
                }
                wsc = recv_ws_events(&mut ws_events_rx) => {
                    write_workspace_changed(&mut tx, wsc).await?;
                    continue;
                }
                tpc = recv_topo_changed(&mut topo_changed_rx) => {
                    write_topology_changed(&mut tx, tpc).await?;
                    continue;
                }
                msg = recv_agent_msg(&mut agent_events_rx) => {
                    write_agent_message(&mut tx, msg).await?;
                    continue;
                }
                rcp = recv_agent_receipt(&mut agent_receipt_rx) => {
                    write_agent_receipt(&mut tx, rcp).await?;
                    continue;
                }
                fc = recv_fe_command(&mut fe_command_rx) => {
                    write_fe_command(&mut tx, fc, client_guard.as_ref().map(|g| g.serial())).await?;
                    continue;
                }
                rf = recv_repl_frame(&mut repl_frame_rx) => {
                    write_repl_frame(&mut tx, rf).await?;
                    continue;
                }
                tick = recv_monitor(&mut monitor_rx) => {
                    if monitor_subscribed {
                        write_monitor_tick(&mut tx, tick).await?;
                    }
                    continue;
                }
                // Same hygiene drain as the pty-present arm above.
                Some(res) = jobs.join_next(), if !jobs.is_empty() => {
                    if let Err(e) = res {
                        tracing::error!(error = %e, "off-loop job panicked");
                    }
                    continue;
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
                    return Ok(());
                }
            }
        };

        // Any frame from an ARMED connection is proof of life — push the
        // reaper deadline back out (topology plan §F step 2). Deliberately
        // unconditional on the op: ordinary traffic counts exactly as much
        // as a `ping`, so a busy connection never needs one. Before this
        // connection's first `ping` (`deadline_armed` still false) this is
        // a no-op; arming itself (flag + first deadline) happens in the
        // `op::PING` arm below, the moment role-eligibility (set at hello)
        // and a first ping coincide.
        if deadline_armed {
            read_deadline = tokio::time::Instant::now() + ping_read_deadline();
        }

        if frame.kind != Kind::Req {
            tracing::debug!(?frame.kind, op = %frame.op, "ignoring non-req frame");
            continue;
        }

        // Each arm evaluates to `Result<HandlerOutput>`; the containment block
        // after the match turns a handler `Err` into an error *frame* for this
        // request instead of letting it bubble out of `handle_connection` and
        // tear down the whole connection (pre-fix, one malformed payload for
        // any op dropped the socket and forced a full FE reconnect).
        // Per-request service time (switch-latency Phase 1, 2026-09-08): the
        // loop below awaits every handler inline, so one slow request delays
        // every later frame on this connection. Logged at info above
        // SLOW_REQUEST_MS so the culprit op is identified, never inferred
        // from a neighbouring log line.
        let dispatch_started = std::time::Instant::now();
        let dispatched: Result<handlers::HandlerOutput> = match frame.op.as_str() {
            op::HELLO => {
                // Register this connection in the client roster the first
                // time we learn its client_id (a reconnect re-sends hello
                // on the same connection — keep the original guard). Done
                // before `handle_hello` so `clients_connected` counts self.
                if client_guard.is_none() {
                    if let Ok(req) =
                        serde_json::from_value::<sot_protocol::HelloReq>(frame.payload.clone())
                    {
                        // Topology plan §F step 2: mark this connection
                        // ELIGIBLE for the read-deadline reaper -- exactly
                        // the two long-lived roles, `fe` and `bridge`
                        // (`cli`/`agent` are one-shot and stay ungated).
                        // This does NOT arm the deadline itself (manager
                        // compatibility fix, post-review) — only this
                        // connection's FIRST `ping` does that (`op::PING`
                        // arm below), so a peer too old to send one keeps
                        // today's behaviour exactly, never reaped by this
                        // path.
                        // A peer on another protocol is about to be
                        // refused by `handle_hello`'s gate: never enter
                        // the roster (it would be counted as a directed
                        // command's audience and listed by `version.query`
                        // while its hello stands refused). It gets the
                        // structured mismatch reply and nothing else.
                        if req.protocol == sot_protocol::PROTOCOL_VERSION {
                            is_long_lived_role = matches!(req.role.as_str(), "fe" | "bridge");
                            hello_host = req.host.clone();
                            hello_name = req.name.clone();
                            client_guard = Some(clients.register(
                                req.client_id,
                                req.app_version,
                                req.protocol,
                                req.role,
                                req.host,
                                req.instance,
                                req.name,
                            ));
                        }
                    }
                }
                handlers::handle_hello(
                    frame.id,
                    frame.payload,
                    &session,
                    &None,
                    &files_mode,
                    label.as_deref(),
                    &clients,
                )
                .await
            }
            op::TREE_ROOT => {
                handlers::handle_tree_root(frame.id, frame.payload, &session, &workspaces).await
            }
            op::TREE_CHILDREN => {
                handlers::handle_tree_children(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::NAV_TOGGLE_HIDDEN => {
                handlers::handle_nav_toggle_hidden(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::PREVIEW_GET => {
                // Off-loop (switch-latency Phase 1): a read-and-render of
                // the requested node. `preview.set_scale` stays INLINE just
                // below — it writes a `.scale.json` sidecar.
                let req_id = frame.id;
                let op_name = frame.op.clone();
                let mut payload = frame.payload;
                if !canonicalize_workspace_id(&mut tx, &workspaces, req_id, &op_name, &mut payload)
                    .await?
                {
                    continue;
                }
                let session = session.clone();
                let workspaces = workspaces.clone();
                spawn_job(
                    &mut jobs,
                    job_sem.clone(),
                    out_tx.clone(),
                    req_id,
                    op_name,
                    async move {
                        handlers::handle_preview_get(req_id, payload, &session, &workspaces).await
                    },
                );
                continue;
            }
            op::PREVIEW_SET_SCALE => {
                handlers::handle_preview_set_scale(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::IMAGE_CROP => {
                // Off-loop (switch-latency Phase 1): decodes the source
                // image and writes a NEW, uniquely-named capture file — it
                // never mutates any EXISTING shared state (the session
                // revision bump it also does is safe off-loop for the same
                // reason concurrent connections already interleave those
                // bumps: their order relative to wall-clock request order
                // was never guaranteed).
                let req_id = frame.id;
                let op_name = frame.op.clone();
                let mut payload = frame.payload;
                if !canonicalize_workspace_id(&mut tx, &workspaces, req_id, &op_name, &mut payload)
                    .await?
                {
                    continue;
                }
                let session = session.clone();
                let workspaces = workspaces.clone();
                spawn_job(
                    &mut jobs,
                    job_sem.clone(),
                    out_tx.clone(),
                    req_id,
                    op_name,
                    async move {
                        handlers::handle_image_crop(req_id, payload, &session, &workspaces).await
                    },
                );
                continue;
            }
            op::MATH_RENDER => {
                handlers::handle_math_render(frame.id, frame.payload, &session, &mathjax).await
            }
            op::PLUTO_OPEN => {
                handlers::handle_pluto_open(frame.id, frame.payload, &session, &pluto, &workspaces)
                    .await
            }
            op::VIDEO_OPEN => {
                handlers::handle_video_open(frame.id, frame.payload, &session).await
            }
            op::DOCS_OPEN => {
                // Per-connection site root (ADR 0029): this connection's serial
                // selects/owns its docs-map entry and becomes the URL's first path
                // segment. `None` only before hello registers the guard, which
                // always precedes docs.open in practice.
                let serial = client_guard.as_ref().map(|g| g.serial());
                handlers::handle_docs_open(frame.id, frame.payload, &session, serial, &workspaces)
                    .await
            }
            op::QUARTO_OPEN => {
                handlers::handle_quarto_open(frame.id, frame.payload, &session).await
            }
            op::FILE_UPLOAD => handlers::handle_file_upload(frame.id, frame.payload).await,
            op::FILE_DOWNLOAD => {
                // Streams chunk frames straight to the socket (bounded memory),
                // so it writes its own frames and skips the response-write below.
                handlers::stream_file_download(&mut tx, frame.id, frame.payload).await?;
                continue;
            }
            op::KERNEL_REQUEST => {
                // Off-loop: this op used to await
                // `handlers::handle_kernel_request(...)` INLINE, in this
                // same per-connection dispatch loop that
                // also carries this connection's `pty` byte stream — a
                // `kernel.request` against a dead/slow kernel held up
                // dispatch of the NEXT frame on this connection, including
                // a `pty.write`/`pty.open` for the same session's attached
                // pane (the frontend multiplexes both over one connection
                // per host). `preview.get`/`concept.read`/`image.crop` were
                // already off-loop for the identical reason; this joins
                // their existing pool (`job_sem`, `OFFLOOP_CONCURRENCY`)
                // rather than adding a second cap for the same shape of
                // operation (a bounded external-process call). Note this is
                // NOT about `job_sem` ever being shared with pty ops —
                // pty.* dispatch inline just below and never touch it; the
                // actual shared choke point was the inline `.await` itself.
                let req_id = frame.id;
                let op_name = frame.op.clone();
                let mut payload = frame.payload;
                if !canonicalize_workspace_id(&mut tx, &workspaces, req_id, &op_name, &mut payload)
                    .await?
                {
                    continue;
                }
                let session = session.clone();
                let workspaces = workspaces.clone();
                spawn_job(
                    &mut jobs,
                    job_sem.clone(),
                    out_tx.clone(),
                    req_id,
                    op_name,
                    async move {
                        handlers::handle_kernel_request(req_id, payload, &session, &workspaces)
                            .await
                    },
                );
                continue;
            }
            op::CONCEPT_READ => {
                // Off-loop (switch-latency Phase 1): a read of one
                // `.concept/` annotation file. `concept.write`/`concept.list`
                // stay INLINE (write, and directory-walk-then-read).
                let req_id = frame.id;
                let op_name = frame.op.clone();
                let mut payload = frame.payload;
                if !canonicalize_workspace_id(&mut tx, &workspaces, req_id, &op_name, &mut payload)
                    .await?
                {
                    continue;
                }
                let session = session.clone();
                let workspaces = workspaces.clone();
                spawn_job(
                    &mut jobs,
                    job_sem.clone(),
                    out_tx.clone(),
                    req_id,
                    op_name,
                    async move {
                        // Test-only (see `test_slow_concept_read_delay`): a
                        // no-op sleep unless a test set the env var.
                        let delay = test_slow_concept_read_delay();
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        handlers::handle_concept_read(req_id, payload, &session, &workspaces).await
                    },
                );
                continue;
            }
            op::CONCEPT_WRITE => {
                handlers::handle_concept_write(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::CONCEPT_LIST => {
                handlers::handle_concept_list(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::FILE_READ => {
                handlers::handle_file_read(frame.id, frame.payload, &session, &workspaces).await
            }
            op::FILE_WRITE => {
                handlers::handle_file_write(frame.id, frame.payload, &session, &workspaces).await
            }
            op::FILE_DELETE => {
                handlers::handle_file_delete(frame.id, frame.payload, &session, &workspaces).await
            }
            op::DIR_CREATE => {
                handlers::handle_dir_create(frame.id, frame.payload, &session, &workspaces).await
            }
            op::REPL_EVAL => {
                handlers::handle_repl_eval(frame.id, frame.payload, &session, &workspaces).await
            }
            op::REPL_RUN_FILE => {
                handlers::handle_repl_run_file(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::REPL_INTERRUPT => {
                handlers::handle_repl_interrupt(frame.id, frame.payload, &session, &workspaces)
                    .await
            }
            op::REPL_EXECUTE => {
                handlers::handle_repl_execute(frame.id, frame.payload, &session, &workspaces).await
            }
            op::DIRECTORY_LIST => {
                handlers::handle_directory_list(frame.id, frame.payload, &session).await
            }
            op::WORKSPACE_CREATE => {
                handlers::handle_workspace_create(
                    frame.id,
                    frame.payload,
                    &session,
                    &workspaces,
                    &ws_events_tx,
                )
                .await
            }
            op::WORKSPACE_LIST => {
                handlers::handle_workspace_list(frame.id, frame.payload, &workspaces).await
            }
            op::ACCOUNTS_LIST => handlers::handle_accounts_list(frame.id, frame.payload).await,
            // ADR 0046 decision 6: the ONE op whose reply must be written
            // before its effect runs, because the caller IS the session
            // being replaced. Written here rather than through the common
            // path below (same reason `PTY_OPEN`'s arm writes its own) so
            // the kill cannot precede the ack.
            op::WORKSPACE_REAUTH => {
                let (out, restart) =
                    crate::reauth::handle_workspace_reauth(frame.id, frame.payload, &workspaces).await?;
                // Both halves of the ordering live in `write_accept_then`,
                // which a test pins: the frame goes out first, and a write
                // that fails rolls the record back before the `?` here ends
                // the connection.
                crate::reauth::write_accept_then(&mut tx, &out, restart, |plan| {
                    // Detached: this connection is about to lose its peer,
                    // and the restart holds the row's guard for its whole
                    // duration wherever it runs.
                    tokio::spawn(async move {
                        if let Err(e) =
                            tokio::task::spawn_blocking(move || {
                                crate::reauth::restart_blocking(plan, &crate::reauth::LiveSupervisor)
                            })
                            .await
                        {
                            tracing::warn!(error = %e, "workspace.reauth: the restart task panicked");
                        }
                    });
                })
                .await?;
                continue;
            }
            op::WORKSPACE_ACTIVATE => {
                // Update `active_workspace` (declared above) HERE, inline —
                // same pattern as HELLO's auth flag just above: peek the raw
                // JSON for `workspace_id` before the typed parse the handler
                // does again, so a malformed payload still reaches the
                // handler's ordinary error path instead of silently
                // skipping the state update.
                //
                // Store the RESOLVED canonical id when it resolves. When it
                // doesn't (a stale id, or a race with a concurrent destroy),
                // record the raw hint verbatim rather than leaving the
                // PREVIOUS activation in place — the frontend just told us
                // its view moved off that workspace, so continuing to
                // filter by the stale one would leak the old view's events
                // into the new one. `preview_changed_visible` re-resolves at
                // write time and drops everything for an id that still
                // doesn't resolve then.
                let hinted = frame.payload.get("workspace_id").and_then(|v| v.as_str());
                active_workspace = Some(
                    workspaces
                        .resolve(hinted)
                        .map(|ws| ws.workspace_id.clone())
                        .unwrap_or_else(|| hinted.unwrap_or_default().to_string()),
                );
                handlers::handle_workspace_activate(frame.id, frame.payload, &workspaces).await
            }
            op::AGENT_SEND => {
                handlers::handle_agent_send(
                    frame.id,
                    frame.payload,
                    &agent_events_tx,
                    &clients,
                    client_guard.as_ref().map(|g| g.serial()),
                )
                .await
            }
            op::AGENT_FILED => {
                // The filer is this connection's DECLARED hello name, read
                // here and nowhere else — the request body cannot name one
                // (ADR 0048). `hello_name` is the same local `hello_host`
                // is kept as, recorded before `register` consumes the req.
                handlers::handle_agent_filed(
                    frame.id,
                    frame.payload,
                    &agent_receipt_tx,
                    hello_name.as_deref(),
                )
                .await
            }
            op::COMM_FILE => {
                handlers::handle_comm_file(frame.id, frame.payload, &workspaces).await
            }
            op::AGENT_JOIN => {
                handlers::handle_agent_join(frame.id, frame.payload, &workspaces, &ws_events_tx)
                    .await
            }
            op::FE_COMMAND_SEND => {
                handlers::handle_fe_command_send(frame.id, frame.payload, &fe_command_tx, &clients)
                    .await
            }
            op::FE_PRESENCE => {
                // The ONLY place `last_person_input_at` is stamped from
                // (2026-09-08 review rework, design point A) — see
                // `touch_person_input`'s doc for why every other op that
                // used to stamp it was removed instead of patched.
                touch_person_input(&clients, &client_guard);
                handlers::handle_fe_presence(frame.id).await
            }
            op::FE_SESSIONS => {
                handlers::handle_fe_sessions(
                    frame.id,
                    frame.payload,
                    &clients,
                    client_guard.as_ref().map(|g| g.serial()),
                )
                .await
            }
            op::PING => {
                // Opt-in arming (manager compatibility fix, post-review):
                // this connection's FIRST `ping`, and only if hello already
                // marked it role-eligible, arms the read-deadline reaper --
                // never hello itself. A peer that never pings (an old
                // frontend or comm bridge not yet converged from main)
                // stays permanently unarmed and keeps today's behaviour:
                // never reaped by this path. Once armed, the generic bump
                // above keeps pushing `read_deadline` out on every
                // subsequent frame, `ping` included.
                if is_long_lived_role && !deadline_armed {
                    deadline_armed = true;
                    read_deadline = tokio::time::Instant::now() + ping_read_deadline();
                }
                handlers::handle_ping(frame.id).await
            }
            op::UPDATE_CHECK => crate::update::handle_update_check(frame.id).await,
            op::UPDATE_APPLY => {
                crate::update::handle_update_apply(frame.id, &fe_command_tx, &leases).await
            }
            op::VERSION_QUERY => {
                handlers::handle_version_query(frame.id, &clients, &topology_store, &topo_changed_tx)
                    .await
            }
            op::TOPOLOGY_SET => {
                crate::topology_set::handle_topology_set(
                    frame.id,
                    frame.payload,
                    &topology_store,
                    &workspaces,
                    &crate::workspaces::declared_host(),
                    hello_host.as_deref(),
                    &topo_changed_tx,
                )
                .await
            }
            op::WORKSPACE_DESTROY => {
                handlers::handle_workspace_destroy(
                    frame.id,
                    frame.payload,
                    &session,
                    &workspaces,
                    &ws_events_tx,
                )
                .await
            }
            op::PTY_OPEN => {
                let req: PtyOpenReq = match serde_json::from_value(frame.payload) {
                    Ok(r) => r,
                    Err(e) => {
                        let payload = serde_json::json!({
                            "error": format!("pty.open payload: {e}"),
                            "code": "bad_request",
                        });
                        write_frame_to(&mut tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                            .await?;
                        continue;
                    }
                };
                // Name validation (security review): an explicit `target`
                // becomes the real tmux session name — a `|`-containing one
                // would corrupt `tmux.rs`'s naive `|`-delimited
                // `list-sessions`/`list-panes` parsing for every session, not
                // just this one. `None` (the default target) is exempt: it's
                // the hardcoded `DEFAULT_TMUX_TARGET` constant, not
                // request-controlled.
                if let Some(t) = req.target.as_deref() {
                    if !handlers::valid_name(t) {
                        let payload = serde_json::json!({
                            "error": format!(
                                "invalid target {t:?} (want 1-64 chars of [A-Za-z0-9._-])"
                            ),
                            "code": "bad_target",
                        });
                        write_frame_to(&mut tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                            .await?;
                        continue;
                    }
                }
                let requested_target = req
                    .target
                    .as_deref()
                    .unwrap_or("");
                // A row's agent pane is a capsule (ADR 0046): `pty.open`
                // starts its supervisor when needed and answers
                // `attach_direct` with the `state_dir` the frontend attaches
                // to (L1b, the U3 client). An unknown target has nothing
                // to attach to.
                let Some(ws) = workspaces.workspace_for_tmux(requested_target) else {
                    let payload = serde_json::json!({
                        "error": format!("no workspace owns session {requested_target:?}"),
                        "code": "no_workspace",
                    });
                    write_frame_to(&mut tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                        .await?;
                    continue;
                };
                let state_root = sot_log::state_dir::sot_state_dir();
                // `attach_direct` answers at once from memory, no
                // lane probe here -- `ensure_started` runs
                // fire-and-forget in the background under its own
                // guard, so a stale cached `Ready` never blocks it.
                {
                    match state_root.clone() {
                        None => {
                            ws.set_activation_error(Some(format!(
                                "could not resolve this machine's state root ({} unset)",
                                crate::capsule_workspace::STATE_ROOT_HINT
                            )));
                        }
                        Some(root) => {
                            let workspace_id = ws.workspace_id.clone();
                            let workspace_id_for_log = workspace_id.clone();
                            let agent_kind = ws.agent();
                            let agent_name = ws.agent_name();
                            let slug = ws.slug.clone();
                            let project_root = ws.project_root.clone();
                            let workspaces_for_start = workspaces.clone();
                            tokio::spawn(async move {
                                wait_for_test_activation_barrier().await;
                                let result = tokio::task::spawn_blocking(move || {
                                    crate::capsule_workspace::ensure_started(
                                        &root,
                                        &workspace_id,
                                        &agent_kind,
                                        &agent_name,
                                        &slug,
                                        &project_root,
                                        crate::capsule_workspace::ActivationIntent::Selection,
                                        workspaces_for_start,
                                    )
                                })
                                .await
                                .unwrap_or_else(|e| {
                                    Err(format!("capsule start-on-attach task panicked: {e}"))
                                });
                                match result {
                                    Ok(Some(())) => {
                                        tracing::info!(workspace_id = %workspace_id_for_log, "pty.open: capsule supervisor started on attach");
                                    }
                                    Ok(None) => {}
                                    Err(detail) => {
                                        tracing::warn!(workspace_id = %workspace_id_for_log, error = %detail, "pty.open: capsule supervisor start-on-attach failed");
                                    }
                                }
                                record_test_activation_marker("completions");
                            });
                        }
                    }
                }
                let state_dir = state_root
                    .map(|root| crate::capsule_workspace::state_dir_for(&root, &ws.workspace_id))
                    .map(|p| p.to_string_lossy().into_owned());
                let payload = serde_json::json!({
                    "error": "this workspace's agent pane is a capsule; attach directly instead of pty.open",
                    "code": "attach_direct",
                    "state_dir": state_dir,
                });
                write_frame_to(&mut tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                    .await?;
                continue;
            }
            op::PTY_INPUT => {
                // ADR 0042 amendment (2026-09-07): answered, unlike
                // `PTY_WRITE` above (this connection's own pty, fire-and-
                // forget) — `PtyInputReq` is untouched by that arm, and
                // vice versa. `origin`, when present, is the handler's own
                // job to validate/prefer; this connection's `hello`
                // `client_id` is only the FALLBACK controller id.
                let default_controller_id = client_guard
                    .as_ref()
                    .map(|g| g.client_id().to_string())
                    .unwrap_or_default();
                handlers::handle_pty_input(frame.id, frame.payload, &workspaces, &default_controller_id)
                    .await
            }
            op::PTY_SCREEN => {
                // ADR 0042 amendment (2026-09-07): a watcher-only read —
                // no controller id needed, it never takes the pen.
                handlers::handle_pty_screen(frame.id, frame.payload, &workspaces).await
            }
            op::MONITOR_SUBSCRIBE => {
                // Open this connection's live tick delivery (sampling is
                // already running). Reply with the host roster + base cadence
                // so the frontend can lay out panels before the first tick.
                monitor_subscribed = true;
                let hosts = workspaces
                    .monitor_hub()
                    .map(|h| h.host_names())
                    .unwrap_or_default();
                let res = MonitorSubscribeRes {
                    interval_s: 1.0,
                    hosts,
                };
                Ok(vec![(
                    Frame::res(frame.id, op::MONITOR_SUBSCRIBE, serde_json::to_value(res)?),
                    None,
                )])
            }
            op::MONITOR_UNSUBSCRIBE => {
                monitor_subscribed = false;
                Ok(vec![(
                    Frame::res(frame.id, op::MONITOR_UNSUBSCRIBE, serde_json::json!({})),
                    None,
                )])
            }
            op::MONITOR_HISTORY => serde_json::from_value::<MonitorHistoryReq>(frame.payload)
                .context("monitor.history payload")
                .and_then(|req| {
                    let hosts = workspaces
                        .monitor_hub()
                        .map(|h| h.history(&req))
                        .unwrap_or_default();
                    let res = MonitorHistoryRes { hosts };
                    Ok(vec![(
                        Frame::res(frame.id, op::MONITOR_HISTORY, serde_json::to_value(res)?),
                        None,
                    )])
                }),
            other => {
                tracing::warn!(op = %other, "unknown op");
                let payload = serde_json::json!({ "error": format!("unknown op: {other}") });
                Ok(vec![(Frame::res(frame.id, other, payload), None)])
            }
        };

        // Service-time logging + per-request error containment (turns a
        // handler `Err` into one `handler_error` frame instead of ending the
        // connection) — shared with every off-loop job via `finish_dispatch`.
        let out_frames = finish_dispatch(&frame.op, frame.id, dispatch_started, dispatched);

        for (out_frame, out_blob) in out_frames {
            write_reply(&mut tx, out_frame, out_blob).await?;
        }
    }
}

/// `ping` (topology plan §F step 2): a bare liveness ack, no side effect
/// beyond answering. Resetting this connection's read deadline is done in
/// `server.rs`'s dispatch loop, ON EVERY frame it reads from an `fe`/
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
