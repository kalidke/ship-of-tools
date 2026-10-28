//! A page reply: confirmed on the window's bar, or sent at once from a page with a direct grant. Identity and text are
//! checked again before the shared typing attempt. A direct reply to a busy row is held in memory, one per row, and
//! the wake's tick offers it the row's prompt again (`release`).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

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
    // This is an id from the bar or a page grant, never a default row or a slug fallback.
    if workspaces.resolve(Some(&req.workspace_id)).filter(|ws| ws.workspace_id == req.workspace_id).is_none() {
        return Ok(vec![(Frame::res(req_id, op::PAGE_REPLY, json!({
            "error": "unknown workspace", "code": "unknown_workspace"
        })), None)]);
    }
    if req.hold {
        let receipt = submit(workspaces.clone(), String::new(), req.workspace_id, req.handle, req.line).await?;
        return Ok(answer(req_id, receipt.outcome, receipt.detail));
    }
    let rows = workspaces.clone();
    tokio::task::spawn_blocking(move || {
        let (outcome, detail) = attempt(&rows, &req.workspace_id, &req.handle, &req.line);
        answer(req_id, outcome, detail)
    }).await.context("page.reply typing task")
}

/// One typing attempt through the wake's gate, the row's identity checked again under it, logged whatever its outcome.
/// Blocks.
fn attempt(rows: &Workspaces, workspace_id: &str, handle: &str, line: &str) -> (&'static str, String) {
    let (outcome, detail) = attempt_unlogged(rows, workspace_id, handle, line);
    tracing::info!(target: "page_reply", { handle = %handle, workspace = %workspace_id,
        line = ?line, outcome, detail = %detail }, "page reply delivery");
    (outcome, detail)
}

fn attempt_unlogged(rows: &Workspaces, workspace_id: &str, handle: &str, line: &str) -> (&'static str, String) {
    let Some(ws) = rows.resolve(Some(workspace_id)).filter(|ws| ws.workspace_id == workspace_id) else {
        return ("refused", "row_gone".into());
    };
    if handle.is_empty() || bound_handle(&ws, &rows.list()) != handle {
        return ("refused", "handle_changed".into());
    }
    if ws.runtime != "capsule" || ws.agent() != "claude" {
        return ("refused", "not_typeable".into());
    }
    let Some(root) = sot_platform::host::state_dir::sot_state_dir() else {
        return ("not_ready", "could not resolve the state folder".into());
    };
    let Some(home) = crate::comm::sot_comm_home() else {
        return ("not_free", "could not resolve the Stop hook registry".into());
    };
    let state_dir = crate::rows::spawn::state_root::state_dir_for(&root, workspace_id);
    if crate::rows::run::probe::phase_of(&state_dir) != "ready" {
        return ("not_ready", "the row is not Ready".into());
    }
    let admit = || {
        let current = rows.resolve(Some(workspace_id))
            .filter(|row| row.workspace_id == workspace_id).ok_or("handle_changed")?;
        if bound_handle(&current, &rows.list()) != handle {
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
    delivery(type_if_free(&home, &state_dir, handle, line, &admit).map(|attempt| attempt.outcome))
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

/// A direct reply a busy row keeps until its prompt frees, for the question its row asked when the page answered.
#[derive(Clone)]
struct Held {
    request_id: String,
    workspace_id: String,
    handle: String,
    line: String,
    question: String,
}

/// What a direct reply came to, by its request id: `sent`, `held`, `refused` or `unknown`, and why.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct Receipt {
    pub request_id: String,
    pub workspace_id: String,
    pub handle: String,
    pub outcome: &'static str,
    pub detail: String,
}

/// Memory only: a daemon restart drops a held reply, and its page then finds no receipt for it and shows unknown.
static HELD: Mutex<BTreeMap<String, Held>> = Mutex::new(BTreeMap::new());
static RECEIPTS: Mutex<VecDeque<Receipt>> = Mutex::new(VecDeque::new());
const RECEIPTS_KEPT: usize = 64;

/// The receipts the pages read, newest last.
pub(crate) fn receipts() -> Vec<Receipt> {
    RECEIPTS.lock().unwrap_or_else(|p| p.into_inner()).iter().cloned().collect()
}

/// A direct reply: typed at once at a free prompt, else held for the wake's tick. A request id already answered gets
/// its receipt again and types nothing.
pub(crate) async fn submit(rows: Workspaces, request_id: String, workspace_id: String, handle: String, line: String) -> Result<Receipt> {
    let request_id = if request_id.is_empty() { minted_id() } else { request_id };
    if let Some(done) = receipts().into_iter().rev().find(|r| r.request_id == request_id) {
        return Ok(done);
    }
    if let Err(reason) = page_reply::check_line(&line) {
        return Ok(record(Receipt { request_id, workspace_id, handle, outcome: "refused", detail: reason.to_string() }));
    }
    tokio::task::spawn_blocking(move || {
        let question = question_of(&handle).unwrap_or_default();
        let held = Held { request_id, workspace_id, handle, line, question };
        supersede(&held);
        let (outcome, detail) = attempt(&rows, &held.workspace_id, &held.handle, &held.line);
        settle(held, outcome, detail)
    }).await.context("page reply submission")
}

/// The wake's tick: each held reply gets one attempt, unless its row's question has changed since the page answered.
pub(crate) async fn release(rows: &Workspaces) {
    let held: Vec<Held> = HELD.lock().unwrap_or_else(|p| p.into_inner()).values().cloned().collect();
    for held in held {
        let rows = rows.clone();
        let _ = tokio::task::spawn_blocking(move || {
            // An unreadable registry keeps the hold for the next tick.
            let Ok(question) = question_of(&held.handle) else { return };
            if question != held.question {
                tracing::info!(target: "page_reply", handle = %held.handle, workspace = %held.workspace_id,
                    line = ?held.line, "page reply ended: question_changed");
                settle(held, "refused", "question_changed: the row's question is not the one answered".into());
                return;
            }
            let (outcome, detail) = attempt(&rows, &held.workspace_id, &held.handle, &held.line);
            settle(held, outcome, detail);
        }).await;
    }
}

/// A newer reply for a row ends the one it holds.
fn supersede(new: &Held) {
    let old = {
        let mut map = HELD.lock().unwrap_or_else(|p| p.into_inner());
        match map.get(&new.workspace_id) {
            Some(old) if old.request_id != new.request_id => map.remove(&new.workspace_id),
            _ => None,
        }
    };
    if let Some(old) = old {
        tracing::info!(target: "page_reply", handle = %old.handle, workspace = %old.workspace_id,
            line = ?old.line, "page reply ended: superseded");
        record(receipt(&old, "refused", "superseded: a newer reply replaced it".into()));
    }
}

/// A busy row keeps the reply (while it is still the row's held one); anything else ends it with its receipt.
fn settle(held: Held, outcome: &'static str, detail: String) -> Receipt {
    let busy = matches!(outcome, "not_free" | "not_ready") && refusal_name(&detail) == "busy";
    let mut map = HELD.lock().unwrap_or_else(|p| p.into_inner());
    let current = map.get(&held.workspace_id).map(|h| h.request_id.clone());
    if busy {
        if current.as_ref().is_some_and(|id| *id != held.request_id) {
            drop(map);
            return record(receipt(&held, "refused", "superseded: a newer reply replaced it".into()));
        }
        map.insert(held.workspace_id.clone(), held.clone());
        drop(map);
        return record(receipt(&held, "held", detail));
    }
    if current.as_deref() == Some(held.request_id.as_str()) {
        map.remove(&held.workspace_id);
    }
    drop(map);
    let (outcome, detail) = match outcome {
        "sent" | "unknown" | "refused" => (outcome, detail),
        "not_free" | "not_ready" => ("refused", format!("{}: {detail}", refusal_name(&detail))),
        other => ("refused", format!("{other}: {detail}")),
    };
    record(receipt(&held, outcome, detail))
}

/// The gate's refusals that waiting does not clear: the person's unsent text (in the box, or the cursor past the prompt
/// after it), and a screen with no input box (a dialog).
fn refusal_name(reason: &str) -> &'static str {
    match reason {
        "input not empty" | "cursor not at the prompt" => "draft",
        "not in an input box" => "dialog",
        _ => "busy",
    }
}

fn receipt(held: &Held, outcome: &'static str, detail: String) -> Receipt {
    Receipt { request_id: held.request_id.clone(), workspace_id: held.workspace_id.clone(), handle: held.handle.clone(), outcome, detail }
}

fn record(receipt: Receipt) -> Receipt {
    let mut all = RECEIPTS.lock().unwrap_or_else(|p| p.into_inner());
    all.retain(|r| r.request_id != receipt.request_id);
    all.push_back(receipt.clone());
    while all.len() > RECEIPTS_KEPT { all.pop_front(); }
    receipt
}

/// An id for a reply whose sender named none: unique within this daemon's run, which is as long as a receipt lives.
fn minted_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    format!("daemon-{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// The question the handle's session declared, as the registry holds it. Blocks.
fn question_of(handle: &str) -> std::result::Result<String, ()> {
    let agents = crate::comm::registry::registry::try_read_comm_agents().map_err(drop)?;
    Ok(agents.get(handle).and_then(|e| e.get("question")).and_then(Value::as_str).unwrap_or_default().to_string())
}

#[cfg(test)]
#[path = "page_reply_tests.rs"]
mod tests;
