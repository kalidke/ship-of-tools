//! tree.children, tree.root, nav.toggle_hidden, directory.list: the requests (send_<op>: write the frame, then record its PendingKind).
//! Their replies (on_<op>: the reply frame becomes an IncomingEvt).

use super::*;

fn tree_children_frame(id: u64, parent_id: String, workspace_id: Option<String>) -> Result<Frame> {
    Ok(Frame::req(
        id,
        op::TREE_CHILDREN,
        serde_json::to_value(TreeChildrenReq {
            node_id: parent_id,
            workspace_id,
        })?,
    ))
}

fn tree_root_frame(id: u64, mode: String, workspace_id: Option<String>) -> Result<Frame> {
    Ok(Frame::req(
        id,
        op::TREE_ROOT,
        serde_json::to_value(TreeRootReq { mode, workspace_id })?,
    ))
}

pub(crate) async fn send_result_tree<W: AsyncWrite + Unpin>(
    tx: &mut W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    attempt: ResultAttemptId,
    request: ResultTreeRequest,
) -> Result<()> {
    pending.insert(
        id,
        PendingKind::ResultTree {
            attempt: attempt.clone(),
            request: request.clone(),
        },
    );
    let workspace_id = Some(attempt.workspace_id);
    let frame = match request {
        ResultTreeRequest::Root => tree_root_frame(id, "files".into(), workspace_id)?,
        ResultTreeRequest::Children { parent_id } => {
            tree_children_frame(id, parent_id, workspace_id)?
        }
    };
    codec::write_frame(tx, &frame, None).await?;
    Ok(())
}

pub(crate) fn on_result_tree(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    attempt: ResultAttemptId,
    request: ResultTreeRequest,
) {
    let parsed = if let Some(error) = frame.payload.get("error") {
        Err(error
            .as_str()
            .unwrap_or("result tree request failed")
            .to_string())
    } else {
        match &request {
            ResultTreeRequest::Root => serde_json::from_value::<TreeRootRes>(frame.payload)
                .map(|res| ResultTreeReply::Root {
                    root: res.node,
                    children: res.children,
                })
                .map_err(|error| format!("malformed result tree root: {error}")),
            ResultTreeRequest::Children { parent_id } => {
                serde_json::from_value::<TreeChildrenRes>(frame.payload)
                    .map(|res| ResultTreeReply::Children {
                        parent_id: parent_id.clone(),
                        children: res.children,
                    })
                    .map_err(|error| format!("malformed result tree children: {error}"))
            }
        }
    };
    let reply = parsed.unwrap_or_else(|error| ResultTreeReply::Failed { request, error });
    emit(IncomingEvt::ResultTree { attempt, reply });
}

/// One row of the `directory.list` response, mirrored here so the
/// chrome doesn't have to depend on `sot_protocol::DirectoryEntry`
/// directly.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub has_children: bool,
}

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
    // with the tree.root re-fetch `toggle_hidden_files` (ui/nav/files/listing.rs) fires right after,
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

pub(crate) fn on_tree_children(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    parent_id: String,
    workspace_id: Option<String>,
) {
    // Backend error frames ({error, code}) are legitimate
    // responses — surface them instead of tripping the struct
    // parse below ("missing field children") and dropping.
    if let Some(err) = frame.payload.get("error").and_then(|v| v.as_str()) {
        tracing::warn!(%parent_id, error = %err, "tree.children answered with error");
        emit(IncomingEvt::TreeChildrenFailed {
            workspace_id,
            parent_id,
            error: err.to_string(),
        });
        return;
    }
    match serde_json::from_value::<TreeChildrenRes>(frame.payload) {
        Ok(res) => {
            emit(IncomingEvt::TreeChildren {
                workspace_id,
                parent_id,
                children: res.children,
            });
        }
        Err(e) => {
            tracing::warn!(error = %e, %parent_id, "tree.children res parse failed");
            emit(IncomingEvt::TreeChildrenFailed {
                workspace_id,
                parent_id,
                error: e.to_string(),
            });
        }
    }
}

pub(crate) fn on_tree_root(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    workspace_id: Option<String>,
) {
    match serde_json::from_value::<TreeRootRes>(frame.payload) {
        Ok(res) => {
            emit(IncomingEvt::TreeRoot {
                workspace_id,
                root: res.node,
                children: res.children,
            });
        }
        Err(e) => {
            tracing::warn!(error = %e, "tree.root res parse failed");
        }
    }
}

pub(crate) fn on_directory_list(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
) {
    match serde_json::from_value::<sot_protocol::DirectoryListRes>(frame.payload) {
        Ok(res) => {
            let entries: Vec<DirEntry> = res
                .entries
                .into_iter()
                .map(|e| DirEntry {
                    name: e.name,
                    path: e.path,
                    has_children: e.has_children,
                })
                .collect();
            emit(IncomingEvt::DirectoryList {
                path: res.path,
                entries,
            });
        }
        Err(e) => {
            tracing::warn!(error = %e, "directory.list res parse failed");
        }
    }
}
