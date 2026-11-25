//! workspace.restart: a Ready Codex session's agent is stopped and started again with the row's own recipe, so the new
//! leg carries the row's hooks and names its conversation, and mail wakes it (messaging's `not_woken_by_mail`).
//!
//! Every check comes before any effect, in this order: the session, whether it is the asker's own, its kind, its phase,
//! whether it is marked not woken by mail (a session that is not marked is left alone), a running turn (a registry that cannot be
//! read refuses too), the row's guard (tried, never waited on) and the run gate. Then the recipe's argv and the executable are resolved, and only then the
//! stop and the spawn, under the guard and the start permit held across both, as the trust restart does. The new leg is a
//! new conversation. A turn that begins between the turn check and the stop ends with the program, and text typed in the
//! session's input and not sent is lost: the op reads no screen.

use anyhow::Result;
use serde_json::{json, Value};
use sot_protocol::op;
use sot_protocol::reply::HandlerOutput;
use sot_protocol::{Frame, WorkspaceRestartReq, WorkspaceRestartRes};

use crate::rows::run::respawn::{LiveRespawn, Respawn};
use crate::rows::spawn::detach::sot_capsule_exe;
use crate::rows::spawn::state_root::state_dir_for;
use crate::rows::workspace::Phase;
use crate::rows::{Workspace, Workspaces};
use std::path::Path;

/// A refusal before any effect: the ordinary `{error, code}` payload.
#[derive(Debug, PartialEq, Eq)]
struct Refused {
    code: &'static str,
    error: String,
}

fn refuse(code: &'static str, error: impl Into<String>) -> Refused {
    Refused { code, error: error.into() }
}

/// Who asks: the session as typed, the row the asker runs in (`""` for none; a claim, as a `pty.input` origin is, not
/// proof) and the asking connection's client id, for the log.
struct Asked<'a> {
    session: &'a str,
    by_row: &'a str,
    client: &'a str,
}

/// What `restart` reads and does besides the registry of rows, so the tests answer for it: whether a turn runs in a row
/// (or why that cannot be told), whether mail does not wake the session whose state folder is given, the row's Codex argv
/// for its folder, and the stop and the spawn.
struct World<'a> {
    turn_runs: &'a dyn Fn(&Workspace) -> Result<bool, String>,
    not_woken: &'a dyn Fn(&Path) -> bool,
    recipe: &'a dyn Fn(&Path) -> Result<Vec<String>, String>,
    fx: &'a dyn Respawn,
}

/// Restarts the agent of the session `asked.session` (a slug or an id). BLOCKING: the stop and the settle of the spawn run
/// here.
fn restart(workspaces: &Workspaces, state_root: &Path, asked: &Asked, world: &World) -> Result<WorkspaceRestartRes, Refused> {
    let (session, by) = (asked.session, asked.client);
    let gone = || refuse("unknown_workspace", format!("this computer runs no session named {session}."));
    let Some(row) = workspaces.resolve(Some(session)) else { return Err(gone()) };
    let (id, slug) = (row.workspace_id.clone(), row.slug.clone());
    // Whatever name it was given: a session never restarts its own agent, which would end the program running this.
    if workspaces.resolve(Some(asked.by_row)).is_some_and(|own| own.workspace_id == id) {
        return Err(refuse("own_session", "a session cannot restart its own agent. Ask its person to run this in another session or a shell on this computer."));
    }
    let agent = row.agent();
    if agent != "codex" {
        let kind = if agent == "none" { "no agent" } else { agent.as_str() };
        return Err(refuse("not_codex", format!("{slug} runs {kind}, not Codex; only a Codex session's agent is started again this way.")));
    }
    let phase = row.shown_phase();
    if phase != Phase::Ready.as_wire_str() {
        return Err(refuse(
            "not_ready",
            format!("{slug} is not running ({phase}). Open it in the window to start it, and run this again if it is still marked not woken by mail."),
        ));
    }
    let state_dir = state_dir_for(state_root, &id);
    if !(world.not_woken)(&state_dir) {
        return Err(refuse("not_needed", format!("{slug} is not marked not woken by mail, so a restart is not needed and would end its conversation.")));
    }
    match (world.turn_runs)(&row) {
        Ok(false) => {}
        Ok(true) => return Err(refuse("busy", format!("{slug} is in a turn. Run this again when the turn has ended."))),
        Err(why) => return Err(refuse("registry_unreadable", format!("whether {slug} is in a turn could not be told ({why}), so its agent was left as it is."))),
    }
    let Some(guard) = workspaces.capsule_guard(&id) else { return Err(gone()) };
    let Ok(_held) = guard.try_lock() else {
        return Err(refuse("in_progress", format!("{slug} is being started or ended already. Run this again in a moment.")));
    };
    if workspaces.resolve(Some(&id)).is_none() {
        return Err(gone());
    }
    let Ok(permit) = workspaces.begin_start(&id) else { return Err(refuse("closing", "this computer's Ship of Tools is closing.")) };
    let argv = (world.recipe)(&row.project_root).map_err(|e| refuse("launcher_unresolved", e))?;
    let exe = sot_capsule_exe().map_err(|e| refuse("capsule_unlocated", format!("sot-capsule was not located next to this daemon: {e}")))?;
    let done = |outcome: &str, detail: String| WorkspaceRestartRes { outcome: outcome.to_string(), workspace_id: id.clone(), slug: slug.clone(), detail };
    if let Err(e) = world.fx.stop(&state_dir) {
        tracing::warn!(workspace_id = %id, slug = %slug, by, error = %e, "workspace.restart: the agent did not stop, so none was started");
        return Ok(done("stop_failed", e));
    }
    if let Err(e) = world.fx.spawn(&permit, &exe, &state_dir, &argv, &row, workspaces) {
        tracing::warn!(workspace_id = %id, slug = %slug, by, error = %e, "workspace.restart: the agent stopped and did not start again; the session rests until it is opened");
        return Ok(done("not_started", e.to_string()));
    }
    tracing::info!(workspace_id = %id, slug = %slug, by, "workspace.restart: the session's Codex agent was started again");
    Ok(done("restarted", String::new()))
}

fn refusal(req_id: u64, r: &Refused) -> Result<HandlerOutput> {
    Ok(vec![(Frame::res(req_id, op::WORKSPACE_RESTART, json!({ "error": r.error, "code": r.code })), None)])
}

/// `workspace.restart`. The check, the stop and the settle run on the blocking pool, and run to their end if the
/// connection closes first, only the reply being lost.
pub async fn handle_workspace_restart(req_id: u64, payload: Value, workspaces: &Workspaces, client_id: &str) -> Result<HandlerOutput> {
    let req: WorkspaceRestartReq = match serde_json::from_value(payload) {
        Ok(req) => req,
        Err(e) => return refusal(req_id, &refuse("bad_request", format!("workspace.restart payload: {e}"))),
    };
    let Some(state_root) = sot_platform::host::state_dir::sot_state_dir() else {
        let hint = crate::rows::spawn::state_root::STATE_ROOT_HINT;
        return refusal(req_id, &refuse("no_state_root", format!("could not resolve this machine's state root ({hint} unset)")));
    };
    let (workspaces, client) = (workspaces.clone(), client_id.to_string());
    let answer = tokio::task::spawn_blocking(move || {
        let host = crate::rows::store::declared_host();
        // Read from the registry fresh, and a registry that cannot be read is an answer of its own, never "no turn".
        let turn_runs = |row: &Workspace| {
            let agents = crate::comm::registry::registry::try_read_comm_agents()?;
            Ok(crate::rows::work_state::turn_runs(row, &workspaces.list(), Some(&serde_json::Value::Object(agents)), &host))
        };
        let recipe = |root: &Path| crate::agents::argv::agent_argv("codex", Some(root));
        let world = World { turn_runs: &turn_runs, not_woken: &crate::comm::wake::not_woken_by_mail, recipe: &recipe, fx: &LiveRespawn };
        restart(&workspaces, &state_root, &Asked { session: &req.session, by_row: &req.by, client: &client }, &world)
    })
    .await?;
    match answer {
        Ok(res) => Ok(vec![(Frame::res(req_id, op::WORKSPACE_RESTART, serde_json::to_value(res)?), None)]),
        Err(r) => refusal(req_id, &r),
    }
}

#[cfg(test)]
#[path = "restart_tests.rs"]
mod tests;
