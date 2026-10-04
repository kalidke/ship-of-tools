//! The Files tree ops: `tree.root`, `tree.children`, `nav.toggle_hidden`, and `directory.list`
//! (any directory on this host, for the new-session picker).

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::Frame;
use sot_protocol::ToggleHiddenReq;
use sot_protocol::ToggleHiddenRes;
use sot_protocol::TreeChildrenReq;
use sot_protocol::TreeChildrenRes;
use sot_protocol::TreeRootReq;
use sot_protocol::TreeRootRes;
use crate::session::Session;
use crate::workspaces::Workspaces;
use crate::handlers::HandlerOutput;

pub async fn handle_tree_root(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: TreeRootReq = serde_json::from_value(payload_json).context("tree.root payload")?;
    tracing::info!(
        mode = %req.mode,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "tree.root"
    );

    // Only files-mode for now; the other six modes live behind their own
    // verbs once kernel-side Mode dispatch is wired (post-phase-1).
    if req.mode != "files" {
        let payload = json!({
            "error": format!("unknown mode: {}", req.mode),
            "code": "unknown_mode",
        });
        return Ok(vec![(Frame::res(req_id, op::TREE_ROOT, payload), None)]);
    }

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::TREE_ROOT,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let files_mode = match ws.files_mode() {
        Ok(fm) => fm,
        Err(e) => {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::TREE_ROOT,
                    json!({
                        "error": format!("files_mode init failed: {e:#}"),
                        "code": "files_mode_init_failed",
                    }),
                ),
                None,
            )]);
        }
    };
    let root = files_mode.root_node();
    let children = files_mode
        .children_of(&root.id)
        .context("listing project root")?;
    let res = TreeRootRes {
        node: root,
        children,
    };

    let rev = session
        .bump("tree.invalidate", json!({ "scope": req.mode }))
        .await;

    Ok(vec![(
        Frame::res(req_id, op::TREE_ROOT, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

pub async fn handle_tree_children(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: TreeChildrenReq =
        serde_json::from_value(payload_json).context("tree.children payload")?;
    tracing::info!(
        node_id = %req.node_id,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "tree.children"
    );

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::TREE_CHILDREN,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let files_mode = ws.files_mode().context("files_mode init")?;
    let children = match files_mode.children_of(&req.node_id) {
        Ok(c) => c,
        Err(e) => {
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "tree_children_failed",
                "node_id": req.node_id,
            });
            return Ok(vec![(Frame::res(req_id, op::TREE_CHILDREN, payload), None)]);
        }
    };

    let res = TreeChildrenRes { children };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::TREE_CHILDREN, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// Flip the workspace's Files-mode "show hidden files" flag and invalidate the
/// files tree. Mirrors `handle_tree_root`'s workspace resolution + the same
/// `tree.invalidate` bump so a reconnecting client re-fetches; the live
/// frontend re-fetches `tree.root` right after this op. The flag lives on the
/// cached `Arc<FilesMode>` (interior mutability), so subsequent
/// `tree.children` / `tree.root` walks pick up the new visibility.
pub async fn handle_nav_toggle_hidden(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: ToggleHiddenReq =
        serde_json::from_value(payload_json).context("nav.toggle_hidden payload")?;
    tracing::info!(
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        mode = req.mode.as_deref().unwrap_or("files"),
        "nav.toggle_hidden"
    );

    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::NAV_TOGGLE_HIDDEN,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let files_mode = match ws.files_mode() {
        Ok(fm) => fm,
        Err(e) => {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::NAV_TOGGLE_HIDDEN,
                    json!({
                        "error": format!("files_mode init failed: {e:#}"),
                        "code": "files_mode_init_failed",
                    }),
                ),
                None,
            )]);
        }
    };
    let show_hidden = files_mode.toggle_hidden();

    let rev = session
        .bump("tree.invalidate", json!({ "scope": "files" }))
        .await;

    let res = ToggleHiddenRes { show_hidden };
    Ok(vec![(
        Frame::res(req_id, op::NAV_TOGGLE_HIDDEN, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

pub async fn handle_directory_list(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
) -> Result<HandlerOutput> {
    use sot_protocol::{DirectoryEntry, DirectoryListReq, DirectoryListRes};
    let req: DirectoryListReq =
        serde_json::from_value(payload_json).context("directory.list payload")?;
    tracing::debug!(path = %req.path, include_hidden = req.include_hidden, "directory.list");

    let path = std::path::PathBuf::from(&req.path);
    let include_hidden = req.include_hidden;
    let result = tokio::task::spawn_blocking(move || -> Result<Vec<DirectoryEntry>> {
        let read = std::fs::read_dir(&path).with_context(|| format!("read_dir {path:?}"))?;
        let mut entries: Vec<DirectoryEntry> = Vec::new();
        for ent in read.flatten() {
            let name = match ent.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue, // non-UTF8 filename — skip
            };
            if !include_hidden && name.starts_with('.') {
                continue;
            }
            let p = ent.path();
            // Follow symlinks via metadata (not symlink_metadata) so a
            // symlink to a directory still surfaces as one entry.
            let is_dir = match std::fs::metadata(&p) {
                Ok(m) => m.is_dir(),
                Err(_) => continue,
            };
            if !is_dir {
                continue;
            }
            // Cheap has_children probe: try to open the dir and see if
            // any subdirectory exists. Don't recurse — just one read_dir
            // pass per entry.
            let has_children = std::fs::read_dir(&p)
                .ok()
                .map(|it| {
                    it.flatten().any(|c| {
                        let cn = c.file_name();
                        if !include_hidden {
                            if let Some(s) = cn.to_str() {
                                if s.starts_with('.') {
                                    return false;
                                }
                            }
                        }
                        std::fs::metadata(c.path())
                            .map(|m| m.is_dir())
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            entries.push(DirectoryEntry {
                name,
                path: p.to_string_lossy().into_owned(),
                has_children,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    })
    .await
    .context("spawn_blocking directory.list")?;

    let (_, rev) = session.snapshot().await;
    match result {
        Ok(entries) => {
            let res = DirectoryListRes {
                path: req.path,
                entries,
            };
            Ok(vec![(
                Frame::res(req_id, op::DIRECTORY_LIST, serde_json::to_value(res)?).with_rev(rev),
                None,
            )])
        }
        Err(e) => {
            let payload = json!({
                "error": format!("{e:#}"),
                "code": "directory_list_failed",
                "path": req.path,
            });
            Ok(vec![(
                Frame::res(req_id, op::DIRECTORY_LIST, payload),
                None,
            )])
        }
    }
}
