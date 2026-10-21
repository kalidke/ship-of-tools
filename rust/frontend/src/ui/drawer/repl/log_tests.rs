use super::*;

/// The first Ctrl+C on an eval is ordinary and a second while the same evals run forces; other evals start over.
#[test]
fn a_second_ctrl_c_on_the_same_evals_forces() {
    assert!(!repl_interrupt_forced(&[], &[3]));
    assert!(!repl_interrupt_forced(&[3], &[4]));
    assert!(!repl_interrupt_forced(&[3], &[]));
    assert!(repl_interrupt_forced(&[3], &[3]));
    assert!(repl_interrupt_forced(&[3, 1 << 40], &[3, 1 << 40]));
}

/// A row switch never restarts the window's eval count, a first visit included: with a count per row, two rows'
/// first evals on one host would both be 1, and the daemon would write one run's output into the other's entry.
#[test]
fn a_row_switch_never_restarts_the_windows_eval_count() {
    let mut rows = ReplRows::default();
    let (x, y): (WsKey, WsKey) = (("host".into(), "x".into()), ("host".into(), "y".into()));
    assert_eq!(rows.next_eval_id(), 1, "row x's first eval");
    rows.park(x.clone(), WorkspaceReplSnapshot::default());
    assert!(rows.unpark(&y).is_none(), "row y: a first visit");
    assert_eq!(rows.next_eval_id(), 2, "row y's first eval");
    rows.park(y, WorkspaceReplSnapshot::default());
    assert!(rows.unpark(&x).is_some(), "row x again");
    assert_eq!(rows.next_eval_id(), 3, "row x's second eval");
}

#[test]
fn a_ctrl_c_names_every_eval_in_flight_and_no_other() {
    let mut session_run = entry(1 << 40, "run x.jl", true);
    session_run.origin = Some("session".to_string());
    let log = vec![
        entry(1, "a", false),
        entry(2, "b", true),
        session_run,
        entry(3, "c", false),
    ];
    assert_eq!(in_flight_eval_ids(&log), vec![2, 1 << 40]);
    assert!(in_flight_eval_ids(&[entry(4, "d", false)]).is_empty());
}

fn entry(eval_id: u64, code: &str, in_flight: bool) -> ReplEntry {
    ReplEntry {
        eval_id,
        code: code.to_string(),
        frames: Vec::new(),
        elapsed_ms: 0,
        in_flight,
        pkg_mode: false,
        origin: None,
    }
}

#[test]
fn history_step_back_returns_none_on_empty_log() {
    let log: Vec<ReplEntry> = Vec::new();
    let mut pos = None;
    let mut saved = None;
    assert_eq!(history_step_back(&log, &mut pos, &mut saved, "draft"), None);
    assert!(pos.is_none());
    assert!(saved.is_none());
}

#[test]
fn history_step_back_skips_in_flight_entries() {
    let log = vec![entry(1, "x = 1", false), entry(2, "x = 2", true)];
    let mut pos = None;
    let mut saved = None;
    // Only the completed entry is a candidate, so back lands on it.
    assert_eq!(
        history_step_back(&log, &mut pos, &mut saved, "draft").as_deref(),
        Some("x = 1")
    );
    // Saved on entry to the walk.
    assert_eq!(saved.as_deref(), Some("draft"));
    // No older entry; second back is a no-op.
    assert_eq!(history_step_back(&log, &mut pos, &mut saved, "x = 1"), None);
}

#[test]
fn history_step_back_walks_oldest_to_newest_in_reverse() {
    let log = vec![
        entry(1, "first", false),
        entry(2, "second", false),
        entry(3, "third", false),
    ];
    let mut pos = None;
    let mut saved = None;
    assert_eq!(
        history_step_back(&log, &mut pos, &mut saved, "").as_deref(),
        Some("third")
    );
    assert_eq!(
        history_step_back(&log, &mut pos, &mut saved, "third").as_deref(),
        Some("second")
    );
    assert_eq!(
        history_step_back(&log, &mut pos, &mut saved, "second").as_deref(),
        Some("first")
    );
    // At the oldest; further back returns None and pos stays put.
    assert_eq!(history_step_back(&log, &mut pos, &mut saved, "first"), None);
    assert_eq!(pos, Some(0));
}

#[test]
fn history_step_forward_no_op_when_not_walking() {
    let log = vec![entry(1, "a", false)];
    let mut pos = None;
    let mut saved = None;
    assert_eq!(history_step_forward(&log, &mut pos, &mut saved), None);
}

#[test]
fn history_step_forward_past_newest_restores_saved_buffer() {
    let log = vec![entry(1, "first", false), entry(2, "second", false)];
    let mut pos = None;
    let mut saved = None;
    // Walk back twice; saved captures the original draft.
    let _ = history_step_back(&log, &mut pos, &mut saved, "in-progress");
    let _ = history_step_back(&log, &mut pos, &mut saved, "second");
    assert_eq!(pos, Some(0));
    // Forward once: back to "second".
    assert_eq!(
        history_step_forward(&log, &mut pos, &mut saved).as_deref(),
        Some("second")
    );
    // Forward again: past newest, restore "in-progress", exit walk.
    assert_eq!(
        history_step_forward(&log, &mut pos, &mut saved).as_deref(),
        Some("in-progress")
    );
    assert!(pos.is_none());
    assert!(saved.is_none());
}

#[test]
fn history_step_forward_with_no_saved_buffer_yields_empty_string() {
    // Shouldn't normally happen (saved is set on first back), but
    // verify the unwrap_or_default fallback doesn't panic.
    let log = vec![entry(1, "x", false)];
    let mut pos = Some(0);
    let mut saved: Option<String> = None;
    assert_eq!(
        history_step_forward(&log, &mut pos, &mut saved).as_deref(),
        Some("")
    );
    assert!(pos.is_none());
}
