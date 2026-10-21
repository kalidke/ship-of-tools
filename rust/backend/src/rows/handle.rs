//! A row's comm handle: the declared cell, private to this module; [`bound_handle`], the one answer to which handle is
//! a row's; the two moves of a handle (`set_agent_handle`, `clear_shared_handles`); and the liveness stamp, which keeps
//! the handles of the rows that run fresh in the comm registry, so an idle row is live.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::comm::registry::registry::{iso8601_utc_now, stamp_last_seen};
use crate::rows::workspace::Phase;
use crate::rows::{Workspace, Workspaces};

/// The handle a row's session declared (`agent.join`), `""` until one does. Its value is private to this module: the
/// binding is read through [`bound_handle`], the declared value through [`declared_handle`], and it changes only
/// through `Workspaces::set_agent_handle`, `Workspaces::clear_shared_handles` and [`clear_declared`], so no reader
/// of a row's handle can bypass the rule.
#[derive(Default)]
pub(crate) struct Declared(Mutex<String>);

impl Declared {
    /// A cell holding `handle`: a row built from its toml, or a test's row.
    pub(crate) fn new(handle: impl Into<String>) -> Self {
        Declared(Mutex::new(handle.into()))
    }

    /// A cell holding this one's value, for a row that replaces another under its slug (`insert`, the boot re-seed).
    pub(crate) fn copy(&self) -> Self {
        Declared::new(self.get())
    }

    fn get(&self) -> String {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set(&self, handle: &str) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = handle.to_string();
    }

    /// Clears the cell if it holds `handle`; whether it did.
    fn clear_if(&self, handle: &str) -> bool {
        let mut cell = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let held = *cell == handle;
        if held {
            cell.clear();
        }
        held
    }
}

/// The handle row `ws`'s session declared, as recorded: what its toml and the list's `agent_handle` field carry, and
/// what the wake wakes, since a session that never joined has no mail to wake for. Every other reader of a row's
/// handle asks [`bound_handle`].
pub(crate) fn declared_handle(ws: &Workspace) -> String {
    ws.agent_handle.get()
}

/// Clears row `ws`'s declared handle: a default row reset to the inert anchor, in which nothing runs.
pub(crate) fn clear_declared(ws: &Workspace) {
    ws.agent_handle.set("");
}

/// Which comm handle is row `ws`'s: the handle its session declared (`agent.join`), else the name the row was created
/// with, unless another row of `rows` (the daemon's rows) declares that name, since a handle binds one row. `""` when
/// none. Every reader of a row's handle asks this: the list, the work-state, the liveness stamp, the wake and a join
/// from outside any row.
pub(crate) fn bound_handle(ws: &Workspace, rows: &[Arc<Workspace>]) -> String {
    let declared = ws.agent_handle.get();
    if !declared.is_empty() {
        return declared;
    }
    let name = ws.agent_name();
    let held_elsewhere = rows.iter().any(|r| r.workspace_id != ws.workspace_id && r.agent_handle.get() == name);
    if name.is_empty() || held_elsewhere {
        String::new()
    } else {
        name
    }
}

/// How often the held handles are stamped when nothing changed.
const STAMP_EVERY: Duration = Duration::from_secs(60);

/// How often the task looks at the rows.
const TICK: Duration = Duration::from_secs(2);

impl Workspaces {
    /// Record the sot-comm handle a session inside `workspace_id` DECLARED
    /// via `agent.join` (ADR 0046 decision 1) — the daemon is told once
    /// instead of re-deriving it from a self-file read-back on every
    /// row-handle read (`rows::handle::bound_handle`) — and MOVE it: ADR 0049
    /// gives a handle to one row only, so every OTHER row whose declared
    /// handle equals `handle` loses it (cell cleared) in the same step.
    /// Returns the row and the ids of the rows that lost the handle, for
    /// the caller to persist; an empty `handle` moves nothing. `None` if
    /// `workspace_id` is not registered (the `agent.join` handler reports
    /// this as `unknown_workspace`).
    ///
    /// (S6, Codex finding B5): a GUARDED IN-PLACE update,
    /// never a replacement `insert`. The write lands on each row's OWN
    /// interior `agent_handle` cell (the SAME `Arc<Workspace>`, resource
    /// caches — kernel, repl, watcher — untouched), not on a freshly
    /// reconstructed `Workspace` that would start every cache cold. This
    /// also closes the destroy race the old replace-based version had:
    /// since this method never touches `by_id`/`by_slug`, a `destroy` that
    /// removes the row between the lookup and this write leaves the row
    /// destroyed — the write lands on an orphaned `Arc` nobody can
    /// `resolve()` to anymore, never a resurrection.
    ///
    /// The registry's WRITE lock is held across the whole move, so two
    /// concurrent declarations of one handle serialise and the later one
    /// wins; a cell is locked only for one clone or one write, so taking
    /// cells under the registry lock cannot deadlock.
    pub fn set_agent_handle(&self, workspace_id: &str, handle: &str) -> Option<(Arc<Workspace>, Vec<String>)> {
        let g = self.inner.write().expect("workspaces lock");
        let ws = g.by_id.get(workspace_id)?.clone();
        let mut moved = Vec::new();
        if !handle.is_empty() {
            for (id, other) in g.by_id.iter() {
                if id == workspace_id {
                    continue;
                }
                if other.agent_handle.clear_if(handle) {
                    moved.push(id.clone());
                }
            }
        }
        ws.agent_handle.set(handle);
        Some((ws, moved))
    }

    /// Boot's side of one row per handle (ADR 0049): every non-empty
    /// declared handle that two or more rows hold stays only on the row
    /// `keep(handle)` names (a workspace id) and is cleared on the rest; a
    /// `keep` that names none of them clears it on all. `keep` runs with
    /// no lock held; the clears run under the registry's write lock, like
    /// [`set_agent_handle`](Self::set_agent_handle)'s. Returns each such
    /// handle with the rows it was cleared on, for the caller to save.
    pub(crate) fn clear_shared_handles(
        &self,
        keep: impl Fn(&str) -> Option<String>,
    ) -> Vec<(String, Vec<Arc<Workspace>>)> {
        let mut by_handle: std::collections::BTreeMap<String, Vec<Arc<Workspace>>> = Default::default();
        for ws in self.list() {
            let h = ws.agent_handle.get();
            if !h.is_empty() {
                by_handle.entry(h).or_default().push(ws);
            }
        }
        let shared: Vec<(String, Vec<Arc<Workspace>>, Option<String>)> = by_handle
            .into_iter()
            .filter(|(_, rows)| rows.len() > 1)
            .map(|(h, rows)| {
                let kept = keep(&h);
                (h, rows, kept)
            })
            .collect();
        let _g = self.inner.write().expect("workspaces lock");
        let mut out = Vec::new();
        for (handle, rows, kept) in shared {
            let mut cleared = Vec::new();
            for ws in rows {
                if kept.as_deref() == Some(ws.workspace_id.as_str()) {
                    continue;
                }
                if ws.agent_handle.clear_if(&handle) {
                    cleared.push(ws.clone());
                }
            }
            out.push((handle, cleared));
        }
        out
    }

}

/// The non-empty handles of the rows whose phase `keep` accepts. A row's handle is resolved (a registry read) only
/// once its phase is kept.
fn handles_where<F: FnOnce() -> String>(
    rows: impl IntoIterator<Item = (Phase, F)>,
    keep: fn(Phase) -> bool,
) -> BTreeSet<String> {
    rows.into_iter()
        .filter(|(phase, _)| keep(*phase))
        .map(|(_, resolve)| resolve())
        .filter(|handle| !handle.is_empty())
        .collect()
}

/// The non-empty handles of Starting or Ready rows: the sessions a daemon is running now.
pub(crate) fn held<F: FnOnce() -> String>(rows: impl IntoIterator<Item = (Phase, F)>) -> BTreeSet<String> {
    handles_where(rows, |phase| matches!(phase, Phase::Starting | Phase::Ready))
}

/// The non-empty handles of every row this daemon has not seen end: all but the latched Terminal and
/// EndedNoRespawn. A row loaded from disk is Stopped until its first probe, and Unreachable, Foreign and Ending
/// rows may still run a session, so none of them frees its handle.
pub(crate) fn unended<F: FnOnce() -> String>(rows: impl IntoIterator<Item = (Phase, F)>) -> BTreeSet<String> {
    handles_where(rows, |phase| !phase.is_latched())
}

/// The handles this daemon's running rows hold: each Starting or Ready row's handle by
/// [`bound_handle`], given the daemon's whole row list, so a row never answers to a stored name
/// that another row declares (ADR 0049, one row per handle).
pub(crate) fn held_handles(rows: &[Arc<Workspace>]) -> BTreeSet<String> {
    held(rows.iter().map(|ws| (ws.phase(), move || bound_handle(ws, rows))))
}

/// The handles of every row not seen end, by the same rule as `held_handles`: what a session outside any row
/// may not take.
pub(crate) fn unended_handles(rows: &[Arc<Workspace>]) -> BTreeSet<String> {
    unended(rows.iter().map(|ws| (ws.phase(), move || bound_handle(ws, rows))))
}

/// A pass is due when the held set changed or `STAMP_EVERY` has passed since the last one.
fn due(held: &BTreeSet<String>, last_held: &BTreeSet<String>, since_last_pass: Duration) -> bool {
    held != last_held || since_last_pass >= STAMP_EVERY
}

/// Every `TICK`, stamp `last_seen` for the handles of the rows this daemon runs, when a pass is
/// due. A failed pass is one warning and is retried at the next due pass.
pub(crate) async fn liveness(workspaces: Workspaces) {
    let mut last_held = BTreeSet::new();
    let mut last_pass: Option<Instant> = None;
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        let rows = workspaces.list();
        let Ok(now_held) = tokio::task::spawn_blocking(move || {
            held_handles(&rows)
        })
        .await
        else {
            tracing::warn!("row liveness handles could not be resolved; retrying");
            continue;
        };
        let since = last_pass.map_or(STAMP_EVERY, |t| t.elapsed());
        if (now_held.is_empty() && last_held.is_empty()) || !due(&now_held, &last_held, since) {
            continue;
        }
        let (handles, stamp) = (now_held.clone(), iso8601_utc_now());
        let rows = handles.clone();
        match tokio::task::spawn_blocking(move || stamp_last_seen(&handles, &stamp)).await {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                ?rows,
                "row liveness stamp failed: the comm registry could not be locked, read or written; once a row's last stamp is ten minutes old, sends to it fail as no live session"
            ),
            Err(e) => tracing::warn!("row liveness stamp task failed: {e}"),
        }
        last_pass = Some(Instant::now());
        last_held = now_held;
    }
}

#[cfg(test)]
#[path = "handle_tests.rs"]
mod tests;
