use super::*;

#[test]
fn inert_default_anchor_is_the_default_row_with_no_agent_on_any_runtime() {
    let reg = Workspaces::new();
    let mut row = Workspace::from_label("local", PathBuf::from("/home/u"), false, "none".into(), String::new(), String::new());
    row.runtime = "capsule".to_string();
    let row = reg.insert(row);
    // Not the default yet -> an ordinary (if agent-less) capsule row.
    assert!(!reg.is_inert_default_anchor(&row));
    reg.set_default(&row.workspace_id);
    assert!(reg.is_inert_default_anchor(&row));
    // The runtime does not matter (2026-09-06): a tmux backend's default
    // row with no agent is the anchor too, and is hidden the same way.
    // Default capsule row that carries an agent: a session, not the anchor.
    let mut agent = Workspace::from_label("local", PathBuf::from("/home/u"), true, "claude".into(), String::new(), String::new());
    agent.runtime = "capsule".to_string();
    let agent = reg.insert(agent);
    assert!(!reg.is_inert_default_anchor(&agent));
}

/// Field defect (v0.6.0-rc.12): a default row that once carried an
/// agent (pre-dating the "nothing runs in the anchor" rule) never
/// went inert again after its run ended, because nothing ever reset
/// `agent` back to "none" — `reset_agent_to_none` is that reset, and
/// this proves it flips `is_inert_default_anchor` from false to true
/// while preserving the row's id and every other field.
#[test]
fn reset_agent_to_none_makes_a_carried_over_default_row_inert_again() {
    let reg = Workspaces::new();
    let mut row = Workspace::from_label(
        "local",
        PathBuf::from("/home/u"),
        true,
        "claude".into(),
        "kal-local".into(),
        "hello".into(),
    );
    row.runtime = "capsule".to_string();
    row.agent_handle = std::sync::Mutex::new("anchor-old-box".to_string());
    let row = reg.insert(row);
    reg.set_default(&row.workspace_id);
    let original_id = row.workspace_id.clone();
    assert!(
        !reg.is_inert_default_anchor(&row),
        "a default row that carries an agent is a real session, not the anchor"
    );

    let reset = reg
        .reset_agent_to_none(&row.workspace_id)
        .expect("the row is registered");
    assert_eq!(reset.workspace_id, original_id, "reset must preserve the id");
    assert_eq!(reset.agent(), "none");
    assert_eq!(reset.agent_name(), "");
    assert_eq!(reset.agent_handle(), "", "an inert anchor holds no handle against a join from outside any row");
    assert_eq!(reset.runtime, "capsule", "unrelated metadata must survive the reset");
    assert!(
        reg.is_inert_default_anchor(&reset),
        "with agent reset to none, the default row must be inert again"
    );
    assert!(
        Arc::ptr_eq(&reset, &row),
        "reset must mutate the EXISTING row's Arc in place, never replace it"
    );
    // The registry's own row (not just the returned handle) reflects
    // the reset -- resolve() must see it too.
    let resolved = reg.resolve(Some(&original_id)).unwrap();
    assert!(reg.is_inert_default_anchor(&resolved));
    assert!(Arc::ptr_eq(&resolved, &row), "resolve() must return the SAME Arc too");
}

/// 2026-09-04 amendment: a genuinely first-ever launch seeds the
/// default/home row as an INERT ANCHOR — no agent, no autostart — on
/// EVERY host, not just non-Windows. Runs (and must pass) on every
/// platform: unlike the corrupted-row case below, this arm no longer
/// branches on `cfg!(windows)` at all.
#[test]
fn default_row_launch_seed_first_launch_is_inert_everywhere() {
    assert_eq!(
        default_row_launch_seed(None),
        (false, "none".to_string(), String::new(), String::new())
    );
}

/// An existing default row whose on-disk `runtime` is `"capsule"` —
/// healthy on every platform this function runs on (Windows requires
/// it; every other host merely permits it) — survives verbatim: its
/// launch fields are never silently clobbered back to the inert
/// defaults just because it was re-registered at boot.
#[test]
fn default_row_launch_seed_preserves_a_healthy_existing_row() {
    assert_eq!(
        default_row_launch_seed(Some((true, "claude", "kal-sot", "hello"))),
        (
            true,
            "claude".to_string(),
            "kal-sot".to_string(),
            "hello".to_string()
        )
    );
}
