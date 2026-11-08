//! The dialog tick (ADR 0044, dialog amendment): a Claude row whose screen shows a dialog that waits on a person
//! reads red, whatever its turn state. The same tick watches every Claude row whose started leg has not yet shown its
//! main input box, whatever its handle, entry or question: where [`startup_of`] finds Claude Code's folder-trust dialog
//! it asks rows' trust restart (`run/trust_restart.rs`) to rerun the leg, and writes no stamp for a screen about to be
//! gone; where it finds the main box it closes the leg record (`Workspace::trust_passed`). A row is read for its leg only
//! while its phase is `ready`.
//!
//! No Claude Code hook says when a permission dialog is on screen (its `PermissionRequest` fires when a request is
//! queued), so the daemon reads the screen. Every [`DIALOG_EVERY`] one blocking task reads the registry once (unread, only the
//! rows with a leg are read), then each Ready capsule row whose agent is `claude` and whose bound handle has an entry, but one red by its question ([`due`]:
//! an AskUserQuestion menu, whose PreToolUse hook wrote the question before the menu drew, keeps that summary), one
//! watcher attach each: it never takes the pen, resizes nothing, writes no record frame and logs nothing. sot-capsule's
//! `a_sweep_of_watcher_reads_writes_nothing_and_stays_bounded` shows that over twenty capsules and measures a sweep of
//! them at 1.2-2.0 s (about 80 ms a read) in a debug build on Linux. [`dialog_of`] reads the screen; the row is
//! written only where screen and registry disagree ([`stamp_for`]), through work-state's one write, in this process
//! (`work_state::write_event`). Each write is logged at info, and so is the first of each run of reads that found nothing
//! on a due row, with its reason, so a row that sat on a dialog and never read red names its cause in the log. Only those
//! runs live between ticks, so after a restart the first tick clears a fact whose dialog has gone. A Codex row is never read: its dialog fact is its own hook's. On a Claude row the daemon is the fact's one
//! writer (no Claude hook sends it, and this write leaves no marker for a heartbeat to find); a `prompt` or a `stop`
//! still deletes it in the reduction, and the next tick writes it again while the dialog shows. Limits: a dialog that
//! closes between a tick's read and its write reads red until the next tick; a turn that ends with no Stop (Esc, a
//! bare No, a reauth, an API error) leaves the row `working`, so a select menu a person opens there (/model, /resume)
//! reads red until it closes, as it does on a row at rest.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::comm::wake::screen::{dialog_of, startup_of, wake_lines, Startup};
use crate::rows::handle::bound_handle;
use crate::rows::run::headless::{attach, checkpointed, SHUTDOWN_WAIT};
use crate::rows::run::trust_restart::{restart_at_trust_dialog, LiveTrust, TrustEffects};
use crate::rows::workspace::Phase;
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

/// One row of a sweep: the handle its entry is under (empty when it has none), its state folder, and, while a started
/// leg has not shown its main box, the row's id and generation (`Workspace::trust_reading`).
struct Row {
    handle: String,
    state_dir: PathBuf,
    trust: Option<(String, u64)>,
}

/// The generation to read a row's screen for the folder-trust dialog: only while the row's phase can show a screen (a
/// resume over an ended run leaves a leg open with no agent).
fn trust_to_read(ws: &crate::rows::Workspace) -> Option<(String, u64)> {
    ws.trust_reading().filter(|_| ws.phase() == Phase::Ready).map(|generation| (ws.workspace_id.clone(), generation))
}

/// What a sweep leaves for the next, so each run of failures is told once: the handles whose last write failed (warned),
/// the handles whose last read found nothing (logged), and whether the registry was unreadable (warned).
#[derive(Debug, Default)]
struct Runs {
    failing: HashSet<String>,
    unread: HashSet<String>,
    registry: bool,
}

/// The tick. Runs forever; started once from `server::run`, its sweeps paced by [`every`].
pub(crate) async fn run(comm_home: PathBuf, state_root: PathBuf, workspaces: Workspaces, period: Duration) {
    let runs = std::sync::Arc::new(std::sync::Mutex::new(Runs::default()));
    every(period, || {
        let all = workspaces.list();
        let rows: Vec<Row> = all
            .iter()
            .filter(|ws| ws.runtime == "capsule" && ws.agent() == "claude")
            .map(|ws| Row {
                handle: bound_handle(ws, &all),
                state_dir: crate::rows::spawn::state_root::state_dir_for(&state_root, &ws.workspace_id),
                trust: trust_to_read(ws),
            })
            .filter(|row| !row.handle.is_empty() || row.trust.is_some())
            .collect();
        let (home, runs, workspaces, state_root) = (comm_home.clone(), runs.clone(), workspaces.clone(), state_root.clone());
        async move {
            if rows.is_empty() {
                return;
            }
            let _ = tokio::task::spawn_blocking(move || {
                let mut held = runs.lock().unwrap_or_else(|e| e.into_inner());
                let trust =
                    |id: &str, generation: u64, startup: Startup| trust_step(&workspaces, &state_root, id, generation, startup, &LiveTrust);
                check_rows(&home, &rows, &read_screen, &stamp_dialog, &trust, &mut held);
            })
            .await;
        }
    })
    .await
}

/// What a row's [`Startup`] calls for: the main box closes the leg record (false: the screen was read all the same); the
/// folder-trust dialog is the restart's, and true says it ran or will be tried again.
fn trust_step(workspaces: &Workspaces, state_root: &Path, id: &str, generation: u64, startup: Startup, fx: &dyn TrustEffects) -> bool {
    match startup {
        Startup::MainBox => {
            if let Some(ws) = workspaces.resolve(Some(id)) {
                ws.trust_passed(generation);
            }
            false
        }
        Startup::TrustDialog => restart_at_trust_dialog(workspaces, state_root, id, generation, fx).pending(),
        Startup::Neither => false,
    }
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

/// One tick over `rows`: the registry read once, then each row with an entry, or with a started leg to watch for the
/// folder-trust dialog, read once. A row with a leg hands the screen's [`Startup`] to `trust`, which answers whether a
/// restart ran or is to be tried again (the dialog is about to be gone, so no stamp is written for it); a row with an
/// entry is written where [`stamp_for`] says so. A write is logged; an unreadable registry, a read that found nothing and a failed
/// write are each told once per run (`runs`).
fn check_rows(
    home: &Path,
    rows: &[Row],
    read: &dyn Fn(&Path) -> Seen,
    write: &dyn Fn(&str, &Stamp) -> Result<(), String>,
    trust: &dyn Fn(&str, u64, Startup) -> bool,
    runs: &mut Runs,
) {
    let registry = crate::comm::registry::registry::read_registry_fresh(&home.join("registry.json"))
        .map_err(|e| format!("{e:?}"))
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|e| e.to_string()));
    let registry = match registry {
        Ok(registry) => {
            runs.registry = false;
            registry
        }
        Err(e) => {
            if !std::mem::replace(&mut runs.registry, true) {
                tracing::warn!("comm dialog: the registry was not read, so no row with an entry was ({e})");
            }
            // No entry is found, so only the rows with a leg to watch are read.
            serde_json::Value::Null
        }
    };
    for Row { handle, state_dir, trust: leg } in rows {
        let entry = registry.get("agents").and_then(|a| a.get(handle)).filter(|e| due(e));
        // A row with a leg to watch may have no handle; its state folder names it then.
        let name = if handle.is_empty() { state_dir.to_string_lossy().into_owned() } else { handle.clone() };
        if entry.is_none() && leg.is_none() {
            continue;
        }
        // A leg the probe shows ended shows no dialog any more; one that does not answer keeps its fact.
        let lines = match read(state_dir) {
            Seen::Unknown(why) => {
                if runs.unread.insert(name.clone()) {
                    tracing::info!(handle = %name, "comm dialog: the row's screen was not read ({why}); its fact stays as it is");
                }
                continue;
            }
            seen => {
                runs.unread.remove(&name);
                match seen {
                    Seen::Screen(lines) => Some(lines),
                    _ => None,
                }
            }
        };
        if let (Some((id, generation)), Some(lines)) = (leg, &lines) {
            if trust(id, *generation, startup_of(lines, "claude", cfg!(windows))) {
                continue;
            }
        }
        let Some(entry) = entry else { continue };
        let shown = lines.and_then(|lines| dialog_of(&lines, "claude", cfg!(windows)));
        let Some(s) = stamp_for(shown, entry) else {
            continue;
        };
        match write(handle, &s) {
            Ok(()) => {
                runs.failing.remove(handle);
                tracing::info!(handle = %handle, stamp = ?s, "comm dialog: the row's dialog fact was written");
            }
            Err(e) => {
                if runs.failing.insert(handle.clone()) {
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
    /// Starting, ending, unreachable or foreign, or a screen that did not arrive: nothing is known, for the reason given.
    Unknown(String),
}

/// The row's screen as the wake reads it, from one watcher attach, after one status probe.
fn read_screen(state_dir: &Path) -> Seen {
    use crate::rows::run::probe::{phase_of, phase_str};
    use sot_capsule::SupervisorPhase as SP;
    let phase = phase_of(state_dir);
    if phase != phase_str(SP::Ready) {
        let ended = [phase_str(SP::EndedNoRespawn), phase_str(SP::Terminal), crate::rows::run::probe::NEVER_STARTED_PHASE];
        return if ended.contains(&phase) { Seen::Ended } else { Seen::Unknown(format!("phase {phase}")) };
    }
    let client = match attach(state_dir, CONTROLLER_ID) {
        Ok(client) => client,
        Err(e) => return Seen::Unknown(format!("attach: {}", e.detail)),
    };
    let mut client = match checkpointed(client, Instant::now() + READ_BUDGET) {
        Ok(client) => client,
        Err(e) => return Seen::Unknown(format!("{} within {READ_BUDGET:?}: {}", e.phase, e.detail)),
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
