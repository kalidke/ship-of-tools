use super::*;

#[test]
fn the_lists_are_disjoint_and_reasoned() {
    for (name, reason) in INLINE.iter().chain(POOLED) {
        assert!(!reason.is_empty(), "{name} has no reason");
    }
    for (name, _) in INLINE {
        assert!(POOLED.iter().all(|(o, _)| o != name), "{name} is in both lists");
    }
    assert_eq!(place(op::PING), Place::Inline);
    assert_eq!(place(op::WORKSPACE_LIST), Place::Inline);
    assert_eq!(place(op::QUARTO_OPEN), Place::Job);
    assert_eq!(place(op::PREVIEW_GET), Place::Pooled);
    assert_eq!(place("no.such"), Place::Job);
}
