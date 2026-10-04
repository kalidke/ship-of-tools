//! The control session's op table: `dispatch` routes one request frame to its owner and writes the reply.

use super::conn::{ping_read_deadline, test_slow_concept_read_delay, touch_person_input, wait_for_test_activation_barrier};
use super::reply::{canonicalize_workspace_id, finish_dispatch, spawn_job, write_reply, OutTx};
use super::*;

/// Answers one request frame: the op table, each arm calling its owner, then the reply write.
pub(super) async fn dispatch<W>(
    tx: &mut W, frame: Frame, session: &Session, mathjax: &MathJax, pluto: &Pluto, files_mode: &Arc<FilesMode>,
    label: &Arc<Option<String>>, workspaces: &Workspaces, ws_events_tx: &broadcast::Sender<WorkspaceChanged>,
    agent_events_tx: &broadcast::Sender<AgentMessage>, agent_receipt_tx: &broadcast::Sender<AgentReceipt>,
    fe_command_tx: &broadcast::Sender<FeCommandEvt>, clients: &Clients,
    topology_store: &Arc<crate::topology_store::TopologyStore>,
    topo_changed_tx: &broadcast::Sender<crate::topology_store::TopologyChanged>, leases: &Arc<crate::lease::Leases>,
    client_guard: &mut Option<crate::clients::ClientGuard>, hello_host: &mut Option<String>,
    hello_name: &mut Option<String>, is_long_lived_role: &mut bool, deadline_armed: &mut bool,
    read_deadline: &mut tokio::time::Instant, active_workspace: &mut Option<String>, monitor_subscribed: &mut bool,
    jobs: &mut JoinSet<()>, job_sem: &Arc<Semaphore>, out_tx: &OutTx,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
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
                        *is_long_lived_role = matches!(req.role.as_str(), "fe" | "bridge");
                        *hello_host = req.host.clone();
                        *hello_name = req.name.clone();
                        *client_guard = Some(clients.register(
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
            if !canonicalize_workspace_id(tx, &workspaces, req_id, &op_name, &mut payload)
                .await?
            {
                return Ok(());
            }
            let session = session.clone();
            let workspaces = workspaces.clone();
            spawn_job(
                jobs,
                job_sem.clone(),
                out_tx.clone(),
                req_id,
                op_name,
                async move {
                    handlers::handle_preview_get(req_id, payload, &session, &workspaces).await
                },
            );
            return Ok(());
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
            if !canonicalize_workspace_id(tx, &workspaces, req_id, &op_name, &mut payload)
                .await?
            {
                return Ok(());
            }
            let session = session.clone();
            let workspaces = workspaces.clone();
            spawn_job(
                jobs,
                job_sem.clone(),
                out_tx.clone(),
                req_id,
                op_name,
                async move {
                    handlers::handle_image_crop(req_id, payload, &session, &workspaces).await
                },
            );
            return Ok(());
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
            handlers::stream_file_download(tx, frame.id, frame.payload).await?;
            return Ok(());
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
            if !canonicalize_workspace_id(tx, &workspaces, req_id, &op_name, &mut payload)
                .await?
            {
                return Ok(());
            }
            let session = session.clone();
            let workspaces = workspaces.clone();
            spawn_job(
                jobs,
                job_sem.clone(),
                out_tx.clone(),
                req_id,
                op_name,
                async move {
                    handlers::handle_kernel_request(req_id, payload, &session, &workspaces)
                        .await
                },
            );
            return Ok(());
        }
        op::CONCEPT_READ => {
            // Off-loop (switch-latency Phase 1): a read of one
            // `.concept/` annotation file. `concept.write`/`concept.list`
            // stay INLINE (write, and directory-walk-then-read).
            let req_id = frame.id;
            let op_name = frame.op.clone();
            let mut payload = frame.payload;
            if !canonicalize_workspace_id(tx, &workspaces, req_id, &op_name, &mut payload)
                .await?
            {
                return Ok(());
            }
            let session = session.clone();
            let workspaces = workspaces.clone();
            spawn_job(
                jobs,
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
            return Ok(());
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
            crate::reauth::write_accept_then(tx, &out, restart, |plan| {
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
            return Ok(());
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
            *active_workspace = Some(
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
            if *is_long_lived_role && !*deadline_armed {
                *deadline_armed = true;
                *read_deadline = tokio::time::Instant::now() + ping_read_deadline();
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
                    write_frame_to(tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                        .await?;
                    return Ok(());
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
                    write_frame_to(tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                        .await?;
                    return Ok(());
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
                write_frame_to(tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                    .await?;
                return Ok(());
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
            write_frame_to(tx, &Frame::res(frame.id, op::PTY_OPEN, payload), None)
                .await?;
            return Ok(());
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
            *monitor_subscribed = true;
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
            *monitor_subscribed = false;
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
        write_reply(tx, out_frame, out_blob).await?;
    }
    Ok(())
}
