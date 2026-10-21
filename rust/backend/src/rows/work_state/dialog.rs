//! The dialog tick (ADR 0044, dialog amendment): a Claude row whose screen shows a dialog that waits on a person
//! reads red, whatever its turn state.
//!
//! No Claude Code hook says when a permission dialog is on screen (its `PermissionRequest` fires when a request is
//! queued), so the daemon reads the screen. Every [`DIALOG_EVERY`] one blocking task reads the registry once, then each
//! Ready capsule row whose agent is `claude` and whose bound handle has an entry, but one red by its question ([`due`]:
//! an AskUserQuestion menu, whose PreToolUse hook wrote the question before the menu drew, keeps that summary), one
//! watcher attach each: it never takes the pen, resizes nothing, writes no record frame and logs nothing. sot-log's
//! `a_sweep_of_watcher_reads_writes_nothing_and_stays_bounded` shows that over twenty capsules and measures a sweep of
//! them at 1.2-2.0 s (about 80 ms a read) in a debug build on Linux. [`dialog_of`] reads the screen; the row is
//! written only where screen and registry disagree ([`stamp_for`]), through work-state's one write, in this process
//! (`work_state::write_event`). Nothing lives between ticks, so after a restart the first tick clears a fact whose dialog
//! has gone. A Codex row is never read: its dialog fact is its own hook's. On a Claude row the daemon is the fact's one
//! writer (no Claude hook sends it, and this write leaves no marker for a heartbeat to find); a `prompt` or a `stop`
//! still deletes it in the reduction, and the next tick writes it again while the dialog shows. Limits: a dialog that
//! closes between a tick's read and its write reads red until the next tick; a turn that ends with no Stop (Esc, a
//! bare No, a reauth, an API error) leaves the row `working`, so a select menu a person opens there (/model, /resume)
//! reads red until it closes, as it does on a row at rest.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::comm::wake::screen::{dialog_of, wake_lines};
use crate::rows::handle::bound_handle;
use crate::rows::run::headless::{attach, checkpointed, SHUTDOWN_WAIT};
use crate::rows::Workspaces;

use super::{write_event, Event, StampError};

pub const DIALOG_EVERY: Duration = Duration::from_secs(5);
const CONTROLLER_ID: &str = "sot-comm-dialog";
/// The attach and its checkpoint, per row.
const READ_BUDGET: Duration = Duration::from_secs(3);
/// One stamp's wait for the registry lock.
const STAMP_BOUND: Duration = Duration::from_secs(10);

/// The write a row needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stamp {
    Dialog(String),
    Resumed,
}

/// Whether the tick reads a row: every row but one red by its `question` and holding no `dialog` fact (an
/// AskUserQuestion menu, whose PreToolUse hook wrote the question before the menu drew, keeps that summary). A row at
/// rest is read too: a background agent's dialog after its lead's Stop waits on the person as much as one in a turn.
fn due(entry: &serde_json::Value) -> bool {
    has_fact(entry) || entry.get("state").and_then(|s| s.as_str()) != Some("blocked")
}

fn has_fact(entry: &serde_json::Value) -> bool {
    entry.get("dialog").is_some_and(|d| !d.is_null())
}

/// What a [`due`] row's screen (`shown`, from [`dialog_of`]) and its entry call for: `dialog` when a dialog shows and
/// the entry has no fact; `resumed` when none shows and it has one; else nothing.
fn stamp_for(shown: Option<String>, entry: &serde_json::Value) -> Option<Stamp> {
    match shown {
        Some(text) if !has_fact(entry) => Some(Stamp::Dialog(text)),
        None if has_fact(entry) => Some(Stamp::Resumed),
        _ => None,
    }
}

/// The tick. Runs forever; started once from `server::run`, its sweeps paced by [`every`].
pub(crate) async fn run(comm_home: PathBuf, state_root: PathBuf, workspaces: Workspaces, period: Duration) {
    let failing = std::sync::Arc::new(std::sync::Mutex::new(HashSet::<String>::new()));
    every(period, || {
        let all = workspaces.list();
        let rows: Vec<(String, PathBuf)> = all
            .iter()
            .filter(|ws| ws.runtime == "capsule" && ws.agent() == "claude")
            .map(|ws| (bound_handle(ws, &all), crate::rows::spawn::state_root::state_dir_for(&state_root, &ws.workspace_id)))
            .filter(|(handle, _)| !handle.is_empty())
            .collect();
        let (home, failing) = (comm_home.clone(), failing.clone());
        async move {
            if rows.is_empty() {
                return;
            }
            let _ = tokio::task::spawn_blocking(move || {
                let mut held = failing.lock().unwrap_or_else(|e| e.into_inner());
                check_rows(&home, &rows, &read_screen, &stamp_dialog, &mut held);
            })
            .await;
        }
    })
    .await
}

/// Runs `sweep` every `period`: each sweep is awaited before the next tick, and ticks missed while it ran are skipped,
/// so a sweep that runs past `period` is followed by one more at once and never by a queue, and two never overlap.
async fn every<F, Fut>(period: Duration, mut sweep: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        sweep().await;
    }
}

/// One tick over `rows` (handle, state dir): the registry read once, then each row with an entry read and, where
/// [`stamp_for`] says so, written. A failed write is warned once per run of failures (`failing` holds the handles).
fn check_rows(
    home: &Path,
    rows: &[(String, PathBuf)],
    read: &dyn Fn(&Path) -> Seen,
    write: &dyn Fn(&str, &Stamp) -> Result<(), String>,
    failing: &mut HashSet<String>,
) {
    let Ok(bytes) = crate::comm::registry::registry::read_registry_fresh(&home.join("registry.json")) else {
        return;
    };
    let Ok(registry) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    for (handle, state_dir) in rows {
        let Some(entry) = registry.get("agents").and_then(|a| a.get(handle)).filter(|e| due(e)) else {
            continue;
        };
        // A leg the probe shows ended shows no dialog any more; one that does not answer keeps its fact.
        let shown = match read(state_dir) {
            Seen::Screen(lines) => dialog_of(&lines, "claude", cfg!(windows)),
            Seen::Ended => None,
            Seen::Unknown => continue,
        };
        let Some(s) = stamp_for(shown, entry) else {
            continue;
        };
        match write(handle, &s) {
            Ok(()) => {
                failing.remove(handle);
            }
            Err(e) => {
                if failing.insert(handle.clone()) {
                    tracing::warn!(handle = %handle, stamp = ?s, "comm dialog: the row's dialog fact was not written ({e})");
                }
            }
        }
    }
}

/// What one row's read found.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    /// The row is Ready and its screen arrived within [`READ_BUDGET`].
    Screen(Vec<String>),
    /// The leg is over (never started, ended with no respawn, terminal): no dialog shows on it.
    Ended,
    /// Starting, ending, unreachable or foreign, or a screen that did not arrive: nothing is known.
    Unknown,
}

/// The row's screen as the wake reads it, from one watcher attach, after one status probe.
fn read_screen(state_dir: &Path) -> Seen {
    use crate::rows::run::probe::{phase_of, phase_str};
    use sot_log::lane::wire::SupervisorPhase as SP;
    let phase = phase_of(state_dir);
    if phase != phase_str(SP::Ready) {
        let ended = [phase_str(SP::EndedNoRespawn), phase_str(SP::Terminal), crate::rows::run::probe::NEVER_STARTED_PHASE];
        return if ended.contains(&phase) { Seen::Ended } else { Seen::Unknown };
    }
    let Some(mut client) = attach(state_dir, CONTROLLER_ID).ok().and_then(|c| checkpointed(c, Instant::now() + READ_BUDGET).ok()) else {
        return Seen::Unknown;
    };
    let lines = wake_lines(client.screen());
    client.shutdown(SHUTDOWN_WAIT);
    Seen::Screen(lines)
}

/// The stamp `s` on `handle`'s entry: work-state's one write, in this process.
fn stamp_dialog(handle: &str, s: &Stamp) -> Result<(), String> {
    let event = match s {
        Stamp::Dialog(text) => Event::Dialog(text.clone()),
        Stamp::Resumed => Event::Resumed,
    };
    match write_event(handle, &event, STAMP_BOUND) {
        Ok(_) => Ok(()),
        Err(StampError::Refused) => Err("refused: a dialog with no text".to_string()),
        Err(StampError::Registry(e)) => Err(format!("{e:?}")),
    }
}

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod tests;
