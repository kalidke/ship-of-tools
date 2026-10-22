//! The REPL child's lifecycle cell: states, spawn generations and the `lifecycle` frames announcing them.

use super::*;
use crate::sidecars::ports::{self, PortSink};

/// Lifecycle of the persistent REPL child, tracked by the supervisor so the
/// front-end can tell a *starting* (precompiling) REPL from a dead or silent
/// one. The per-package REPL env (#44) means the FIRST child in a workspace
/// precompiles that workspace's project — minutes of wall clock with zero
/// output frames, previously indistinguishable from a dead kernel.
///
/// Transitions: `NotStarted` → (`ensure_supervisor` spawn) → `Starting` →
/// (first stdout line: `using ShipToolsRepl` finished, serve loop up) →
/// `Ready` → (supervisor exit) → `Dead` → (next eval respawns) → `Starting` …
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReplLifecycle {
    NotStarted,
    Starting,
    Ready,
    Dead,
}

impl ReplLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            ReplLifecycle::NotStarted => "not_started",
            ReplLifecycle::Starting => "starting",
            ReplLifecycle::Ready => "ready",
            ReplLifecycle::Dead => "dead",
        }
    }
}

/// Shared lifecycle cell. `gen` is a spawn generation: each (re)spawn bumps it
/// and the supervisor task holds the gen it was spawned under, so a *stale*
/// task's writes (e.g. the old child's death racing a `restart_with_project`
/// respawn that is already `Starting`) are ignored instead of stomping the
/// fresh child's state.
pub(super) struct LifecycleCell {
    pub(super) gen: u64,
    pub(super) state: ReplLifecycle,
    /// What the next child says at its first eval (`ReplEnded::successor_notice`): set when the current generation
    /// dies, cleared when the next one opens.
    pub(super) notice: Option<&'static str>,
    /// The current generation's start (its age and stderr tail), set at its spawn and read while `state` is
    /// `Starting` (`Repl::still_starting`).
    pub(super) start: Option<crate::sidecars::child::Start>,
}

pub(super) type SharedLifecycle = Arc<std::sync::Mutex<LifecycleCell>>;

/// Begin a new spawn generation: bump `gen`, set `Starting`, and announce it
/// as a synthetic `lifecycle` frame on the repl.frame bus (same convention as
/// the supervisor's synthetic error/done close-out frames — the backend, not
/// the shim, fabricates control frames). Returns the new generation for the
/// supervisor task to hold. `eval_id` is 0: lifecycle frames are not eval
/// output; front-ends route them by the evt's `workspace_id`.
pub(super) fn lifecycle_begin_starting(
    cell: &SharedLifecycle,
    frame_tx: &broadcast::Sender<ReplFrameMsg>,
    workspace_id: &Option<String>,
    ports: &PortSink,
) -> u64 {
    let my_gen = {
        let mut c = cell.lock().unwrap_or_else(|e| e.into_inner());
        // From the process's one counter, so a row's later handle opens above this one (`ports::next_generation`).
        c.gen = ports::next_generation();
        c.state = ReplLifecycle::Starting;
        c.notice = None;
        c.gen
    };
    // A respawn takes ownership of the workspace's proxy grants: the OLD
    // child's browser-served ports end here, not only on its Dead
    // transition — a restart-raced stale supervisor's Dead is gen-rejected
    // (below), so this is the path that reliably clears them. The receiver
    // also learns the new generation, so the old child's late events are stale.
    ports.at(my_gen).end();
    emit_lifecycle(frame_tx, workspace_id, ReplLifecycle::Starting);
    my_gen
}

/// Apply a lifecycle transition IF `my_gen` is still the current spawn
/// generation and the state actually changes; emit the frame only when it
/// applied. Returns whether it applied. Keeps a stale supervisor's `Dead`
/// from overwriting (and mis-announcing over) a respawned child's `Starting`.
pub(super) fn lifecycle_transition(
    cell: &SharedLifecycle,
    my_gen: u64,
    to: ReplLifecycle,
    frame_tx: &broadcast::Sender<ReplFrameMsg>,
    workspace_id: &Option<String>,
) -> bool {
    let applied = {
        let mut c = cell.lock().unwrap_or_else(|e| e.into_inner());
        if c.gen == my_gen && c.state != to {
            c.state = to;
            true
        } else {
            false
        }
    };
    if applied {
        emit_lifecycle(frame_tx, workspace_id, to);
    }
    applied
}

fn emit_lifecycle(
    frame_tx: &broadcast::Sender<ReplFrameMsg>,
    workspace_id: &Option<String>,
    state: ReplLifecycle,
) {
    // Ignore send errors — no subscriber is fine, same as output frames.
    let _ = frame_tx.send(ReplFrameMsg {
        eval_id: 0,
        workspace_id: workspace_id.clone(),
        frame: serde_json::json!({ "kind": "lifecycle", "state": state.as_str() }),
        opener: Default::default(),
    });
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
