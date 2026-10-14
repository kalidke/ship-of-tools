//! The per-bus evt writers (`write_*`), each turning one broadcast item into its evt frame, and `recv_or_pending` for
//! the two buses a connection holds as `Option` (`Some` in `run`'s connections; `None` only in tests).

use super::*;

/// The next item of a bus a connection holds as an `Option` (the watcher and
/// monitor receivers; `None` only in tests); pending forever when `rx` is
/// `None`, so its arm never fires.
pub(super) async fn recv_or_pending<T: Clone>(
    rx: &mut Option<broadcast::Receiver<T>>,
) -> Result<T, broadcast::error::RecvError> {
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
///   (`rust/frontend/src/ui/preview/pane.rs`), just evaluated once at fan-out instead of
///   once per received-and-discarded frame — (a) the event is tagged with
///   the active workspace's slug, or (b) workspaces overlap (umbrella roots
///   registered over the same tree, watch budgets capping a watcher's
///   coverage) so the event's absolute path lies under the active
///   workspace's CANONICAL root (`FilesMode::root_path`, not the raw stored
///   `project_root`) even when tagged with a different slug. A tag-only
///   filter would break case (b) — that's why the frontend never used one,
///   and why this mirrors its rule instead of inventing a simpler one.
///
/// Containment reuses `crate::files::confine::path_within_root` (component-aware; correctly
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
            change.path != root && crate::files::confine::path_within_root(&change.path, root)
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

/// Translates one topology write into a `topology.changed` evt frame.
/// Mirrors `write_workspace_changed`.
pub(super) async fn write_topology_changed<W>(
    tx: &mut W,
    change: Result<crate::topology::store::TopologyChanged, broadcast::error::RecvError>,
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
            let payload = serde_json::json!({
                "from": m.from,
                "to": m.to,
                "text": m.text,
                "ts": m.ts,
            });
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

/// Translates one streamed REPL frame into a `repl.frame` evt frame on the
/// wire. Returns `Ok(true)` if a frame was written, `Ok(false)` if the
/// receiver was lagged or closed (skip and keep the connection alive). The
/// `frame` value is passed through verbatim — its `{kind, ...}` shape is
/// kernel-defined, so the backend stays oblivious to new frame kinds — except
/// the one rewrite `repl_frame_for` makes to a page every frontend would open.
pub(super) async fn write_repl_frame<W>(
    tx: &mut W,
    msg: Result<ReplFrameMsg, broadcast::error::RecvError>,
    my_serial: Option<u64>,
    clients: &Clients,
) -> Result<bool>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    match msg {
        Ok(m) => {
            let payload = serde_json::json!({
                "eval_id": m.eval_id,
                "workspace_id": m.workspace_id,
                "frame": repl_frame_for(&m, my_serial, clients),
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

/// The frame `m` is written as to the connection `my_serial`. A `browser` frame every frontend would open (`open`
/// true or absent: `wglshow(fig)`) opens on one window, the one a show from the REPL's row goes to
/// (`ClientsSnapshot::show_target`): two browsers on one `wglshow` figure resize its one scene against each other.
/// The window is chosen once per frame, by the first connection that writes it (`ReplFrameMsg::opener`), and every
/// other connection is sent the frame with `open: false` and no `fe`. Every other frame is written as it came.
fn repl_frame_for(m: &ReplFrameMsg, my_serial: Option<u64>, clients: &Clients) -> serde_json::Value {
    let mut frame = m.frame.clone();
    if frame["kind"] != "browser" || frame.get("open").is_some_and(|open| open != true) {
        return frame;
    }
    let opener = *m.opener.get_or_init(|| {
        let (to, why) = clients.snapshot_with_active().show_target(m.workspace_id.as_deref())?;
        tracing::info!(workspace_id = ?m.workspace_id, to = %to.handle, why, "repl browser frame opens on one window");
        Some(to.serial)
    });
    if opener != my_serial {
        if let Some(fields) = frame.as_object_mut() {
            fields.insert("open".into(), false.into());
            fields.remove("fe");
        }
    }
    frame
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
        use crate::files::watcher::{ChangeKind, PreviewChanged};
        use crate::rows::{Workspace, Workspaces};
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

    // `repl_frame_for`: a page every frontend would open goes out as it came to the one window a show from its row
    // goes to, and with `open: false` and no `fe` to every other connection, from one answer per frame. Every other
    // frame goes out as it came.
    mod browser_opener {
        use super::super::repl_frame_for;
        use crate::clients::Clients;
        use crate::sidecars::repl::ReplFrameMsg;
        use serde_json::{json, Value};

        fn msg(frame: &Value) -> ReplFrameMsg {
            ReplFrameMsg { eval_id: 1, workspace_id: Some("ws-x".into()), frame: frame.clone(), opener: Default::default() }
        }

        #[test]
        fn a_page_for_every_frontend_opens_on_one_window() {
            let clients = Clients::new();
            let fe_a = clients.register("a", "0.6.6", 1, "fe".into(), None, None, Some("fe@a".into()));
            let fe_b = clients.register("b", "0.6.6", 1, "fe".into(), None, None, Some("fe@b".into()));
            let (a, b) = (Some(fe_a.serial()), Some(fe_b.serial()));
            let page = json!({"kind": "browser", "url": "http://127.0.0.1:9/p", "open": true});
            let served = json!({"kind": "browser", "url": "http://127.0.0.1:9/p", "open": false});

            let m = msg(&page);
            assert_eq!(repl_frame_for(&m, b, &clients), page, "no window used: the one that connected last");
            assert_eq!(repl_frame_for(&m, a, &clients), served, "never every window");

            clients.touch_person_input(b.unwrap());
            clients.touch_person_input(a.unwrap());
            clients.show_row(b.unwrap(), "ws-x".into());
            let unset = json!({"kind": "browser", "url": "http://127.0.0.1:9/p"});
            let aimed = json!({"kind": "browser", "url": "http://127.0.0.1:9/p", "open": true, "fe": "fe@a"});
            for frame in [&page, &unset, &aimed] {
                let m = msg(frame);
                assert_eq!(repl_frame_for(&m, b, &clients), *frame, "the window showing the row: as it came");
                assert_eq!(repl_frame_for(&m, a, &clients), served, "{frame}");
                assert_eq!(repl_frame_for(&m, None, &clients), served, "{frame}");
            }

            let m = msg(&page);
            assert_eq!(repl_frame_for(&m, a, &clients), served);
            clients.show_row(b.unwrap(), "ws-y".into());
            assert_eq!(repl_frame_for(&m, b, &clients), page, "one answer per frame");
            assert_eq!(repl_frame_for(&m, a, &clients), served, "one answer per frame");

            let one = json!({"kind": "browser", "url": "http://127.0.0.1:9/p", "open": false, "fe": "fe@a"});
            let text = json!({"kind": "stdout", "text": "hi"});
            for frame in [&served, &one, &text] {
                assert_eq!(repl_frame_for(&msg(frame), a, &clients), *frame);
                assert_eq!(repl_frame_for(&msg(frame), b, &clients), *frame);
            }
        }
    }
}
