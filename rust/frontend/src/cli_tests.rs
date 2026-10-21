use super::{parse_demo_session, HELP};

#[test]
fn help_describes_the_start_flags() {
    assert!(HELP.contains("--start-focus <p>"));
    assert!(HELP.contains("maximize the focused pane"));
    assert!(!HELP.contains("--start-monitor <"));
}

#[test]
fn demo_session_parses_optional_state() {
    // Bare slug → no state.
    assert_eq!(parse_demo_session("alpha"), Some(("alpha".into(), None)));
    // slug:state for each recognised work-state.
    assert_eq!(
        parse_demo_session("beta:working"),
        Some(("beta".into(), Some("working".into())))
    );
    assert_eq!(
        parse_demo_session("gamma:blocked"),
        Some(("gamma".into(), Some("blocked".into())))
    );
    // Whitespace around an entry is trimmed.
    assert_eq!(
        parse_demo_session("  delta:done "),
        Some(("delta".into(), Some("done".into())))
    );
    // Unknown suffix is not a state — keep the whole thing as the slug so
    // a stray colon doesn't silently drop text.
    assert_eq!(
        parse_demo_session("eps:bogus"),
        Some(("eps:bogus".into(), None))
    );
    // Empty entry is filtered out.
    assert_eq!(parse_demo_session("   "), None);
}
