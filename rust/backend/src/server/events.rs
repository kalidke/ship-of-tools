//! The per-bus event pairs: each awaits one broadcast receiver (`recv_*`) and writes its evt frame (`write_*`).

use super::*;

/// Awaits the next file-change from the watcher subscription. When the
/// connection has no watcher (Watcher::spawn failed at startup) this future
/// stays pending forever, leaving the `tokio::select!` arm inactive.
pub(super) async fn recv_watcher(
    rx: &mut Option<broadcast::Receiver<PreviewChanged>>,
) -> Result<PreviewChanged, broadcast::error::RecvError> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

/// Whether a `preview.changed` event should reach a connection whose active
/// workspace is `active_workspace_id` — the CANONICAL id from this
/// connection's last `workspace.activate` (see `active_workspace`'s
/// declaration in `handle_connection`). Resolves it FRESH against
/// `workspaces` on every call rather than trusting a cached slug/root: a
/// same-slug reinsertion, an uncanonical stored root, or a stale slug match
/// after a destroy-then-recreate can otherwise leak a wrong answer.
///
/// - `None` = never activated (a fresh connection, before its first
///   `workspace.activate`) — keeps seeing every event, exactly as before
///   this filter existed.
/// - `Some(id)` that no longer resolves (the workspace was destroyed since
///   activation) drops EVERY event — deliberately not "send everything":
///   the connection told us it was viewing a specific workspace, and that
///   workspace is gone, so there is no view left to serve traffic to.
/// - `Some(id)` that resolves: the SAME two-path predicate
///   `resolve_preview_changed` applies frontend-side
///   (`rust/frontend/src/gpu.rs`), just evaluated once at fan-out instead of
///   once per received-and-discarded frame — (a) the event is tagged with
///   the active workspace's slug, or (b) workspaces overlap (umbrella roots
///   registered over the same tree, watch budgets capping a watcher's
///   coverage) so the event's absolute path lies under the active
///   workspace's CANONICAL root (`FilesMode::root_path`, not the raw stored
///   `project_root`) even when tagged with a different slug. A tag-only
///   filter would break case (b) — that's why the frontend never used one,
///   and why this mirrors its rule instead of inventing a simpler one.
///
/// Containment reuses `paths::path_within_root` (component-aware; correctly
/// rejects the lookalike sibling `/a/wsx` against root `/a/ws`, and — unlike
/// a bare `/`-only prefix check — handles a native Windows event path from
/// `notify`, which is `\`-separated). `path_within_root` treats the root
/// itself as contained; a `preview.changed` for the root path itself (rare —
/// a rename/touch of the project directory node) is intentionally excluded
/// here, matching this filter's original behaviour.
fn preview_changed_visible(
    change: &PreviewChanged,
    active_workspace_id: Option<&str>,
    workspaces: &Workspaces,
) -> bool {
    let Some(id) = active_workspace_id else {
        return true;
    };
    let Some(ws) = workspaces.resolve(Some(id)) else {
        return false;
    };
    if change.workspace_id.as_deref() == Some(ws.slug.as_str()) {
        return true;
    }
    // `files_mode()` errors only if the root has vanished from disk since
    // construction (or on the very first call, if it never canonicalized at
    // all) — fall back to the tag-only check above rather than guessing at
    // containment with an un-canonicalized path.
    match ws.files_mode() {
        Ok(fm) => {
            let root = fm.root_path();
            change.path != root && paths::path_within_root(&change.path, root)
        }
        Err(_) => false,
    }
}

/// Translates one watcher event into a `preview.changed` evt frame on the
/// wire — but only when `active_workspace_id` says this connection can use
/// it (`preview_changed_visible`); otherwise the event is silently dropped
/// here, before a frame decode/JSON-parse/redraw is spent on the frontend
/// for traffic it would have discarded anyway (the measured flood this
/// filter exists for). Returns `Ok(true)` if a frame was written, `Ok(false)`
/// if the receiver was lagged/closed, or if the event was filtered for this
/// connection's active workspace (skip and keep the connection alive either
/// way).
///
/// No `.with_rev(...)` on the outgoing frame, deliberately: `preview.changed`
/// is NOT part of the session revision ring (`watcher.rs` never calls
/// `Session::bump` for it) — see that module's header comment for why a
/// reconnect doesn't need to replay these. Stamping a revision here would
/// silently re-couple this event to the ring's watermark bookkeeping the
/// other half of that fix removes.
pub(super) async fn write_preview_changed<W>(
    tx: &mut W,
    change: Result<PreviewChanged, broadcast::error::RecvError>,
    active_workspace_id: Option<&str>,
    workspaces: &Workspaces,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match change {
        Ok(c) => {
            if !preview_changed_visible(&c, active_workspace_id, workspaces) {
                return Ok(false);
            }
            let payload = serde_json::json!({
                "path": c.path.to_string_lossy(),
                "node_id": c.node_id,
                "kind": c.kind.as_str(),
                "workspace_id": c.workspace_id,
            });
            let frame = Frame::evt(op::PREVIEW_CHANGED, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                "preview watcher lagged on this connection; client missed file events"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!("preview watcher channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next workspace lifecycle event. The channel is always present
/// (created unconditionally in `run`), so unlike `recv_watcher` this takes a
/// plain receiver rather than an `Option`.
pub(super) async fn recv_ws_events(
    rx: &mut broadcast::Receiver<WorkspaceChanged>,
) -> Result<WorkspaceChanged, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one workspace lifecycle event into a `workspace.changed` evt
/// frame on the wire. Returns `Ok(true)` if a frame was written, `Ok(false)`
/// if the receiver was lagged or closed (skip and keep the connection alive).
pub(super) async fn write_workspace_changed<W>(
    tx: &mut W,
    change: Result<WorkspaceChanged, broadcast::error::RecvError>,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match change {
        Ok(c) => {
            let payload = serde_json::json!({
                "action": c.action,
                "slug": c.slug,
                "workspace_id": c.workspace_id,
            });
            let frame = Frame::evt(op::WORKSPACE_CHANGED, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                "workspace event bus lagged on this connection; client missed workspace changes"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!("workspace event bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next topology write (plan §B). Same shape as `recv_ws_events`
/// — the channel is always present (created unconditionally in `run`).
pub(super) async fn recv_topo_changed(
    rx: &mut broadcast::Receiver<crate::topology_store::TopologyChanged>,
) -> Result<crate::topology_store::TopologyChanged, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one topology write into a `topology.changed` evt frame.
/// Mirrors `write_workspace_changed`.
pub(super) async fn write_topology_changed<W>(
    tx: &mut W,
    change: Result<crate::topology_store::TopologyChanged, broadcast::error::RecvError>,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match change {
        Ok(c) => {
            let payload = serde_json::json!({ "hash": c.hash });
            let frame = Frame::evt(op::TOPOLOGY_CHANGED, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                "topology event bus lagged on this connection; client missed a topology change"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!("topology event bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next relayed agent message. The channel is always present
/// (created unconditionally in `run`), so like `recv_ws_events` this takes a
/// plain receiver rather than an `Option`.
pub(super) async fn recv_agent_msg(
    rx: &mut broadcast::Receiver<AgentMessage>,
) -> Result<AgentMessage, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one relayed agent message into an `agent.message` evt frame on
/// the wire. Returns `Ok(true)` if a frame was written, `Ok(false)` if the
/// receiver was lagged or closed (skip and keep the connection alive).
pub(super) async fn write_agent_message<W>(
    tx: &mut W,
    msg: Result<AgentMessage, broadcast::error::RecvError>,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg {
        Ok(m) => {
            // `id` is what makes a receipt possible at all (ADR 0048):
            // without it in THIS payload the filer has nothing to attribute
            // its claim to, and the sender waits out its 5 s for a verdict
            // that can never arrive. Omitted when absent — an older sender
            // minted none.
            let mut payload = serde_json::json!({
                "from": m.from,
                "to": m.to,
                "text": m.text,
                "ts": m.ts,
            });
            if let Some(id) = m.id {
                payload["id"] = serde_json::Value::String(id);
            }
            let frame = Frame::evt(op::AGENT_MESSAGE, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                "agent relay bus lagged on this connection; client missed messages"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!("agent relay bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next filer receipt (ADR 0048). The channel is always present
/// (created unconditionally in `run`), so like `recv_agent_msg` this takes a
/// plain receiver rather than an `Option`.
pub(super) async fn recv_agent_receipt(
    rx: &mut broadcast::Receiver<AgentReceipt>,
) -> Result<AgentReceipt, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one filer receipt into an `agent.receipt` evt frame on the
/// wire. Mirrors `write_agent_message`. Two fields, both always present:
/// the frame's arrival is the claim, so there is nothing optional to omit.
pub(super) async fn write_agent_receipt<W>(
    tx: &mut W,
    rcp: Result<AgentReceipt, broadcast::error::RecvError>,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match rcp {
        Ok(r) => {
            let frame = Frame::evt(
                op::AGENT_RECEIPT,
                serde_json::json!({ "id": r.id, "filer": r.filer }),
            );
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                "agent receipt bus lagged on this connection; client missed receipts"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!("agent receipt bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next FE command (ADR 0025). The channel is always present
/// (created unconditionally in `run`), so like `recv_agent_msg` this takes a
/// plain receiver rather than an `Option`.
pub(super) async fn recv_fe_command(
    rx: &mut broadcast::Receiver<FeCommandEvt>,
) -> Result<FeCommandEvt, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one FE command into an `fe.command` evt frame on the wire
/// (ADR 0025). Returns `Ok(true)` if a frame was written OR correctly
/// filtered out for this connection, `Ok(false)` if the receiver was lagged
/// or closed (skip and keep the connection alive). The evt is broadcast to
/// every connection; ordinarily the FE self-filters on `target`, but when
/// `e.target_serial` names a specific connection (2026-09-08 review rework,
/// design point B — exclusive delivery by connection identity, not merely a
/// handle string that two connections could share) this connection drops
/// the event outright, without writing anything, unless its own `my_serial`
/// matches. An explicit `--fe <handle>` send carries `target_serial: None`
/// and keeps today's behaviour: every connection gets it and self-filters
/// on `target`.
pub(super) async fn write_fe_command<W>(
    tx: &mut W,
    evt: Result<FeCommandEvt, broadcast::error::RecvError>,
    my_serial: Option<u64>,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match evt {
        Ok(e) => {
            if let Some(want) = e.target_serial {
                if my_serial != Some(want) {
                    return Ok(true);
                }
            }
            let frame = Frame::evt(op::FE_COMMAND, serde_json::to_value(e)?);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                "fe command bus lagged on this connection; client missed commands"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!("fe command bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next streamed REPL frame. The channel is always present (created
/// unconditionally in `run`), so like `recv_agent_msg` this takes a plain
/// receiver rather than an `Option`.
pub(super) async fn recv_repl_frame(
    rx: &mut broadcast::Receiver<ReplFrameMsg>,
) -> Result<ReplFrameMsg, broadcast::error::RecvError> {
    rx.recv().await
}

/// Translates one streamed REPL frame into a `repl.frame` evt frame on the
/// wire. Returns `Ok(true)` if a frame was written, `Ok(false)` if the
/// receiver was lagged or closed (skip and keep the connection alive). The
/// `frame` value is passed through verbatim — its `{kind, ...}` shape is
/// kernel-defined, so the backend stays oblivious to new frame kinds.
pub(super) async fn write_repl_frame<W>(
    tx: &mut W,
    msg: Result<ReplFrameMsg, broadcast::error::RecvError>,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg {
        Ok(m) => {
            let payload = serde_json::json!({
                "eval_id": m.eval_id,
                "workspace_id": m.workspace_id,
                "frame": m.frame,
            });
            let frame = Frame::evt(op::REPL_FRAME, payload);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                "repl frame bus lagged on this connection; client missed frames"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!("repl frame bus channel closed");
            Ok(false)
        }
    }
}

/// Awaits the next monitor tick. Mirrors `recv_watcher`: when the hub wasn't
/// installed the receiver is `None` and this stays pending, leaving the
/// select! arm inactive.
pub(super) async fn recv_monitor(
    rx: &mut Option<broadcast::Receiver<HostLatest>>,
) -> Result<HostLatest, broadcast::error::RecvError> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

/// Translates one monitor tick into a `monitor.tick` evt (one host per evt; the
/// frontend merges by host). Skips on lag/close, keeping the connection alive.
pub(super) async fn write_monitor_tick<W>(
    tx: &mut W,
    msg: Result<HostLatest, broadcast::error::RecvError>,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg {
        Ok(m) => {
            let evt = MonitorTickEvt { hosts: vec![m] };
            let frame = Frame::evt(op::MONITOR_TICK, serde_json::to_value(evt)?);
            write_frame_to(tx, &frame, None).await?;
            Ok(true)
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            tracing::warn!(
                skipped = n,
                "monitor bus lagged on this connection"
            );
            Ok(false)
        }
        Err(broadcast::error::RecvError::Closed) => {
            tracing::debug!("monitor bus channel closed");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod agent_relay_wire_tests {
    use super::*;
    use crate::workspaces::{AgentMessage, AgentReceipt};

    fn one_frame(buf: &[u8]) -> serde_json::Value {
        let line = String::from_utf8(buf.to_vec()).expect("utf8 frame");
        serde_json::from_str(line.trim_end()).expect("one JSON line")
    }

    #[tokio::test]
    async fn agent_message_carries_the_id_only_when_the_sender_minted_one() {
        // The test that would have caught the payload being built by hand
        // (ADR 0048): `write_agent_message` uses an explicit `json!`, so a
        // new field on `AgentMessage` reaches the wire only if it is added
        // HERE. Without the id on the wire no filer can attribute a claim
        // and no receipt is ever possible.
        let msg = AgentMessage {
            from: "a".into(),
            to: "peer-otherbox".into(),
            text: "hi".into(),
            ts: "2026-01-01T00:00:00Z".into(),
            id: Some("x-1".into()),
        };
        let mut buf: Vec<u8> = Vec::new();
        assert!(write_agent_message(&mut buf, Ok(msg.clone()))
            .await
            .expect("write"));
        let f = one_frame(&buf);
        assert_eq!(f["op"], op::AGENT_MESSAGE);
        assert_eq!(f["payload"]["id"], "x-1");

        let mut buf2: Vec<u8> = Vec::new();
        let old = AgentMessage { id: None, ..msg };
        assert!(write_agent_message(&mut buf2, Ok(old))
            .await
            .expect("write"));
        let f2 = one_frame(&buf2);
        assert!(
            f2["payload"].get("id").is_none(),
            "an older sender minted no id; the key must be absent, got {}",
            f2["payload"]
        );
    }

    #[tokio::test]
    async fn agent_receipt_frame_is_the_id_and_the_filer_and_nothing_else() {
        let mut buf: Vec<u8> = Vec::new();
        assert!(write_agent_receipt(
            &mut buf,
            Ok(AgentReceipt { id: "x-1".into(), filer: "fe@otherbox".into() }),
        )
        .await
        .expect("write"));
        let f = one_frame(&buf);
        assert_eq!(f["op"], op::AGENT_RECEIPT);
        assert_eq!(f["kind"], "evt");
        assert_eq!(f["payload"]["id"], "x-1");
        assert_eq!(f["payload"]["filer"], "fe@otherbox");
        // No negative form on the wire: a frame that never arrives is the
        // only "not filed", and it is the sender's own conclusion.
        assert!(f["payload"].get("filed").is_none(), "got {}", f["payload"]);
        assert!(f["payload"].get("reason").is_none(), "got {}", f["payload"]);
    }
}

#[cfg(test)]
mod tests {
    // Per-connection preview.changed fan-out filter (the flood fix): a
    // connection must see exactly what its ACTIVATED workspace can use —
    // same workspace tag, or a foreign-tagged event whose path is under the
    // active root (workspaces overlap by design) — and nothing else, except
    // before any `workspace.activate` has landed at all (send everything)
    // and after an activated id stops resolving (send nothing until the
    // next activate). Real on-disk roots throughout: `preview_changed_visible`
    // resolves through `Workspace::files_mode()`, which canonicalizes
    // (paths.rs `simplify_verbatim`) — exactly the "uncanonical stored root"
    // failure mode this rework closes, so a literal `PathBuf` root would
    // test nothing.
    mod preview_changed_fanout {
        use super::super::preview_changed_visible;
        use crate::watcher::{ChangeKind, PreviewChanged};
        use crate::workspaces::{Workspace, Workspaces};
        use std::path::PathBuf;

        fn change(workspace_id: Option<&str>, path: PathBuf) -> PreviewChanged {
            PreviewChanged {
                path,
                node_id: Some("files:x".to_string()),
                kind: ChangeKind::Modified,
                workspace_id: workspace_id.map(str::to_string),
            }
        }

        /// A registry with one real, on-disk workspace named `slug`.
        /// Returns (registry, its CANONICAL workspace_id, its canonicalized
        /// root). Caller removes the returned root's directory when done.
        fn registry_with_workspace(tag: &str, slug: &str) -> (Workspaces, String, PathBuf) {
            let dir = std::env::temp_dir().join(format!(
                "sot-preview-fanout-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let workspaces = Workspaces::new();
            let ws = workspaces.insert(Workspace::from_label(
                slug,
                dir,
                false,
                "none".to_string(),
                String::new(),
                String::new(),
            ));
            let root = ws.files_mode().unwrap().root_path().to_path_buf();
            (workspaces, ws.workspace_id.clone(), root)
        }

        #[test]
        fn same_workspace_tag_is_sent() {
            let (workspaces, id, root) = registry_with_workspace("tag", "alpha");
            // Trusted on the tag alone — the path doesn't even need to be
            // real or nearby.
            let c = change(Some("alpha"), PathBuf::from("/anywhere/at/all"));
            assert!(preview_changed_visible(&c, Some(&id), &workspaces));
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn foreign_tag_under_active_root_is_sent() {
            // The umbrella-workspace / watch-budget case: tagged "beta" but
            // the path is really inside the active workspace's own
            // (canonical, on-disk) tree.
            let (workspaces, id, root) = registry_with_workspace("under", "alpha");
            let c = change(Some("beta"), root.join("src").join("main.jl"));
            assert!(preview_changed_visible(&c, Some(&id), &workspaces));
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn foreign_tag_elsewhere_is_dropped() {
            let (workspaces, id, root) = registry_with_workspace("elsewhere", "alpha");
            let c = change(Some("beta"), PathBuf::from("/repos/beta/src/main.jl"));
            assert!(!preview_changed_visible(&c, Some(&id), &workspaces));
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn no_active_workspace_yet_sends_everything() {
            // Fresh connection, before its first `workspace.activate` —
            // today's (pre-filter) behaviour. Registry is irrelevant: `None`
            // short-circuits before it's ever consulted.
            let workspaces = Workspaces::new();
            let c = change(Some("beta"), PathBuf::from("/repos/beta/src/main.jl"));
            assert!(preview_changed_visible(&c, None, &workspaces));
        }

        #[test]
        fn activated_id_no_longer_registered_drops_everything() {
            // The workspace was destroyed since this connection activated
            // it (or the activate itself named something that never
            // resolved) — drop, don't fall back to "send everything": the
            // registry has nothing to fall back TO.
            let workspaces = Workspaces::new();
            let c = change(Some("alpha"), PathBuf::from("/repos/alpha/src/main.jl"));
            assert!(!preview_changed_visible(
                &c,
                Some("ws-no-longer-exists"),
                &workspaces
            ));
        }
    }
}
