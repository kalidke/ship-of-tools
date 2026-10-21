use super::*;
use serde_json::json;

/// The rule: an escalation exists only for a running eval the request names (or names none), and then only while
/// that eval is the first in flight.
#[test]
fn it_signals_only_the_running_eval_the_request_names() {
    let cases = [
        (json!({}), None, false),
        (json!({}), Some(7), true),
        (json!({ "eval_ids": [7] }), Some(7), true),
        (json!({ "eval_ids": [3, 7] }), Some(7), true),
        (json!({ "eval_ids": [99] }), Some(7), false),
        (json!({ "eval_ids": [] }), Some(7), false),
        (json!({ "eval_ids": [7] }), None, false),
    ];
    for (payload, running, expected) in cases {
        assert_eq!(Escalation::for_request(1, &payload, running).is_some(), expected, "{payload} {running:?}");
    }
    let e = Escalation::for_request(1, &json!({}), Some(7)).unwrap();
    assert!(e.targets(Some(&7)) && !e.targets(Some(&8)) && !e.targets(None));
}

/// A forced request for the same eval replaces a plain escalation; nothing else replaces one.
#[test]
fn a_forced_request_for_the_same_eval_replaces_a_plain_escalation() {
    let make = |id, eval, force: bool| Escalation::for_request(id, &json!({ "force": force }), Some(eval));
    let kept = |old, new| Escalation::prefer(old, new).map(|e| e.id);
    assert_eq!(kept(make(1, 7, false), make(2, 7, true)), Some(2));
    assert_eq!(kept(make(1, 7, false), make(2, 7, false)), Some(1));
    assert_eq!(kept(make(1, 7, true), make(2, 7, true)), Some(1));
    assert_eq!(kept(make(1, 7, false), make(2, 8, true)), Some(1));
    assert_eq!(kept(make(1, 7, false), None), Some(1));
    assert_eq!(kept(None, make(2, 7, true)), Some(2));
    assert_eq!(kept(None, None), None);
}

#[test]
fn force_is_read_from_the_request_and_defaults_to_off() {
    assert!(!Escalation::for_request(1, &json!({}), Some(7)).unwrap().force);
    assert!(!Escalation::for_request(1, &json!({ "force": false }), Some(7)).unwrap().force);
    assert!(Escalation::for_request(1, &json!({ "force": true }), Some(7)).unwrap().force);
}

/// A forced request's signals go `BURST_GAP` apart while less than `FORCE_SPAN` has passed since the first, and end
/// at the child's first line after one; a plain request's one signal is its last, and a line before any signal ends
/// none.
#[test]
fn a_forced_burst_ends_at_its_span_or_at_the_childs_first_line() {
    let pending = HashMap::from([(1, oneshot::channel::<anyhow::Result<Value>>().0)]);
    let t0 = Instant::now();
    let mut forced = Escalation::for_request(1, &json!({ "force": true }), Some(7)).unwrap();
    let mut signals = 0;
    while !forced.over {
        forced.signalled(t0 + BURST_GAP * signals);
        signals += 1;
    }
    let (expected, what) = if cfg!(any(target_os = "macos", windows)) {
        (8, "signals at 0, 50, ..., 350 ms")
    } else {
        (16, "signals at 0, 50, ..., 750 ms")
    };
    assert_eq!(signals, expected, "{what}");
    assert_eq!(forced.at, t0 + BURST_GAP * (expected - 1) + QUIET, "the quiet runs from the last");
    let mut plain = Escalation::for_request(1, &json!({}), Some(7)).unwrap();
    plain.signalled(t0);
    assert!(plain.over && plain.at == t0 + QUIET);
    let mut heard = Escalation::for_request(1, &json!({ "force": true }), Some(7)).unwrap().heard(&pending).unwrap();
    assert!(!heard.over, "a line before any signal ends none");
    heard.signalled(t0);
    assert!(!heard.over);
    assert!(heard.heard(&pending).unwrap().over, "a line after a signal ends the signals");
}

/// Where Julia counts each signal on a thread of its own (macOS, Windows), any signal of the burst can be the one
/// it forces at, so the burst ends 0.1 s or more inside the half second Julia ignores signals after a force: no
/// signal of it is a new interrupt that lands in the shim's answer.
#[test]
fn where_julia_counts_every_signal_the_burst_ends_inside_its_half_second() {
    if cfg!(any(target_os = "macos", windows)) {
        assert!(FORCE_SPAN + Duration::from_millis(100) <= Duration::from_millis(500), "{FORCE_SPAN:?}");
    }
    assert!(FORCE_SPAN >= BURST_GAP * 5, "five signals at least, the count Julia forces at: {FORCE_SPAN:?}");
}

#[test]
fn each_answer_says_what_is_left() {
    let plain = unanswered(false);
    assert!(plain.contains("second Ctrl+C") && plain.contains("--force") && plain.contains("crash"), "{plain}");
    assert!(plain.contains("--fresh"));
    let forced = unanswered(true);
    assert!(forced.contains("forced") && forced.contains("--fresh") && !forced.contains("second Ctrl+C"), "{forced}");
    assert!(forced.contains("heavy CPU load"), "{forced}");
    let none = not_sent("Access is denied");
    assert!(none.contains("no interrupt signal could be sent") && none.contains("Access is denied"), "{none}");
    assert!(none.contains("--fresh"));
}
