//! tree.children, tree.root, nav.toggle_hidden, directory.list: the requests (send_<op>: write the frame, then record its PendingKind).

use super::*;

pub(crate) async fn send_tree_children<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    parent_id: String,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(%parent_id, ?workspace_id, id, "→ tree.children");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::TREE_CHILDREN,
            serde_json::to_value(TreeChildrenReq {
                node_id: parent_id.clone(),
                workspace_id: workspace_id.clone(),
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(
        id,
        PendingKind::TreeChildren { parent_id, workspace_id },
    );
    Ok(())
}

pub(crate) async fn send_tree_root<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    mode: String,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(%mode, ?workspace_id, id, "→ tree.root");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::TREE_ROOT,
            serde_json::to_value(TreeRootReq {
                mode,
                workspace_id: workspace_id.clone(),
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::TreeRoot { workspace_id });
    Ok(())
}

pub(crate) async fn send_toggle_hidden<W: AsyncWrite + Unpin>(
    mut tx: W,
    id: u64,
    workspace_id: Option<String>,
) -> Result<()> {
    tracing::debug!(?workspace_id, id, "→ nav.toggle_hidden");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::NAV_TOGGLE_HIDDEN,
            serde_json::to_value(ToggleHiddenReq {
                workspace_id,
                mode: Some("files".to_string()),
            })?,
        ),
        None,
    )
    .await?;
    // No PendingKind: the response's new-state is redundant
    // with the tree.root re-fetch gpu.rs fires right after,
    // and an unmatched response id is silently ignored.
    Ok(())
}

pub(crate) async fn send_directory_list<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    path: String,
    include_hidden: bool,
) -> Result<()> {
    tracing::debug!(%path, include_hidden, id, "→ directory.list");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::DIRECTORY_LIST,
            serde_json::to_value(sot_protocol::DirectoryListReq {
                path,
                include_hidden,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::DirectoryList);
    Ok(())
}
