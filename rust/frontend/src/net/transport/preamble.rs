//! The connect preamble: after the hello, one tree.root and one preview.get of the default root.

use super::*;

/// The connect preamble's tree.root: returns the root node id, empty if refused.
pub(super) async fn preamble_tree_root<R, W, Wn>(
    mut tx: W,
    mut rx: R,
    tree_id: u64,
    host: &HostKey,
    session: &mut SessionState,
    emit: &impl Fn(IncomingEvt),
    window: &Wn,
) -> Result<String>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    Wn: Redraw,
{
    codec::write_frame(
        &mut tx,
        &Frame::req(
            tree_id,
            op::TREE_ROOT,
            serde_json::to_value(TreeRootReq {
                mode: "files".into(),
                workspace_id: None,
            })?,
        ),
        None,
    )
    .await?;
    let (frame, _) = codec::read_frame(&mut rx).await?;
    note_revision(frame.rev, &mut session.memory, host, &mut session.gate);
    // Hold the root node id so preview.get can target it without hardcoding
    // the backend's id-format conventions in the frontend. Today files mode
    // uses `files:` for the root; that may change.
    let mut root_node_id = String::new();
    if frame.id == tree_id && frame.payload.get("error").is_some() {
        // An unreadable default directory is no reason to end the session:
        // that would cycle the transport and flap the link gate.
        tracing::warn!(payload = %frame.payload, "tree.root preamble refused; staying connected");
    } else if frame.id == tree_id {
        let res: TreeRootRes = serde_json::from_value(frame.payload).context("tree.root res")?;
        root_node_id = res.node.id.clone();
        emit(IncomingEvt::TreeRoot {
            // Connect-time fetch is always the default workspace (see the
            // tree.root request above). If the chrome resumed a non-default
            // active workspace it re-fires tree.root with the id set, and
            // this default reply is dropped by the workspace check rather
            // than briefly flashing the wrong project's tree.
            workspace_id: None,
            root: res.node,
            children: res.children,
        });
        window.request_redraw();
    }
    Ok(root_node_id)
}

/// The connect preamble's preview.get of the root node.
pub(super) async fn preamble_preview<R, W, Wn>(
    mut tx: W,
    mut rx: R,
    prev_id: u64,
    root_node_id: String,
    host: &HostKey,
    session: &mut SessionState,
    emit: &impl Fn(IncomingEvt),
    window: &Wn,
) -> Result<()>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    Wn: Redraw,
{
    codec::write_frame(
        &mut tx,
        &Frame::req(
            prev_id,
            op::PREVIEW_GET,
            serde_json::to_value(PreviewGetReq {
                node_id: root_node_id,
                workspace_id: None,
                page: None,
                fit_w: None,
                fit_h: None,
            })?,
        ),
        None,
    )
    .await?;
    let (frame, blob) = codec::read_frame(&mut rx).await?;
    note_revision(frame.rev, &mut session.memory, host, &mut session.gate);
    if frame.id == prev_id && frame.payload.get("error").is_some() {
        // Same rule as the tree.root preamble: with no readable root there
        // is nothing to preview, and the session stays up.
        tracing::warn!(payload = %frame.payload, "preview.get preamble refused; staying connected");
    } else if frame.id == prev_id {
        let res: PreviewGetRes =
            serde_json::from_value(frame.payload).context("preview.get res")?;
        let bytes = blob.unwrap_or_default();
        emit(IncomingEvt::Preview {
            node_id: None,
            workspace_id: None,
            mime: res.mime,
            bytes,
            extras: res.extras,
            // No `PendingKind` to source a generation from — `0` is a
            // sentinel below the chrome's counter (which starts at `0` and
            // only increments on the first cursor-driven `preview.get`), so
            // this preamble reply naturally loses to any real request the
            // chrome has since fired, exactly like the workspace/host guard
            // above already intends for a resumed non-default workspace.
            generation: 0,
        });
        window.request_redraw();
    }
    Ok(())
}
