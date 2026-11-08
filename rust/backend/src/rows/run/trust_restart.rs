//! The trust restart: a Claude leg that stopped at Claude Code's folder-trust dialog, under the declared prefix, is
//! stopped and spawned again with its own argv, once. The spawn prepares trust again, so the new leg finds its record.
//! Never keys: the dialog's answer is the record, not a keystroke.
//!
//! Called by the dialog tick (`work_state/dialog.rs`) when a started leg's screen shows the dialog. It runs under the
//! row's guard, tried and never waited on; the generation, the leg record and the once-per-episode rule are the row's
//! (`Workspace::take_trust_leg`). The effects are behind [`TrustEffects`], which the tests fake.

use super::start::spawn_and_watch;
use crate::rows::gate::StartPermit;
use crate::rows::spawn::detach::sot_capsule_exe;
use crate::rows::spawn::state_root::state_dir_for;
use crate::rows::workspace::TrustTake;
use crate::rows::{Workspace, Workspaces};
use std::path::Path;
use std::time::Duration;

/// A Claude leg on the same trust file prepared less recently than this does not hold a restart back: Claude Code's
/// own start saves that file, and the restart's record is the same unlocked write.
const RECENT_START: Duration = Duration::from_secs(2);

/// What a restart attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustRestart {
    /// The leg was stopped and spawned again.
    Restarted,
    /// The leg was taken and the restart failed after it (a stop or a spawn); the episode is spent.
    Failed,
    /// Not now, nothing taken: the row's guard is busy or a Claude leg on its trust file started a moment ago. The next
    /// tick asks again.
    Waiting,
    /// No restart, and none to come: no open leg, a spent episode, a root outside the prefix, a moved generation, a
    /// row gone or a closing gate. The dialog is its person's.
    Left,
}

impl TrustRestart {
    /// Whether a restart ran or will be tried again, so the dialog on the screen read is about to be gone.
    pub(crate) fn pending(self) -> bool {
        self != TrustRestart::Left
    }
}

/// The effects of a restart, behind one name so the tests record them.
pub(crate) trait TrustEffects {
    /// Whether `root` lies under the declared trust prefix.
    fn covered(&self, root: &Path) -> bool;
    /// Whether a Claude leg on the row's trust file was prepared less than [`RECENT_START`] ago.
    fn recent_start(&self, row: &Workspace) -> bool;
    /// Ends the supervisor at `state_dir`, returning after its process ended.
    fn stop(&self, state_dir: &Path) -> Result<(), String>;
    /// Spawns the row's supervisor again with `argv`, resuming its run, watched.
    fn spawn(&self, permit: &StartPermit, exe: &Path, state_dir: &Path, argv: &[String], row: &Workspace, workspaces: &Workspaces)
        -> std::io::Result<&'static str>;
}

/// The real effects, each function named once.
pub(crate) struct LiveTrust;

impl TrustEffects for LiveTrust {
    fn covered(&self, root: &Path) -> bool {
        crate::agents::folder_trust::under_declared_prefix(root).unwrap_or_else(|e| {
            tracing::warn!(root = ?root, "folder-trust restart: the trust scope was not read, so none is made ({e})");
            false
        })
    }

    fn recent_start(&self, row: &Workspace) -> bool {
        // The key each preparation marks: the same home, account env and process environment `account_spawn_env` uses.
        let Some(home) = crate::agents::accounts::account_home() else { return false };
        let Ok(extra) = crate::agents::accounts::account_env("claude", &row.account(), &home) else { return false };
        crate::agents::env::effective_claude_trust_file(&home, &extra)
            .is_ok_and(|file| crate::agents::folder_trust::claude_prepared_within(&file, RECENT_START))
    }

    fn stop(&self, state_dir: &Path) -> Result<(), String> {
        sot_capsule::attach_client::supervisor_client::stop(state_dir).map_err(|e| e.to_string())
    }

    fn spawn(
        &self,
        permit: &StartPermit,
        exe: &Path,
        state_dir: &Path,
        argv: &[String],
        row: &Workspace,
        workspaces: &Workspaces,
    ) -> std::io::Result<&'static str> {
        spawn_and_watch(
            permit,
            exe,
            state_dir,
            sot_capsule::supervisor::StartMode::Resume,
            argv,
            &row.project_root,
            &row.agent_name(),
            row.workspace_id.clone(),
            row.slug.clone(),
            workspaces.clone(),
        )
    }
}

/// Tells why a leg is left, once: the take closed it (`Spent`, `Outside`).
fn say_left(id: &str, row: &Workspace, took: TrustTake) {
    match took {
        TrustTake::Spent => {
            tracing::info!(workspace_id = %id, "folder-trust dialog again before a main box: the row's one restart is spent, so its person answers");
        }
        TrustTake::Outside => {
            tracing::info!(workspace_id = %id, root = ?row.project_root, "folder-trust dialog outside the declared prefix: left to its person");
        }
        TrustTake::NotOpen | TrustTake::Leg(_) => {}
    }
}

/// Restarts row `id`'s leg at generation `generation`, whose screen shows the folder-trust dialog. BLOCKING: the stop
/// and the settle of the spawn run here. Order: the guard (tried), whether a leg is open, the steps with no side effect
/// (the scope, a recent start on the trust file, the run gate, the executable), then the take, which spends the episode, then the stop and
/// the spawn. The permit and the guard are held across both, so a close waits for them.
pub(crate) fn restart_at_trust_dialog(
    workspaces: &Workspaces,
    state_root: &Path,
    id: &str,
    generation: u64,
    fx: &dyn TrustEffects,
) -> TrustRestart {
    let Some(guard) = workspaces.capsule_guard(id) else { return TrustRestart::Left };
    let Ok(_held) = guard.try_lock() else { return TrustRestart::Waiting };
    let Some(row) = workspaces.resolve(Some(id)) else { return TrustRestart::Left };
    if row.spawn_generation() != generation {
        return TrustRestart::Left;
    }
    // Nothing to do, and nothing to look up, for a row whose leg is closed (spent, outside, or past its main box).
    if !row.trust_leg_open(generation) {
        return TrustRestart::Left;
    }
    if !fx.covered(&row.project_root) {
        say_left(id, &row, row.take_trust_leg(generation, false));
        return TrustRestart::Left;
    }
    if fx.recent_start(&row) {
        return TrustRestart::Waiting;
    }
    let Ok(permit) = workspaces.begin_start(id) else { return TrustRestart::Left };
    let exe = match sot_capsule_exe() {
        Ok(exe) => exe,
        Err(e) => {
            tracing::warn!(workspace_id = %id, "folder-trust restart: sot-capsule was not located, so the leg is left ({e})");
            return TrustRestart::Left;
        }
    };
    let argv = match row.take_trust_leg(generation, true) {
        TrustTake::Leg(argv) => argv,
        left => {
            say_left(id, &row, left);
            return TrustRestart::Left;
        }
    };
    tracing::info!(workspace_id = %id, argv = ?argv, "folder-trust dialog on a started leg under the declared prefix: restarting it");
    let state_dir = state_dir_for(state_root, id);
    if let Err(e) = fx.stop(&state_dir) {
        tracing::warn!(workspace_id = %id, argv = ?argv, "folder-trust restart: the leg did not stop, so it was left ({e})");
        return TrustRestart::Failed;
    }
    if let Err(e) = fx.spawn(&permit, &exe, &state_dir, &argv, &row, workspaces) {
        tracing::warn!(
            workspace_id = %id, argv = ?argv,
            "folder-trust restart: the leg stopped and did not spawn again; the row rests until it is opened ({e})"
        );
        return TrustRestart::Failed;
    }
    TrustRestart::Restarted
}

#[cfg(test)]
#[path = "trust_restart_tests.rs"]
mod tests;
