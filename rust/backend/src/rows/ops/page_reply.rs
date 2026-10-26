//! A confirmed page reply. Identity and text are checked again before the shared typing attempt.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sot_protocol::{op, page_reply, Frame, PageReplyReq, PageReplyRes};
use sot_protocol::reply::HandlerOutput;

use crate::comm::wake::{type_if_free, WakeOutcome};
use crate::rows::run::headless::HeadlessError;
use crate::rows::{handle::bound_handle, Workspaces};

fn answer(req_id: u64, outcome: &str, detail: impl Into<String>) -> HandlerOutput {
    let res = PageReplyRes { outcome: outcome.into(), detail: detail.into() };
    vec![(Frame::res(req_id, op::PAGE_REPLY, json!(res)), None)]
}

pub(crate) async fn handle_page_reply(req_id: u64, payload: Value, workspaces: &Workspaces) -> Result<HandlerOutput> {
    let req: PageReplyReq = serde_json::from_value(payload).context("page.reply payload")?;
    if let Err(reason) = page_reply::check_line(&req.line) {
        return Ok(answer(req_id, "refused", reason.to_string()));
    }
    // This is an id from the bar, never a default row or a slug fallback.
    let Some(ws) = workspaces.resolve(Some(&req.workspace_id)).filter(|ws| ws.workspace_id == req.workspace_id) else {
        return Ok(vec![(Frame::res(req_id, op::PAGE_REPLY, json!({
            "error": "unknown workspace", "code": "unknown_workspace"
        })), None)]);
    };
    if req.handle.is_empty() || bound_handle(&ws, &workspaces.list()) != req.handle {
        return Ok(answer(req_id, "refused", "handle_changed"));
    }
    if ws.runtime != "capsule" || ws.agent() != "claude" {
        return Ok(answer(req_id, "refused", "not_typeable"));
    }
    let Some(root) = sot_platform::host::state_dir::sot_state_dir() else {
        return Ok(answer(req_id, "not_ready", "could not resolve the state folder"));
    };
    let Some(home) = crate::comm::sot_comm_home() else {
        return Ok(answer(req_id, "not_free", "could not resolve the Stop hook registry"));
    };
    let rows = workspaces.clone();
    tokio::task::spawn_blocking(move || {
        let state_dir = crate::rows::spawn::state_root::state_dir_for(&root, &ws.workspace_id);
        if crate::rows::run::probe::phase_of(&state_dir) != "ready" {
            return answer(req_id, "not_ready", "the row is not Ready");
        }
        let admit = || {
            let current = rows.resolve(Some(&req.workspace_id))
                .filter(|row| row.workspace_id == req.workspace_id).ok_or("handle_changed")?;
            if bound_handle(&current, &rows.list()) != req.handle {
                return Err("handle_changed");
            }
            if current.runtime != "capsule" || current.agent() != "claude" {
                return Err("not_typeable");
            }
            if crate::rows::run::probe::phase_of(&state_dir) != "ready" {
                return Err("the row is not Ready");
            }
            Ok(())
        };
        let (outcome, detail) = delivery(type_if_free(&home, &state_dir, &req.handle, &req.line, &admit)
            .map(|attempt| attempt.outcome));
        tracing::info!(target: "page_reply", { handle = %req.handle, workspace = %req.workspace_id,
            line = ?req.line, outcome, detail = %detail }, "page reply delivery");
        answer(req_id, outcome, detail)
    }).await.context("page.reply typing task")
}

fn delivery(result: std::result::Result<WakeOutcome, HeadlessError>) -> (&'static str, String) {
    match result {
        Ok(WakeOutcome::Woke) => ("sent", "the marked line and Enter were recorded".into()),
        Ok(WakeOutcome::NotFree { reason, .. }) => ("not_free", reason.into()),
        Ok(WakeOutcome::TypedNoEnter { reason, .. }) => ("typed_not_sent", reason.into()),
        Ok(WakeOutcome::EnterRefused { detail }) => ("typed_not_sent", detail),
        Ok(WakeOutcome::Unconfirmed { step: "text", detail }) => ("not_free", detail),
        Ok(WakeOutcome::Unconfirmed { detail, .. }) | Ok(WakeOutcome::TextUnknown { detail }) => ("unknown", detail),
        Ok(WakeOutcome::RunChanged) => ("not_free", "the row's run changed".into()),
        Err(error) if error.submitted => ("unknown", format!("{}: {}", error.phase, error.detail)),
        Err(error) => ("not_free", format!("{}: {}", error.phase, error.detail)),
    }
}

#[cfg(test)]
#[path = "page_reply_tests.rs"]
mod tests;
