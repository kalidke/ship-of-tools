//! The control session's op table: `place` says where each request op runs, `dispatch` runs one request frame
//! there, and `serve` hands it to its owner.

use super::conn::{ping_read_deadline, test_slow_concept_read_delay};
use crate::rows::ops::pty::handle_pty_open;
use crate::rows::reauth::answer_workspace_reauth;
use crate::sidecars::ops::{handle_monitor_history, handle_monitor_subscribe, handle_monitor_unsubscribe};
use crate::topology::store::{TopologyChanged, TopologyStore};
use super::reply::{canonicalize_workspace_id, finish_dispatch, spawn_job, write_reply, HandlerOutput, OutTx};
use super::*;

/// Where a request op runs. Off the loop is the default: while an inline op runs, the loop reads nothing else,
/// answers no ping and writes no event, so no op runs inline that can wait on a child, a peer or an unbounded lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Place {
    /// In the connection's loop ([`INLINE`]).
    Inline,
    /// A job holding one of the connection's `OFFLOOP_CONCURRENCY` permits, its reply written by the loop
    /// ([`POOLED`]).
    Pooled,
    /// A job without a permit, its reply written by the loop: every op that [`INLINE`] and [`POOLED`] do not name.
    Job,
}

/// The ops a control session runs in its loop, each with its reason. An op is here only if it uses the loop's own
/// state or writer, or must keep its order on the connection, and it waits only on memory, the local disk, a capsule
/// op (5 s), the registry lock (1 s), a REPL slot settle (5 s), a kernel call made by `preview.set_scale` (10 s) or,
/// for `workspace.reauth` (sent only by a row's own client), a row's start lock: nothing that outlasts the window's
/// 90 s silence rule.
pub(super) const INLINE: &[(&str, &str)] = &[
    (op::PING, "the liveness answer never waits behind a job, and the first ping arms this connection's reaper"),
    (op::WORKSPACE_ACTIVATE, "it sets the loop's active workspace, which filters every later preview event"),
    (op::MONITOR_SUBSCRIBE, "it sets the loop's monitor flag"),
    (op::MONITOR_UNSUBSCRIBE, "it clears the loop's monitor flag"),
    (op::MONITOR_HISTORY, "it answers from memory after the subscribe sent before it, so the first ticks are not cleared by a later \
     history"),
    (op::FE_PRESENCE, "it stamps this connection's person-input time as it arrives"),
    (op::FE_SESSIONS, "a later declaration replaces an earlier one"),
    (op::PTY_OPEN, "it writes its reply on the loop's writer"),
    (op::WORKSPACE_REAUTH, "its reply goes out on the loop's writer before its effect runs (ADR 0046 decision 6)"),
    (op::FILE_DOWNLOAD, "it streams its chunks on the loop's writer, in order"),
    (op::PTY_INPUT, "keystrokes reach a row in the order they were sent"),
    (op::PTY_SCREEN, "a screen read follows the input sent before it"),
    (op::REPL_EVAL, "submissions reach a REPL in the order they were sent"),
    (op::REPL_RUN_FILE, "submissions reach a REPL in the order they were sent"),
    (op::AGENT_SEND, "UI notifications are published in the order they were sent"),
    (op::FILE_UPLOAD, "an upload's chunks are written in the order they were sent"),
    (op::FILE_WRITE, "a later request on this connection sees the write"),
    (op::FILE_DELETE, "a later request on this connection sees the delete"),
    (op::DIR_CREATE, "a later request on this connection sees the folder"),
    (op::CONCEPT_WRITE, "a later request on this connection sees the annotation"),
    (op::PREVIEW_SET_SCALE, "a preview.get sent after it sees the new scale"),
    (op::NAV_TOGGLE_HIDDEN, "a tree read sent after it sees the flag"),
    (op::TREE_ROOT, "the window applies tree replies newest-wins and relies on request order (`ui/nav/tree_store.rs`)"),
    (op::TREE_CHILDREN, "the window applies tree replies newest-wins and relies on request order (`ui/nav/tree_store.rs`)"),
    (op::DIRECTORY_LIST, "the picker applies the listing that echoes its path, and a `.` toggle sends two for one path"),
    (op::WORKSPACE_LIST, "the Sessions strip keeps the last list (`ui/control/replies.rs`)"),
];

const COSTLY: &str = "costly work, a kernel call or an image decode: one connection's burst must not starve the runtime";

/// The ops that take one of the connection's `OFFLOOP_CONCURRENCY` permits.
pub(super) const POOLED: &[(&str, &str)] = &[
    (op::PREVIEW_GET, COSTLY),
    (op::IMAGE_CROP, COSTLY),
    (op::KERNEL_REQUEST, COSTLY),
];

/// Where `op` runs: [`INLINE`], [`POOLED`], or else a job without a permit.
pub(super) fn place(op: &str) -> Place {
    if INLINE.iter().any(|(o, _)| *o == op) {
        Place::Inline
    } else if POOLED.iter().any(|(o, _)| *o == op) {
        Place::Pooled
    } else {
        Place::Job
    }
}

/// What a handler may use: the daemon's shared state and this connection's identity, owned so a job can take a copy
/// off the loop. The loop's own state (its writer, read deadline, active workspace and monitor flag) is not here.
#[derive(Clone)]
pub(super) struct Shared {
    pub(super) session: Session,
    pub(super) mathjax: MathJax,
    pub(super) pluto: Pluto,
    pub(super) workspaces: Workspaces,
    pub(super) ws_events_tx: broadcast::Sender<WorkspaceChanged>,
    pub(super) agent_events_tx: broadcast::Sender<AgentMessage>,
    pub(super) links: crate::comm::mail::links::Links,
    pub(super) fe_command_tx: broadcast::Sender<FeCommandEvt>,
    pub(super) clients: Clients,
    pub(super) topology_store: Arc<TopologyStore>,
    pub(super) topo_changed_tx: broadcast::Sender<TopologyChanged>,
    pub(super) leases: Arc<crate::lifecycle::lease::Leases>,
    /// This connection's roster serial and client id, from its hello.
    pub(super) serial: u64,
    pub(super) client_id: String,
    /// The hello's declared host.
    pub(super) hello_host: Option<String>,
}

/// Runs one request frame where [`place`] puts it: an inline op here, before the loop reads again, and any other as a
/// job whose reply returns over `out_tx`.
pub(super) async fn dispatch<W>(
    tx: &mut W, frame: Frame, shared: &Shared, deadline_armed: &mut bool,
    read_deadline: &mut tokio::time::Instant, active_workspace: &mut Option<String>, monitor_subscribed: &mut bool,
    jobs: &mut JoinSet<()>, job_sem: &Arc<Semaphore>, out_tx: &OutTx,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    // Per-request service time (switch-latency Phase 1, 2026-09-08): logged at info above SLOW_REQUEST_MS so the
    // culprit op is identified, never inferred from a neighbouring log line.
    let started = std::time::Instant::now();
    let (req_id, op_name) = (frame.id, frame.op.clone());
    // The ops that use the loop's own state or writer; every other op goes to `serve`, here or as a job.
    let served = match frame.op.as_str() {
        op::PING => {
            // Opt-in arming (manager compatibility fix, post-review):
            // this connection's FIRST `ping`, whatever its role, arms the
            // read-deadline reaper -- never hello itself. A peer that never
            // pings (an old frontend or comm bridge not yet converged from
            // main) stays permanently unarmed and keeps today's behaviour:
            // never reaped by this path.
            if !*deadline_armed {
                *deadline_armed = true;
                *read_deadline = tokio::time::Instant::now() + ping_read_deadline();
            }
            crate::server::conn::handle_ping(req_id).await
        }
        op::WORKSPACE_ACTIVATE => {
            // Update `active_workspace` HERE, inline —
            // peek the raw
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
                shared
                    .workspaces
                    .resolve(hinted)
                    .map(|ws| ws.workspace_id.clone())
                    .unwrap_or_else(|| hinted.unwrap_or_default().to_string()),
            );
            crate::rows::ops::list::handle_workspace_activate(req_id, frame.payload, &shared.workspaces).await
        }
        op::MONITOR_SUBSCRIBE => {
            // Open this connection's live tick delivery (sampling is
            // already running). Reply with the host roster + base cadence
            // so the frontend can lay out panels before the first tick.
            *monitor_subscribed = true;
            handle_monitor_subscribe(req_id, &shared.workspaces)
        }
        op::MONITOR_UNSUBSCRIBE => {
            *monitor_subscribed = false;
            handle_monitor_unsubscribe(req_id)
        }
        op::PTY_OPEN => return handle_pty_open(tx, frame, &shared.workspaces).await,
        // ADR 0046 decision 6: the ONE op whose reply must be written
        // before its effect runs, because the caller IS the session
        // being replaced.
        op::WORKSPACE_REAUTH => return answer_workspace_reauth(tx, frame, &shared.workspaces).await,
        // Streams chunk frames straight to the socket (bounded memory),
        // so it writes its own frames and skips the response-write below.
        op::FILE_DOWNLOAD => return crate::files::transfer::stream_file_download(tx, req_id, frame.payload).await,
        _ => match place(&frame.op) {
            Place::Inline => serve(frame, shared).await,
            off_loop => {
                let mut frame = frame;
                let permit = (off_loop == Place::Pooled).then(|| job_sem.clone());
                if permit.is_some()
                    && !canonicalize_workspace_id(tx, &shared.workspaces, req_id, &op_name, &mut frame.payload).await?
                {
                    return Ok(());
                }
                let shared = shared.clone();
                spawn_job(jobs, permit, out_tx.clone(), req_id, op_name, async move { serve(frame, &shared).await });
                return Ok(());
            }
        },
    };
    // Service-time logging + per-request error containment (turns a
    // handler `Err` into one `handler_error` frame instead of ending the
    // connection) — shared with every off-loop job via `finish_dispatch`.
    for (out_frame, out_blob) in finish_dispatch(&op_name, req_id, started, served) {
        write_reply(tx, out_frame, out_blob).await?;
    }
    Ok(())
}

/// Hands one request to its owner's handler: every op but the few `dispatch` keeps for the loop's own state.
#[allow(clippy::too_many_lines, reason = "the op table: one arm per request op, each calling its owner; predates the 100-line limit")]
async fn serve(frame: Frame, shared: &Shared) -> Result<HandlerOutput> {
    let Shared { session, mathjax, pluto, workspaces, ws_events_tx, clients, topology_store, topo_changed_tx, .. } =
        shared;
    let (req_id, payload) = (frame.id, frame.payload);
    // Each arm evaluates to `Result<HandlerOutput>`; `finish_dispatch`
    // turns a handler `Err` into an error *frame* for this request
    // instead of letting it bubble out of `handle_connection` and tear
    // down the whole connection (pre-fix, one malformed payload for any
    // op dropped the socket and forced a full FE reconnect).
    match frame.op.as_str() {
        op::TREE_ROOT => crate::files::tree_ops::handle_tree_root(req_id, payload, session, workspaces).await,
        op::TREE_CHILDREN => crate::files::tree_ops::handle_tree_children(req_id, payload, session, workspaces).await,
        op::NAV_TOGGLE_HIDDEN => {
            crate::files::tree_ops::handle_nav_toggle_hidden(req_id, payload, session, workspaces).await
        }
        op::PREVIEW_GET => crate::files::preview::handle_preview_get(req_id, payload, session, workspaces).await,
        op::PREVIEW_SET_SCALE => {
            crate::files::preview::scale::handle_preview_set_scale(req_id, payload, session, workspaces).await
        }
        // Decodes the source image and writes a NEW, uniquely-named capture
        // file — it never mutates any EXISTING shared state.
        op::IMAGE_CROP => crate::files::preview::crop::handle_image_crop(req_id, payload, session, workspaces).await,
        op::MATH_RENDER => crate::sidecars::ops::handle_math_render(req_id, payload, session, mathjax).await,
        op::PLUTO_OPEN => crate::sidecars::ops::handle_pluto_open(req_id, payload, session, pluto, workspaces).await,
        op::VIDEO_OPEN => crate::pages::ops::handle_video_open(req_id, payload, session).await,
        op::DOCS_OPEN => {
            // Per-connection site root (ADR 0029): this connection's serial
            // selects/owns its docs-map entry and becomes the URL's first path
            // segment.
            crate::pages::ops::handle_docs_open(req_id, payload, session, Some(shared.serial), workspaces).await
        }
        op::QUARTO_OPEN => crate::pages::ops::handle_quarto_open(req_id, payload, session).await,
        op::FILE_UPLOAD => crate::files::transfer::handle_file_upload(req_id, payload).await,
        op::KERNEL_REQUEST => crate::sidecars::ops::handle_kernel_request(req_id, payload, session, workspaces).await,
        op::CONCEPT_READ => {
            // Test-only (see `test_slow_concept_read_delay`): a no-op
            // sleep unless a test set the env var.
            let delay = test_slow_concept_read_delay();
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            crate::files::concept_ops::handle_concept_read(req_id, payload, session, workspaces).await
        }
        op::CONCEPT_WRITE => crate::files::concept_ops::handle_concept_write(req_id, payload, session, workspaces).await,
        op::CONCEPT_LIST => crate::files::concept_ops::handle_concept_list(req_id, payload, session, workspaces).await,
        op::FILE_READ => crate::files::io_ops::handle_file_read(req_id, payload, session, workspaces).await,
        op::FILE_WRITE => crate::files::io_ops::handle_file_write(req_id, payload, session, workspaces).await,
        op::FILE_DELETE => crate::files::io_ops::handle_file_delete(req_id, payload, session, workspaces).await,
        op::DIR_CREATE => crate::files::io_ops::handle_dir_create(req_id, payload, session, workspaces).await,
        op::REPL_EVAL => crate::sidecars::repl::ops::handle_repl_eval(req_id, payload, session, workspaces).await,
        op::REPL_RUN_FILE => {
            crate::sidecars::repl::ops::handle_repl_run_file(req_id, payload, session, workspaces).await
        }
        op::REPL_INTERRUPT => {
            crate::sidecars::repl::ops::handle_repl_interrupt(req_id, payload, session, workspaces).await
        }
        op::REPL_EXECUTE => {
            crate::sidecars::repl::execute::handle_repl_execute(req_id, payload, session, workspaces).await
        }
        op::DIRECTORY_LIST => crate::files::tree_ops::handle_directory_list(req_id, payload, session).await,
        op::WORKSPACE_CREATE => {
            crate::rows::ops::create::handle_workspace_create(req_id, payload, session, workspaces, ws_events_tx).await
        }
        op::WORKSPACE_LIST => crate::rows::ops::list::handle_workspace_list(req_id, payload, workspaces).await,
        op::ACCOUNTS_LIST => crate::agents::ops::handle_accounts_list(req_id, payload).await,
        op::AGENT_SEND => crate::comm::mail::relay::handle_agent_send(req_id, payload, &shared.agent_events_tx).await,
        op::COMM_FILE => crate::comm::mail::filer::handle_comm_file(req_id, payload, &shared.links).await,
        op::AGENT_JOIN => {
            crate::comm::registry::join::handle_agent_join(req_id, payload, workspaces, ws_events_tx).await
        }
        op::FE_COMMAND_SEND => {
            crate::clients::handle_fe_command_send(req_id, payload, &shared.fe_command_tx, clients).await
        }
        op::FE_PRESENCE => {
            // The ONLY place `last_person_input_at` is stamped from (2026-09-08 review rework, design point A): this op
            // alone is trustworthy evidence of a person, because the frontend sends it from its own real keyboard and
            // mouse input handlers, throttled there, never from a command file or another automated path. An earlier
            // design stamped it from ordinary navigation ops (`tree.root`, `preview.get`, `pty.write`, a
            // `workspace.activate` with `read: true`); every one of them had an automated producer too (reconnect
            // re-announces tree and preview requests, a badge-consuming `goto` fires them, an autostarted agent
            // writes into its own pane, a command-file `cycle_ws` could forge `read: true`), so they were deleted,
            // not patched.
            clients.touch_person_input(shared.serial);
            crate::clients::handle_fe_presence(req_id).await
        }
        op::FE_SESSIONS => crate::clients::handle_fe_sessions(req_id, payload, clients, Some(shared.serial)).await,
        op::UPDATE_CHECK => crate::update::handle_update_check(req_id).await,
        op::UPDATE_APPLY => crate::update::handle_update_apply(req_id, &shared.fe_command_tx, &shared.leases).await,
        op::VERSION_QUERY => {
            crate::clients::handle_version_query(req_id, clients, topology_store, topo_changed_tx).await
        }
        op::TOPOLOGY_SET => {
            crate::topology::set::handle_topology_set(
                req_id,
                payload,
                topology_store,
                workspaces,
                &crate::rows::store::declared_host(),
                shared.hello_host.as_deref(),
                topo_changed_tx,
            )
            .await
        }
        op::WORKSPACE_DESTROY => {
            crate::rows::ops::destroy::handle_workspace_destroy(req_id, payload, session, workspaces, ws_events_tx)
                .await
        }
        op::PTY_INPUT => {
            // ADR 0042 amendment (2026-09-07): answered.
            // `origin`, when present, is the handler's own
            // job to validate/prefer; this connection's `hello`
            // `client_id` is only the FALLBACK controller id.
            crate::rows::ops::pty::handle_pty_input(req_id, payload, workspaces, &shared.client_id).await
        }
        op::PTY_SCREEN => {
            // ADR 0042 amendment (2026-09-07): a watcher-only read —
            // no controller id needed, it never takes the pen.
            crate::rows::ops::pty::handle_pty_screen(req_id, payload, workspaces).await
        }
        op::MONITOR_HISTORY => handle_monitor_history(req_id, payload, workspaces),
        other => {
            tracing::warn!(op = %other, "unknown op");
            let payload = serde_json::json!({ "error": format!("unknown op: {other}") });
            Ok(vec![(Frame::res(req_id, other, payload), None)])
        }
    }
}

#[cfg(test)]
mod place_tests {
    use super::*;

    #[test]
    fn the_lists_are_disjoint_and_reasoned() {
        for (name, reason) in INLINE.iter().chain(POOLED) {
            assert!(!reason.is_empty(), "{name} has no reason");
        }
        for (name, _) in INLINE {
            assert!(POOLED.iter().all(|(o, _)| o != name), "{name} is in both lists");
        }
        assert_eq!(place(op::PING), Place::Inline);
        assert_eq!(place(op::WORKSPACE_LIST), Place::Inline);
        assert_eq!(place(op::QUARTO_OPEN), Place::Job);
        assert_eq!(place(op::PREVIEW_GET), Place::Pooled);
        assert_eq!(place("no.such"), Place::Job);
    }
}
