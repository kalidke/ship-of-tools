//! Editor file ops: `file.read`, `file.write`, `file.delete`, `dir.create`, each resolving its
//! node with `resolve_file_node`.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::DirCreateReq;
use sot_protocol::DirCreateRes;
use sot_protocol::FileDeleteReq;
use sot_protocol::FileDeleteRes;
use sot_protocol::FileReadReq;
use sot_protocol::FileReadRes;
use sot_protocol::FileWriteReq;
use sot_protocol::FileWriteRes;
use sot_protocol::Frame;
use crate::files::io;
use crate::files::io::WriteResult;
use crate::session::Session;
use crate::rows::Workspaces;
use crate::server::reply::HandlerOutput;

/// Read a source file's full text for the in-frontend editor. Unlike
/// `preview.get` (kernel-rendered), this is raw backend byte IO — no kernel
/// dependency — returning the text plus a content `version` for the matching
/// conflict-aware `file.write`.
pub async fn handle_file_read(
    req_id: u64,
    payload_json: serde_json::Value,
    _session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: FileReadReq = serde_json::from_value(payload_json).context("file.read payload")?;
    tracing::info!(
        node_id = %req.node_id,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "file.read"
    );

    let path = match resolve_file_node(
        op::FILE_READ,
        req_id,
        &req.node_id,
        req.workspace_id.as_deref(),
        workspaces,
        false,
    ) {
        Ok(p) => p,
        Err(out) => return Ok(out),
    };

    match io::read_file(&path) {
        Ok(Some(r)) => {
            let res = FileReadRes {
                node_id: req.node_id,
                exists: true,
                content: r.content,
                version: r.version,
            };
            Ok(vec![(
                Frame::res(req_id, op::FILE_READ, serde_json::to_value(res)?),
                None,
            )])
        }
        Ok(None) => {
            let res = FileReadRes {
                node_id: req.node_id,
                exists: false,
                content: String::new(),
                version: String::new(),
            };
            Ok(vec![(
                Frame::res(req_id, op::FILE_READ, serde_json::to_value(res)?),
                None,
            )])
        }
        Err(e) => Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_READ,
                json!({ "error": format!("{e:#}"), "code": "file_read_failed", "node_id": req.node_id }),
            ),
            None,
        )]),
    }
}

/// Write a source file from the in-frontend editor with optimistic concurrency.
/// When `expected_version` is set and the on-disk content has changed since the
/// matching `file.read`, the write is refused with `code: "conflict"` and the
/// response carries the current on-disk content/version for reconciliation.
pub async fn handle_file_write(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: FileWriteReq = serde_json::from_value(payload_json).context("file.write payload")?;
    tracing::info!(
        node_id = %req.node_id,
        len = req.content.len(),
        expected_set = req.expected_version.is_some(),
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "file.write"
    );

    let path = match resolve_file_node(
        op::FILE_WRITE,
        req_id,
        &req.node_id,
        req.workspace_id.as_deref(),
        workspaces,
        true,
    ) {
        Ok(p) => p,
        Err(out) => return Ok(out),
    };

    match io::write_file(&path, &req.content, req.expected_version.as_deref()) {
        Ok(WriteResult::Written { version }) => {
            let res = FileWriteRes {
                node_id: req.node_id.clone(),
                path: path.to_string_lossy().to_string(),
                version,
                written: req.content.len() as u64,
            };
            // A source write mutates the project; bump the revision so a
            // reconnecting client (and the file-watcher consumers) know.
            let rev = session
                .bump("file.written", json!({ "node_id": req.node_id }))
                .await;
            Ok(vec![(
                Frame::res(req_id, op::FILE_WRITE, serde_json::to_value(res)?).with_rev(rev),
                None,
            )])
        }
        Ok(WriteResult::Conflict {
            current_content,
            current_version,
        }) => Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_WRITE,
                json!({
                    "error": "conflict: on-disk content changed since read",
                    "code": "conflict",
                    "node_id": req.node_id,
                    "current_content": current_content,
                    "current_version": current_version,
                }),
            ),
            None,
        )]),
        Err(e) => Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_WRITE,
                json!({ "error": format!("{e:#}"), "code": "file_write_failed", "node_id": req.node_id }),
            ),
            None,
        )]),
    }
}

/// Trash a file from Files-mode nav (FE Ctrl+D). v1 contract: directories are
/// refused (`code: "is_directory"`) and nothing is ever hard-unlinked —
/// `io::trash_file` goes to the system trash (`gio trash`) or falls back
/// to `<workspace_root>/.sot-trash/` (the response's `trash_path` says
/// which). Bumps the session revision like file.write so the watcher and
/// reconnecting clients refresh.
pub async fn handle_file_delete(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: FileDeleteReq = serde_json::from_value(payload_json).context("file.delete payload")?;
    tracing::info!(
        node_id = %req.node_id,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "file.delete"
    );

    let path = match resolve_file_node(
        op::FILE_DELETE,
        req_id,
        &req.node_id,
        req.workspace_id.as_deref(),
        workspaces,
        true,
    ) {
        Ok(p) => p,
        Err(out) => return Ok(out),
    };

    // symlink_metadata: a symlink *to* a directory is still trashable as a
    // file (we move the link, never its target); only real directories are
    // refused in v1.
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) => {
            return Ok(vec![(
                Frame::res(
                    req_id,
                    op::FILE_DELETE,
                    json!({ "error": format!("{e:#}"), "code": "not_found", "node_id": req.node_id }),
                ),
                None,
            )]);
        }
    };
    if meta.is_dir() {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_DELETE,
                json!({ "error": "directories are not deletable in v1", "code": "is_directory", "node_id": req.node_id }),
            ),
            None,
        )]);
    }

    // resolve_file_node already validated the workspace; re-resolve for the
    // project root the fallback trash dir lives under.
    let Some(ws) = workspaces.resolve(req.workspace_id.as_deref()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_DELETE,
                json!({ "error": format!("unknown workspace: {:?}", req.workspace_id), "code": "unknown_workspace" }),
            ),
            None,
        )]);
    };

    match io::trash_file(&path, &ws.project_root) {
        Ok(trash_path) => {
            let res = FileDeleteRes {
                node_id: req.node_id.clone(),
                path: path.to_string_lossy().to_string(),
                trashed: true,
                trash_path: trash_path.map(|p| p.to_string_lossy().to_string()),
            };
            let rev = session
                .bump("file.deleted", json!({ "node_id": req.node_id }))
                .await;
            Ok(vec![(
                Frame::res(req_id, op::FILE_DELETE, serde_json::to_value(res)?).with_rev(rev),
                None,
            )])
        }
        Err(e) => Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_DELETE,
                json!({ "error": format!("{e:#}"), "code": "file_delete_failed", "node_id": req.node_id }),
            ),
            None,
        )]),
    }
}

/// Create a directory from Files-mode nav (FE Ctrl+N, a name ending in `/`).
/// Non-recursive (`std::fs::create_dir`, not `create_dir_all`): a missing
/// parent fails loudly instead of being silently created, mirroring
/// `file.write`'s new-file contract. An existing file or directory at the
/// target path is refused with `code: "already_exists"` rather than
/// silently succeeding. Bumps the session revision like file.write/delete so
/// the watcher and reconnecting clients refresh.
pub async fn handle_dir_create(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: DirCreateReq = serde_json::from_value(payload_json).context("dir.create payload")?;
    tracing::info!(
        node_id = %req.node_id,
        workspace_id = req.workspace_id.as_deref().unwrap_or("<default>"),
        "dir.create"
    );

    let path = match resolve_file_node(
        op::DIR_CREATE,
        req_id,
        &req.node_id,
        req.workspace_id.as_deref(),
        workspaces,
        true,
    ) {
        Ok(p) => p,
        Err(out) => return Ok(out),
    };

    match std::fs::create_dir(&path) {
        Ok(()) => {
            let res = DirCreateRes {
                node_id: req.node_id.clone(),
                path: path.to_string_lossy().to_string(),
            };
            let rev = session
                .bump("dir.created", json!({ "node_id": req.node_id }))
                .await;
            Ok(vec![(
                Frame::res(req_id, op::DIR_CREATE, serde_json::to_value(res)?).with_rev(rev),
                None,
            )])
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(vec![(
            Frame::res(
                req_id,
                op::DIR_CREATE,
                json!({ "error": "already exists", "code": "already_exists", "node_id": req.node_id }),
            ),
            None,
        )]),
        Err(e) => Ok(vec![(
            Frame::res(
                req_id,
                op::DIR_CREATE,
                json!({ "error": format!("{e:#}"), "code": "dir_create_failed", "node_id": req.node_id }),
            ),
            None,
        )]),
    }
}

/// Shared workspace → FilesMode → safe path resolution for the file.read /
/// file.write / file.delete / dir.create handlers. On any failure returns the
/// error `HandlerOutput` to send back (tagged with `op`); on success returns
/// the resolved absolute path. Both resolvers reject `..`/absolute ids;
/// `confined` selects the WRITE resolver (`node_id_to_path_confined`, the
/// symlink escape guard — mutations can't leave the project root) vs the
/// READ resolver (follows user symlinks, e.g. NAS mounts — see
/// files/tree.rs).
fn resolve_file_node(
    op_name: &'static str,
    req_id: u64,
    node_id: &str,
    workspace_id: Option<&str>,
    workspaces: &Workspaces,
    confined: bool,
) -> std::result::Result<std::path::PathBuf, HandlerOutput> {
    let Some(ws) = workspaces.resolve(workspace_id) else {
        return Err(vec![(
            Frame::res(
                req_id,
                op_name,
                json!({ "error": format!("unknown workspace: {workspace_id:?}"), "code": "unknown_workspace" }),
            ),
            None,
        )]);
    };
    let files_mode = match ws.files_mode() {
        Ok(fm) => fm,
        Err(e) => {
            return Err(vec![(
                Frame::res(
                    req_id,
                    op_name,
                    json!({ "error": format!("files_mode init failed: {e:#}"), "code": "files_mode_init_failed" }),
                ),
                None,
            )]);
        }
    };
    let resolved = if confined {
        files_mode.node_id_to_path_confined(node_id)
    } else {
        files_mode.node_id_to_path(node_id)
    };
    match resolved {
        Ok(p) => Ok(p),
        Err(e) => Err(vec![(
            Frame::res(
                req_id,
                op_name,
                json!({ "error": format!("{e:#}"), "code": "bad_node_id" }),
            ),
            None,
        )]),
    }
}
