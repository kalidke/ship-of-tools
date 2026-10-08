//! Behavior checks for pane completion eligibility, request origins and one-shot receipt ownership.

use super::*;

const READY: PaneFacts = PaneFacts { checkpointed: true, attached: true, live: true };
const AREA: (u16, u16) = (80, 24);

fn after(origin: Instant, ms: u64) -> Instant {
    origin + Duration::from_millis(ms)
}

#[test]
fn uncheckpointed_client_does_not_complete_presentation() {
    let t0 = Instant::now();
    let mut p = PanePresentation::default();
    p.begin(Some(t0));
    for facts in [
        PaneFacts { checkpointed: false, ..READY },
        PaneFacts { attached: false, ..READY },
        PaneFacts { live: false, ..READY },
    ] {
        assert!(p.candidate(facts, AREA).is_none(), "presentation completed before checkpoint");
    }
    let c = p.candidate(READY, AREA).expect("a checkpointed attached live client qualifies");
    assert!(matches!(p.complete(c, after(t0, 5)), Presentation::Receipt { .. }));
}

#[test]
fn a_hidden_pane_does_not_complete_presentation() {
    let mut p = PanePresentation::default();
    p.begin(Some(Instant::now()));
    for area in [(0, 24), (80, 0), (0, 0)] {
        assert!(p.candidate(READY, area).is_none(), "presentation completed for an empty pane");
    }
    assert!(p.candidate(READY, AREA).is_some());
}

#[test]
fn missing_origin_is_not_zero_latency_success() {
    let mut p = PanePresentation::default();
    p.begin(None);
    let c = p.candidate(READY, AREA).expect("candidate");
    assert_eq!(p.complete(c, Instant::now()), Presentation::NoOrigin, "missing origin reported as successful zero");
}

#[test]
fn a_candidate_is_not_a_completion() {
    let t0 = Instant::now();
    let mut p = PanePresentation::default();
    p.begin(Some(t0));
    let first = p.candidate(READY, AREA).expect("candidate");
    let again = p.candidate(READY, AREA).expect("a dropped candidate leaves the request open");
    drop(first);
    assert!(matches!(p.complete(again, after(t0, 5)), Presentation::Receipt { .. }));
}

#[test]
fn the_receipt_is_one_shot_and_belongs_to_its_request() {
    let t0 = Instant::now();
    let mut p = PanePresentation::default();
    p.begin(Some(t0));
    let old = p.candidate(READY, AREA).expect("candidate");
    p.begin(Some(after(t0, 100)));
    assert_eq!(p.complete(old, after(t0, 200)), Presentation::Stale, "a stale generation completes nothing");
    let c = p.candidate(READY, AREA).expect("candidate");
    assert!(matches!(p.complete(c, after(t0, 300)), Presentation::Receipt { .. }));
    assert!(p.candidate(READY, AREA).is_none(), "a consumed request offers no second candidate");
}

#[test]
fn elapsed_runs_from_the_request_origin_not_the_client() {
    let t0 = Instant::now();
    let mut p = PanePresentation::default();
    p.begin(Some(t0));
    let c = p.candidate(READY, AREA).expect("candidate");
    match p.complete(c, after(t0, 1500)) {
        Presentation::Receipt { elapsed, .. } => assert_eq!(elapsed, Duration::from_millis(1500)),
        other => panic!("expected a receipt, got {other:?}"),
    }
}
