//! The row liveness stamp: while a row runs, its handle's `last_seen` stays fresh, so an idle row is live.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::comm::registry::registry::{comm_handle_for_workspace, iso8601_utc_now, stamp_last_seen};
use crate::rows::workspace::Phase;
use crate::rows::{Workspace, Workspaces};

/// How often the held handles are stamped when nothing changed.
const STAMP_EVERY: Duration = Duration::from_secs(60);

/// How often the task looks at the rows.
const TICK: Duration = Duration::from_secs(2);

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
/// `comm_handle_for_workspace`, given the daemon's whole row list, so a row never answers to a handle
/// it reaches by a self-file or stored name that another row declares (ADR 0049, one row per handle).
pub(crate) fn held_handles(rows: &[Arc<Workspace>]) -> BTreeSet<String> {
    held(rows.iter().map(|ws| (ws.phase(), move || comm_handle_for_workspace(ws, rows))))
}

/// The handles of every row not seen end, by the same rule as `held_handles`: what a session outside any row
/// may not take.
pub(crate) fn unended_handles(rows: &[Arc<Workspace>]) -> BTreeSet<String> {
    unended(rows.iter().map(|ws| (ws.phase(), move || comm_handle_for_workspace(ws, rows))))
}

/// A pass is due when the held set changed or `STAMP_EVERY` has passed since the last one.
fn due(held: &BTreeSet<String>, last_held: &BTreeSet<String>, since_last_pass: Duration) -> bool {
    held != last_held || since_last_pass >= STAMP_EVERY
}

/// Every `TICK`, stamp `last_seen` for the handles of the rows this daemon runs, when a pass is
/// due. A failed pass is one warning and is retried at the next due pass.
pub(crate) async fn run(workspaces: Workspaces) {
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
#[path = "liveness_tests.rs"]
mod tests;
