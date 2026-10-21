use super::*;

#[test]
fn pane_discard_notice_counts_every_discarded_input() {
    // The frontend's count (Pending-arm inputs, +1 each) plus the
    // attach client's own count; absent at 0, singular at 1.
    assert_eq!(pane_discard_notice(0, 0), None);
    assert_eq!(
        pane_discard_notice(1, 0).as_deref(),
        Some("1 keystroke discarded, not sent")
    );
    assert_eq!(
        pane_discard_notice(3, 0).as_deref(),
        Some("3 keystrokes discarded, not sent")
    );
    assert_eq!(
        pane_discard_notice(2, 2).as_deref(),
        Some("4 keystrokes discarded, not sent")
    );
}

#[test]
fn pane_overlay_keeps_the_count_beside_any_reason() {
    let n = || Some("2 keystrokes discarded, not sent".to_string());
    assert_eq!(
        pane_overlay_lines(Some("dial failed".to_string()), n()),
        vec!["dial failed".to_string(), n().unwrap()]
    );
    assert_eq!(pane_overlay_lines(None, n()), vec![n().unwrap()]);
    assert_eq!(
        pane_overlay_lines(Some("dial failed".to_string()), None),
        vec!["dial failed".to_string()]
    );
    assert!(pane_overlay_lines(None, None).is_empty());
}
