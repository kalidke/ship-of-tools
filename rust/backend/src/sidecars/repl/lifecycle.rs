//! The REPL child's lifecycle cell: states, spawn generations and the `lifecycle` frames announcing them.

use super::*;

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
) -> u64 {
    let my_gen = {
        let mut c = cell.lock().unwrap_or_else(|e| e.into_inner());
        c.gen += 1;
        c.state = ReplLifecycle::Starting;
        c.gen
    };
    // A respawn takes ownership of the workspace's proxy grants: the OLD
    // child's browser-served ports are revoked here, not only on its Dead
    // transition — a restart-raced stale supervisor's Dead is gen-rejected
    // (below), so this is the path that reliably clears them.
    crate::proxy::revoke_browser_ports(browser_ports_key(workspace_id));
    emit_lifecycle(frame_tx, workspace_id, ReplLifecycle::Starting);
    my_gen
}

/// Key for the per-workspace browser-served-port grants (`crate::proxy`).
/// The legacy singleton REPL has no workspace id; give it a fixed key so its
/// grants are still tracked and revoked.
pub(super) fn browser_ports_key(workspace_id: &Option<String>) -> &str {
    workspace_id.as_deref().unwrap_or("<legacy>")
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
    });
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn cell() -> SharedLifecycle {
        Arc::new(std::sync::Mutex::new(LifecycleCell {
            gen: 0,
            state: ReplLifecycle::NotStarted,
        }))
    }

    fn state_of(c: &SharedLifecycle) -> ReplLifecycle {
        c.lock().unwrap().state
    }

    /// Drain every lifecycle announcement currently queued on the bus.
    fn drain_states(rx: &mut broadcast::Receiver<ReplFrameMsg>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            assert_eq!(msg.eval_id, 0, "lifecycle frames are not eval output");
            let kind = msg.frame.get("kind").and_then(Value::as_str).unwrap();
            assert_eq!(kind, "lifecycle");
            out.push(
                msg.frame
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap()
                    .to_string(),
            );
        }
        out
    }

    #[test]
    fn begin_starting_bumps_gen_sets_state_and_announces() {
        let (tx, mut rx) = broadcast::channel(8);
        let c = cell();
        let g1 = lifecycle_begin_starting(&c, &tx, &None);
        assert_eq!(g1, 1);
        assert_eq!(state_of(&c), ReplLifecycle::Starting);
        assert_eq!(drain_states(&mut rx), vec!["starting"]);
        // A respawn opens the next generation.
        let g2 = lifecycle_begin_starting(&c, &tx, &None);
        assert_eq!(g2, 2);
        assert_eq!(drain_states(&mut rx), vec!["starting"]);
    }

    #[test]
    fn transition_applies_once_and_is_change_gated() {
        let (tx, mut rx) = broadcast::channel(8);
        let c = cell();
        let g = lifecycle_begin_starting(&c, &tx, &None);
        let _ = drain_states(&mut rx);
        // First stdout line: Starting -> Ready, announced once.
        assert!(lifecycle_transition(&c, g, ReplLifecycle::Ready, &tx, &None));
        assert_eq!(state_of(&c), ReplLifecycle::Ready);
        // Every subsequent line: same-state no-op, no announcement (this is
        // what makes the per-line call in the supervisor loop free).
        assert!(!lifecycle_transition(&c, g, ReplLifecycle::Ready, &tx, &None));
        assert_eq!(drain_states(&mut rx), vec!["ready"]);
    }

    #[test]
    fn stale_generation_cannot_stomp_a_respawned_child() {
        // THE race this guard exists for: restart_with_project drops the old
        // supervisor and immediately spawns a new child (Starting). The OLD
        // task then notices its channel closed and reports Dead — which must
        // NOT overwrite (or mis-announce over) the fresh child's Starting.
        let (tx, mut rx) = broadcast::channel(8);
        let c = cell();
        let old_gen = lifecycle_begin_starting(&c, &tx, &None);
        let _new_gen = lifecycle_begin_starting(&c, &tx, &None); // respawn
        let _ = drain_states(&mut rx);
        assert!(!lifecycle_transition(
            &c,
            old_gen,
            ReplLifecycle::Dead,
            &tx,
            &None
        ));
        assert_eq!(state_of(&c), ReplLifecycle::Starting);
        assert_eq!(drain_states(&mut rx), Vec::<String>::new());
    }

    #[test]
    fn current_generation_death_is_reported() {
        let (tx, mut rx) = broadcast::channel(8);
        let c = cell();
        let g = lifecycle_begin_starting(&c, &tx, &None);
        assert!(lifecycle_transition(&c, g, ReplLifecycle::Ready, &tx, &None));
        assert!(lifecycle_transition(&c, g, ReplLifecycle::Dead, &tx, &None));
        assert_eq!(state_of(&c), ReplLifecycle::Dead);
        assert_eq!(drain_states(&mut rx), vec!["starting", "ready", "dead"]);
    }

    #[test]
    fn wire_words_match_protocol_vocabulary() {
        assert_eq!(ReplLifecycle::NotStarted.as_str(), "not_started");
        assert_eq!(ReplLifecycle::Starting.as_str(), "starting");
        assert_eq!(ReplLifecycle::Ready.as_str(), "ready");
        assert_eq!(ReplLifecycle::Dead.as_str(), "dead");
    }
}
