use super::*;
use std::time::{Duration, Instant};

fn mark(at: Instant) -> ReadMark {
    ReadMark { host: "h".into(), workspace_id: Some("ws".into()), at }
}

#[test]
fn no_mark_is_keep() {
    assert_eq!(read_mark_decision(None, "h", Some("ws"), Instant::now()), ReadMarkAction::Keep);
}

#[test]
fn not_due_yet_is_keep_due_is_fire() {
    let t0 = Instant::now();
    let m = mark(t0 + READ_DWELL);
    assert_eq!(read_mark_decision(Some(&m), "h", Some("ws"), t0 + Duration::from_secs(3)), ReadMarkAction::Keep);
    assert_eq!(read_mark_decision(Some(&m), "h", Some("ws"), t0 + READ_DWELL), ReadMarkAction::Fire);
}

#[test]
fn a_different_view_cancels_even_when_due() {
    let t0 = Instant::now();
    let m = mark(t0);
    assert_eq!(read_mark_decision(Some(&m), "h", Some("other"), t0 + Duration::from_secs(1)), ReadMarkAction::Cancel);
    assert_eq!(read_mark_decision(Some(&m), "elsewhere", Some("ws"), t0 + Duration::from_secs(1)), ReadMarkAction::Cancel);
    assert_eq!(read_mark_decision(Some(&m), "h", None, t0 + Duration::from_secs(1)), ReadMarkAction::Cancel);
}
