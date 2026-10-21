use super::*;

fn cell() -> SharedLifecycle {
    Arc::new(std::sync::Mutex::new(LifecycleCell {
        gen: 0,
        state: ReplLifecycle::NotStarted,
        notice: None,
        start: None,
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
