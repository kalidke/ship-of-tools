//! `selection.set`: the row's selection file holds exactly the rendering of the last selection (text or drawing) the daemon
//! accepted for that row, or nothing. An accepted selection publishes `<row state folder>/selection.json`; a clear, every refusal and
//! a failed publish remove it. The check and the rendering are `selection_check.rs`'s. A process-wide map, behind one lock,
//! keeps per row the connection that set the file; that connection's end (`forget`) and the start-up walk
//! (`clear_unowned`) remove the files nobody live set.

use std::io::Read;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result};
use sot_protocol::ops::{op, selection_code, Drawing, InkBase, SelectedText, SelectionSetReq, SelectionSetRes, SelectionState};
use sot_protocol::reply::HandlerOutput;
use sot_protocol::{selection_digest, Frame};

use super::selection_check::{check_selection, render_drawing, render_selection};
use crate::files::preview::ink::compose;
use crate::files::preview::PREVIEW_BYTE_CAP;
use crate::files::ID_PREFIX;
use crate::rows::spawn::state_root::state_dir_for;
use crate::rows::{Workspace, Workspaces};

/// The file's name in the row's state folder.
pub(crate) const FILE_NAME: &str = "selection.json";

/// The marker's name in the row's state folder: present when the row's current agent was started with the hook.
const MARKER_NAME: &str = "selection-hook";

/// Records, in the row's state folder, whether the leg just started hears a selection: the marker exists when the row's
/// agent is Claude, the leg carries `SOT_SELECTION_FILE` in `env`, and its `argv` carries the selection hook; it is
/// removed otherwise. Codex legs have none until the Codex wiring is built.
pub(crate) fn write_marker(state_dir: &Path, agent_kind: &str, env: &[(String, String)], argv: &[String]) -> std::io::Result<()> {
    let marker = state_dir.join(MARKER_NAME);
    let hears = agent_kind == "claude"
        && env.iter().any(|(k, _)| k == "SOT_SELECTION_FILE")
        && super::layer::carries_selection_hook(argv);
    if !hears {
        return sot_platform::host::remove_durable(&marker);
    }
    // the supervisor makes the folder too; this is only for a write that wins the race to it
    std::fs::create_dir_all(state_dir)?;
    sot_platform::host::write_durable(&marker, b"")
}

/// The agent kinds that take a selection; every other kind answers `no_agent`. A Codex row answers `no_agent` until the
/// plan's H2 decides whether Codex hears one at all.
const TAKES_SELECTION: [&str; 1] = ["claude"];

/// The agent kinds that take a drawing; a Codex row answers `no_agent` for one until the plan's H-D2 decides it reads
/// the picture.
const TAKES_DRAWING: [&str; 1] = ["claude"];

/// Held across a drawing's composite and its publish: one drawing at a time in the daemon, so the file names the
/// picture on disk and the digest it states. A text selection takes only the setter lock.
static COMPOSE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The rows whose file a live connection set: row id to the setting connection's roster serial and the file's path.
/// Every publish and removal of a selection file holds the lock, and no holder awaits.
static SETTERS: LazyLock<Mutex<HashMap<String, (u64, PathBuf)>>> = LazyLock::new(Mutex::default);

/// The map under its lock. A holder that panicked left the map whole (each change is one insert or remove), so a
/// poisoned lock is taken as it is rather than stopping the daemon.
fn setters() -> MutexGuard<'static, HashMap<String, (u64, PathBuf)>> {
    SETTERS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Removes `file`; a failure has nobody to answer, so it is logged and the next start-up walk removes it.
fn remove_logged(row: &str, file: &Path) {
    if let Err(e) = sot_platform::host::remove_durable(file) {
        tracing::warn!(row, error = %e, "a selection file was not removed; the next start-up walk removes it");
    }
}

/// A connection's end: removes every file that `serial` set, and none another connection set.
pub(crate) fn forget(serial: u64) {
    setters().retain(|row, (setter, file)| {
        if *setter == serial {
            remove_logged(row, file);
        }
        *setter != serial
    });
}

/// The start-up walk's call for one row: removes its file only when it exists and no live connection of this process
/// set it (a row with no file costs one `stat`); answers whether it removed one.
pub(crate) fn clear_unowned(state_root: &Path, row: &str) -> bool {
    let map = setters();
    let file = state_dir_for(state_root, row).join(FILE_NAME);
    let removable = !map.contains_key(row) && file.try_exists().unwrap_or(true);
    if removable {
        remove_logged(row, &file);
    }
    removable
}

/// `selection.set`: one reply. `window` is whether the connection is a window's, the server's one definition
/// (`clients::window_name`); `serial` is its roster serial. An accepted selection answers `shared` when the row's agent was started with the hook and `held` when it was not; a clear answers `shared`.
pub(crate) async fn handle_selection_set(
    req_id: u64, payload: serde_json::Value, state_root: Option<&Path>, workspaces: &Workspaces, window: bool, serial: u64,
) -> Result<HandlerOutput> {
    let req: SelectionSetReq = serde_json::from_value(payload).context("selection.set payload")?;
    let result = match &req.drawing {
        Some(drawing) => set_drawing(state_root, workspaces, window, serial, &req.workspace_id, drawing).await,
        None => set_selection(state_root, workspaces, window, serial, &req),
    };
    let frame = match result {
        Ok(state) => Frame::res(req_id, op::SELECTION_SET, serde_json::to_value(SelectionSetRes { state })?),
        Err(code) => {
            let payload = serde_json::json!({ "error": format!("selection not shared: {code}"), "code": code });
            Frame::res(req_id, op::SELECTION_SET, payload)
        }
    };
    Ok(vec![(frame, None)])
}

/// The checks every request passes first, in order: a window's connection, the state root, the row. Answers the row
/// and its state folder.
fn row_of(
    state_root: Option<&Path>, workspaces: &Workspaces, window: bool, workspace: &str,
) -> Result<(Arc<Workspace>, PathBuf), &'static str> {
    if !window {
        return Err(selection_code::NOT_A_WINDOW);
    }
    let state_root = state_root.ok_or(selection_code::PUBLISH_FAILED)?;
    let ws = workspaces.resolve(Some(workspace)).ok_or(selection_code::UNKNOWN_WORKSPACE)?;
    let folder = state_dir_for(state_root, &ws.workspace_id);
    Ok((ws, folder))
}

/// Applies a text selection or a clear (`req` holds no drawing) to its row's file under `state_root`; the refusal is a
/// `selection_code`. The row is resolved and the project file read and rendered before the lock; the lock covers the
/// file's publish or removal and the map.
fn set_selection(
    state_root: Option<&Path>, workspaces: &Workspaces, window: bool, serial: u64, req: &SelectionSetReq,
) -> Result<SelectionState, &'static str> {
    let (ws, folder) = row_of(state_root, workspaces, window, &req.workspace_id)?;
    let checked = req.selection.as_ref().map(|sel| {
        if takes_selection(&ws.agent()) { render(&ws, sel) } else { Err(selection_code::NO_AGENT) }
    });
    publish(&ws.workspace_id, &folder, serial, checked)
}

/// Applies a drawing to its row's file. After the row's checks and its agent's, the composite and the publish run
/// under `COMPOSE`, so after each publish the file names the picture on disk and the digest it holds. The composite runs on the blocking pool, off
/// the connection's loop; the setter lock is taken only inside `publish`, after the await.
async fn set_drawing(
    state_root: Option<&Path>, workspaces: &Workspaces, window: bool, serial: u64, workspace: &str, drawing: &Drawing,
) -> Result<SelectionState, &'static str> {
    let (ws, folder) = row_of(state_root, workspaces, window, workspace)?;
    let _one_at_a_time = COMPOSE.lock().await;
    let checked = if takes_drawing(&ws.agent()) { render_picture(&ws, drawing).await } else { Err(selection_code::NO_AGENT) };
    sot_platform::hold_point_async!("agents.selection.publish", &ws.workspace_id);
    publish(&ws.workspace_id, &folder, serial, Some(checked))
}

/// The file after `checked` (`None` for a clear): applied under the setter lock, which then records the setter for a
/// request that sets something (`checked` is `Some`) and drops it otherwise.
fn publish(
    row: &str, folder: &Path, serial: u64, checked: Option<Result<String, &'static str>>,
) -> Result<SelectionState, &'static str> {
    let sets = checked.is_some();
    let file = folder.join(FILE_NAME);
    let hooked = folder.join(MARKER_NAME).exists();
    let mut map = setters();
    let result = apply(&file, checked, hooked);
    match (&result, sets) {
        (Ok(_), true) => map.insert(row.to_string(), (serial, file)),
        // a removal that failed leaves the file and its setter's entry, so that setter's end or a later clear retries it
        (Err(_), _) if file.exists() => None,
        _ => map.remove(row),
    };
    result
}

/// The file after a clear (`checked` is `None`) or the checked rendering, with the map's lock held by the caller.
fn apply(file: &Path, checked: Option<Result<String, &'static str>>, hooked: bool) -> Result<SelectionState, &'static str> {
    let withdraw = || sot_platform::host::remove_durable(file).map_err(|_| selection_code::PUBLISH_FAILED);
    let Some(checked) = checked else {
        withdraw()?;
        return Ok(SelectionState::Shared);
    };
    let rendering = match checked {
        Ok(r) => r,
        Err(code) => {
            withdraw()?;
            return Err(code);
        }
    };
    if sot_platform::host::write_durable(file, rendering.as_bytes()).is_err() {
        let _ = withdraw();
        return Err(selection_code::PUBLISH_FAILED);
    }
    Ok(if hooked { SelectionState::Shared } else { SelectionState::Held })
}

/// Whether an agent of `kind` takes a drawing.
fn takes_drawing(kind: &str) -> bool {
    TAKES_DRAWING.contains(&kind)
}

/// Whether an agent of `kind` takes a selection.
fn takes_selection(kind: &str) -> bool {
    TAKES_SELECTION.contains(&kind)
}

/// The hook's output for `sel`, from the file's own text: resolved as `preview.get` resolves (a link out of the root is
/// followed, never further than the preview reads) and read to at most the preview's cap.
fn render(ws: &Workspace, sel: &SelectedText) -> Result<String, &'static str> {
    let files = ws.files_mode().map_err(|_| selection_code::BAD_NODE_ID)?;
    let path = files.node_id_to_path(&sel.node_id).map_err(|_| selection_code::BAD_NODE_ID)?;
    let bytes = read_prefix(&path).ok_or(selection_code::NOT_FILE_TEXT)?;
    let text = check_selection(&bytes, sel).ok_or(selection_code::NOT_FILE_TEXT)?;
    let absolute = path.to_string_lossy().into_owned();
    let id = files.path_to_node_id(&path).ok_or(selection_code::BAD_NODE_ID)?;
    let relative = id.strip_prefix(ID_PREFIX).ok_or(selection_code::BAD_NODE_ID)?;
    Ok(render_selection(&text, &absolute, relative))
}

/// The hook's output for `drawing`: its picture composed on the blocking pool, then named with the figure's paths and
/// the digest of the drawing as the wire serializes it.
async fn render_picture(ws: &Workspace, drawing: &Drawing) -> Result<String, &'static str> {
    let files = ws.files_mode().map_err(|_| selection_code::BAD_NODE_ID)?;
    let (row, owned) = (ws.workspace_id.clone(), drawing.clone());
    let composed = {
        let files = files.clone();
        tokio::task::spawn_blocking(move || compose(&files, &row, &owned)).await
    };
    let picture = match composed {
        Ok(Ok(picture)) => picture,
        Ok(Err((code, message))) => {
            tracing::debug!(row = %ws.workspace_id, code, %message, "a drawing was not composed");
            return Err(code);
        }
        Err(_) => return Err(selection_code::PUBLISH_FAILED),
    };
    let figure = match &drawing.base {
        InkBase::Canvas => None,
        InkBase::Figure { node_id } => {
            let path = files.node_id_to_path(node_id).map_err(|_| selection_code::BAD_NODE_ID)?;
            let id = files.path_to_node_id(&path).ok_or(selection_code::BAD_NODE_ID)?;
            let relative = id.strip_prefix(ID_PREFIX).ok_or(selection_code::BAD_NODE_ID)?.to_string();
            Some((path.to_string_lossy().into_owned(), relative))
        }
    };
    let digest = selection_digest(&serde_json::to_string(drawing).map_err(|_| selection_code::PUBLISH_FAILED)?);
    let figure = figure.as_ref().map(|(abs, rel)| (abs.as_str(), rel.as_str()));
    Ok(render_drawing(figure, &picture.to_string_lossy(), drawing.strokes.len(), digest))
}

/// The first `PREVIEW_BYTE_CAP` bytes of a regular file (never a pipe or device, whose open may wait), or `None`.
fn read_prefix(path: &Path) -> Option<Vec<u8>> {
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(PREVIEW_BYTE_CAP as u64).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
