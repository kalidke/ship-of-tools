//! The two effects that end a row's supervisor and start another over the same state folder: the trust restart's and
//! `workspace.restart`'s. Behind one trait so each caller's tests record them, implemented once by [`LiveRespawn`].

use super::start::spawn_and_watch;
use crate::rows::gate::StartPermit;
use crate::rows::{Workspace, Workspaces};
use std::path::Path;

pub(crate) trait Respawn {
    /// Ends the supervisor at `state_dir`, returning after its process ended.
    fn stop(&self, state_dir: &Path) -> Result<(), String>;
    /// Spawns the row's supervisor again with `argv`, resuming its run, watched.
    fn spawn(&self, permit: &StartPermit, exe: &Path, state_dir: &Path, argv: &[String], row: &Workspace, workspaces: &Workspaces)
        -> std::io::Result<&'static str>;
}

/// The real effects, each function named once.
pub(crate) struct LiveRespawn;

impl Respawn for LiveRespawn {
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
