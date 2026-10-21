use super::*;

#[test]
fn held_is_the_handles_of_starting_and_ready_rows() {
    let rows: [(Phase, fn() -> String); 5] = [
        (Phase::Ready, || "a".to_string()),
        (Phase::Starting, || "b".to_string()),
        (Phase::Stopped, || "c".to_string()),
        (Phase::Terminal, || "d".to_string()),
        (Phase::Ready, String::new),
    ];
    assert_eq!(held(rows), ["a".to_string(), "b".to_string()].into_iter().collect());
}

#[test]
fn unended_is_the_handles_of_every_row_not_latched_ended() {
    let rows: [(Phase, fn() -> String); 9] = [
        (Phase::Stopped, || "stopped".to_string()),
        (Phase::Starting, || "starting".to_string()),
        (Phase::Ready, || "ready".to_string()),
        (Phase::Ending, || "ending".to_string()),
        (Phase::Unreachable, || "unreachable".to_string()),
        (Phase::Foreign, || "foreign".to_string()),
        (Phase::Terminal, || "terminal".to_string()),
        (Phase::EndedNoRespawn, || "ended".to_string()),
        (Phase::Stopped, String::new),
    ];
    let want = ["stopped", "starting", "ready", "ending", "unreachable", "foreign"].map(String::from);
    assert_eq!(unended(rows), want.into_iter().collect());
}

#[test]
fn held_resolves_a_handle_only_for_a_running_row() {
    let rows: [(Phase, Box<dyn FnOnce() -> String>); 2] =
        [(Phase::Ready, Box::new(|| "a".to_string())), (Phase::Stopped, Box::new(|| panic!("resolved a stopped row")))];
    assert_eq!(held(rows), ["a".to_string()].into_iter().collect());
}

#[test]
fn a_pass_is_due_for_a_changed_set_or_a_minute() {
    let a: BTreeSet<String> = ["a".to_string()].into_iter().collect();
    let b: BTreeSet<String> = ["b".to_string()].into_iter().collect();
    assert!(due(&a, &b, Duration::from_secs(1)));
    assert!(due(&a, &a, STAMP_EVERY));
    assert!(!due(&a, &a, STAMP_EVERY - Duration::from_secs(1)));
}

use crate::rows::workspace::{Observation, SupervisorIdentity};

/// A row created as `name`, having declared `declared`; Ready when `ready`.
fn row(label: &str, name: &str, declared: &str, ready: bool) -> Arc<Workspace> {
    let w = Workspace::from_label(label, std::env::temp_dir(), false, "claude".into(), name.into(), String::new());
    *w.agent_handle.lock().unwrap() = declared.to_string();
    if ready {
        assert!(w.apply_phase_observation(Observation::Phase {
            phase: Phase::Ready,
            supervisor: SupervisorIdentity { pid: 1, created: 1 },
            voyage: Some(uuid::Uuid::from_u128(1)),
        }));
    }
    Arc::new(w)
}

/// A row's handle is the one its session declared, else the name it was created with, unless another row declares
/// that name; a row with neither has none.
#[test]
fn a_rows_handle_is_its_declared_one_else_its_name_unless_declared_elsewhere() {
    let declared = row("a", "created-as", "joined-as", false);
    assert_eq!(bound_handle(&declared, &[declared.clone()]), "joined-as");
    let named = row("b", "shared", "", false);
    assert_eq!(bound_handle(&named, &[named.clone()]), "shared");
    let claimer = row("c", "", "shared", false);
    assert_eq!(bound_handle(&named, &[named.clone(), claimer.clone()]), "", "another row declares the name");
    assert_eq!(bound_handle(&claimer, &[named.clone(), claimer.clone()]), "shared");
    let bare = row("d", "", "", false);
    assert_eq!(bound_handle(&bare, &[bare.clone()]), "");
}

/// ADR 0049: a running row whose stored name another row declares does not hold it; with no declaring row it does.
#[test]
fn held_handles_skips_a_handle_another_row_declares() {
    let older = row("m5-older", "m5-shared", "", true);
    let newer = row("m5-newer", "", "m5-shared", false);
    assert!(!held_handles(&[older.clone(), newer]).contains("m5-shared"));
    assert!(held_handles(&[older.clone()]).contains("m5-shared"), "no declaring row: the stored name binds");
    let newer_ready = row("m5-newer-ready", "", "m5-shared", true);
    assert!(held_handles(&[older, newer_ready]).contains("m5-shared"));
}
