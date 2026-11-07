//! `selection.set`: the row's selection file holds exactly the rendering of the last selection the daemon accepted for
//! that row, or nothing. An accepted selection publishes `<row state folder>/selection.json`; a clear, every refusal and
//! a failed publish remove it. The check and the rendering are `selection_check.rs`'s.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use sot_protocol::ops::{op, selection_code, SelectedText, SelectionSetReq, SelectionSetRes, SelectionState};
use sot_protocol::reply::HandlerOutput;
use sot_protocol::Frame;

use super::selection_check::{check_selection, render_selection};
use crate::files::preview::PREVIEW_BYTE_CAP;
use crate::files::ID_PREFIX;
use crate::rows::spawn::state_root::state_dir_for;
use crate::rows::{Workspace, Workspaces};

/// The file's name in the row's state folder.
const FILE_NAME: &str = "selection.json";

/// The marker's name in the row's state folder: present when the row's current agent was started with the hook.
const MARKER_NAME: &str = "selection-hook";

/// The agent kinds that take a selection; every other kind answers `no_agent`. `codex` is here until the plan's H2
/// decides whether Codex hears one at all.
const TAKES_SELECTION: [&str; 2] = ["claude", "codex"];

/// `selection.set`: one reply. `window` is whether the connection holds a roster entry. An accepted selection answers
/// `shared` when the row's agent was started with the hook and `held` when it was not; a clear answers `shared`.
pub(crate) async fn handle_selection_set(
    req_id: u64, payload: serde_json::Value, state_root: Option<&Path>, workspaces: &Workspaces, window: bool,
) -> Result<HandlerOutput> {
    let req: SelectionSetReq = serde_json::from_value(payload).context("selection.set payload")?;
    let frame = match state_root.map_or(Err(selection_code::PUBLISH_FAILED), |root| set_selection(root, workspaces, window, &req)) {
        Ok(state) => Frame::res(req_id, op::SELECTION_SET, serde_json::to_value(SelectionSetRes { state })?),
        Err(code) => {
            let payload = serde_json::json!({ "error": format!("selection not shared: {code}"), "code": code });
            Frame::res(req_id, op::SELECTION_SET, payload)
        }
    };
    Ok(vec![(frame, None)])
}

/// Applies `req` to its row's file under `state_root`; the refusal is a `selection_code`.
fn set_selection(
    state_root: &Path, workspaces: &Workspaces, window: bool, req: &SelectionSetReq,
) -> Result<SelectionState, &'static str> {
    if !window {
        return Err(selection_code::NOT_A_WINDOW);
    }
    let ws = workspaces.resolve(Some(&req.workspace_id)).ok_or(selection_code::UNKNOWN_WORKSPACE)?;
    let file = state_dir_for(state_root, &ws.workspace_id).join(FILE_NAME);
    let withdraw = || sot_platform::host::remove_durable(&file).map_err(|_| selection_code::PUBLISH_FAILED);
    let Some(sel) = &req.selection else {
        withdraw()?;
        return Ok(SelectionState::Shared);
    };
    let checked = if takes_selection(&ws.agent()) { render(&ws, sel) } else { Err(selection_code::NO_AGENT) };
    let rendering = match checked {
        Ok(r) => r,
        Err(code) => {
            withdraw()?;
            return Err(code);
        }
    };
    if sot_platform::host::write_durable(&file, rendering.as_bytes()).is_err() {
        let _ = withdraw();
        return Err(selection_code::PUBLISH_FAILED);
    }
    let hooked = state_dir_for(state_root, &ws.workspace_id).join(MARKER_NAME).exists();
    Ok(if hooked { SelectionState::Shared } else { SelectionState::Held })
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
