//! The dialog tick (ADR 0044, dialog amendment): a Claude row whose screen shows a dialog that waits on a person
//! reads red.
//!
//! No Claude Code hook says when a permission dialog is on screen (its `PermissionRequest` fires when a request is
//! queued), so the daemon reads the screen. Every [`DIALOG_EVERY`] one blocking task reads the registry once, then each
//! Ready capsule row whose agent is `claude` and whose handle is declared and has a registry entry, one watcher attach
//! each (it never takes the pen). [`dialog_of`] reads the screen; the row is written only where screen and registry
//! disagree ([`stamp_for`]), through `comm-status.sh`'s one reduction and its daemon entry, `SOT_COMM_ROW_HANDLE`
//! ([`stamp`]). Nothing lives between ticks, so after a restart the first tick clears a fact whose dialog has gone.
//! A Codex row is never read: its dialog fact is its own hook's. On a Claude row the daemon is the fact's one writer
//! (no Claude hook sends it, and the entry writes no marker for a heartbeat to find); a turn boundary still deletes
//! it in the reduction, and the next tick writes it again while the dialog shows. Unix only: the daemon has no route
//! to Git Bash on Windows, so there a Claude row on a dialog still reads green (`server::run` starts the tick).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::rows::run::headless::{attach, checkpointed, SHUTDOWN_WAIT};
use crate::rows::Workspaces;

use super::screen::{dialog_of, wake_lines};

pub const DIALOG_EVERY: Duration = Duration::from_secs(5);
const CONTROLLER_ID: &str = "sot-comm-dialog";
/// The attach and its checkpoint, per row.
const READ_BUDGET: Duration = Duration::from_secs(3);
/// One `comm-status.sh` run: its registry lock waits up to 10 s.
const STAMP_BOUND: Duration = Duration::from_secs(15);

/// The write a row needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stamp {
    Dialog(String),
    Resumed,
}

/// What a row's screen (`shown`, from [`dialog_of`]) and its registry entry call for: `dialog` when a dialog shows and the
/// entry does not already read `blocked` (a `dialog` fact reads blocked; so does an AskUserQuestion menu, red by the
/// `question` its PreToolUse hook wrote before the menu drew, which keeps that summary); `resumed` when no dialog shows
/// and the entry has the fact; else nothing.
fn stamp_for(shown: Option<String>, entry: &serde_json::Value) -> Option<Stamp> {
    match shown {
        Some(text) if entry.get("state").and_then(|s| s.as_str()) != Some("blocked") => Some(Stamp::Dialog(text)),
        None if entry.get("dialog").is_some_and(|d| !d.is_null()) => Some(Stamp::Resumed),
        _ => None,
    }
}

/// The tick. Runs forever; started once from `server::run`, on Unix only.
pub async fn run(comm_home: PathBuf, state_root: PathBuf, workspaces: Workspaces, period: Duration) {
    let mut failing: HashSet<String> = HashSet::new();
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let rows: Vec<(String, PathBuf)> = workspaces
            .list()
            .into_iter()
            .filter(|ws| ws.runtime == "capsule" && ws.agent() == "claude" && !ws.agent_handle().is_empty())
            .map(|ws| (ws.agent_handle(), crate::rows::spawn::state_root::state_dir_for(&state_root, &ws.workspace_id)))
            .collect();
        if rows.is_empty() {
            continue;
        }
        let home = comm_home.clone();
        let mut held = std::mem::take(&mut failing);
        if let Ok(f) = tokio::task::spawn_blocking(move || {
            check_rows(&home, &rows, &read_screen, &stamp, &mut held);
            held
        })
        .await
        {
            failing = f;
        }
    }
}

/// One tick over `rows` (handle, state dir): the registry read once, then each row with an entry read and, where
/// [`stamp_for`] says so, written. A failed write is warned once per run of failures (`failing` holds the handles).
fn check_rows(
    home: &Path,
    rows: &[(String, PathBuf)],
    read: &dyn Fn(&Path) -> Option<Vec<String>>,
    write: &dyn Fn(&Path, &str, &Stamp) -> Result<(), String>,
    failing: &mut HashSet<String>,
) {
    let Ok(bytes) = crate::comm::registry::registry::read_registry_fresh(&home.join("registry.json")) else {
        return;
    };
    let Ok(registry) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    for (handle, state_dir) in rows {
        let Some(entry) = registry.get("agents").and_then(|a| a.get(handle)) else {
            continue;
        };
        let Some(lines) = read(state_dir) else {
            continue;
        };
        let Some(s) = stamp_for(dialog_of(&lines, "claude", cfg!(windows)), entry) else {
            continue;
        };
        match write(home, handle, &s) {
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

/// The row's screen as the wake reads it, from one watcher attach; `None` unless the row is Ready and its screen
/// arrives within [`READ_BUDGET`].
fn read_screen(state_dir: &Path) -> Option<Vec<String>> {
    let ready = crate::rows::run::probe::phase_str(sot_log::lane::wire::SupervisorPhase::Ready);
    if crate::rows::run::probe::phase_of(state_dir) != ready {
        return None;
    }
    let mut client = checkpointed(attach(state_dir, CONTROLLER_ID).ok()?, Instant::now() + READ_BUDGET).ok()?;
    let lines = wake_lines(client.screen());
    client.shutdown(SHUTDOWN_WAIT);
    Some(lines)
}

/// `bash <home>/bin/comm-status.sh dialog "<text>"` (or `resumed`) for `handle`, through the daemon's entry
/// (`SOT_COMM_ROW_HANDLE`), contained and bounded by [`STAMP_BOUND`]. The child gets no row or session identity of its
/// own: the entry names the row, and the variables that would name one are removed.
fn stamp(home: &Path, handle: &str, s: &Stamp) -> Result<(), String> {
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(home.join("bin").join("comm-status.sh"));
    match s {
        Stamp::Dialog(text) => cmd.arg("dialog").arg(text),
        Stamp::Resumed => cmd.arg("resumed"),
    };
    cmd.current_dir(home)
        .env("SOT_COMM_HOME", home)
        .env("SOT_COMM_ROW_HANDLE", handle)
        .env_remove("SOT_WORKSPACE_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("SOT_COMM_SELF_FILE")
        .env_remove("SOT_COMM_HOOKS");
    let out = crate::lifecycle::child_signal::process().output(&mut cmd, STAMP_BOUND).map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{}: {}", out.status, String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or_default()))
    }
}

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod tests;
