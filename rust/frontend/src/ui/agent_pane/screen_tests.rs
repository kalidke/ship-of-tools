use super::*;

// The `attach_direct` switch itself (parsing the daemon's refusal
// payload) is tested where it lives — the tests of
// `net/transport/ops/workspace.rs` (`is_attach_direct_declines_every_other_response_shape`,
// plus a real-seam test driven through `handle_response_frame`) —
// rather than a reimplementation here.

/// LU6a: `pane_screen_choice`'s branches, checked here so this
/// decision runs on every platform (its call site is unconditional
/// too, ADR 0045 decision 1). The
/// `is_dead` cases are below.
#[test]
fn pane_screen_choice_covers_every_branch() {
    // A checkpointed client always wins, hold or no hold, pending or
    // not, dead or not — this is the case that used to paint an empty
    // parser. `is_dead` never overrides a client that DID checkpoint
    // (a client that showed real content and only later died keeps
    // showing it).
    assert_eq!(
        pane_screen_choice(true, true, false, PaneFeed::Capsule, false),
        PaneScreen::Client
    );
    assert_eq!(
        pane_screen_choice(true, true, false, PaneFeed::Capsule, true),
        PaneScreen::Client
    );
    assert_eq!(
        pane_screen_choice(true, true, true, PaneFeed::Capsule, false),
        PaneScreen::Client
    );

    // A live but not-yet-checkpointed client defers to a hold when
    // one exists...
    assert_eq!(
        pane_screen_choice(true, false, false, PaneFeed::Capsule, true),
        PaneScreen::Hold
    );
    // ...and otherwise paints its own (empty) screen — the first-ever
    // attach case, where there is nothing to hold.
    assert_eq!(
        pane_screen_choice(true, false, false, PaneFeed::Capsule, false),
        PaneScreen::Client
    );

    // A client that goes TERMINAL before it
    // ever checkpointed is a dead end, not a stall — it must not keep
    // showing a hold (the departed row's screen) OR its own (blank)
    // screen. Both fall all the way through to `Empty`.
    assert_eq!(
        pane_screen_choice(true, false, true, PaneFeed::Capsule, true),
        PaneScreen::Empty
    );
    assert_eq!(
        pane_screen_choice(true, false, true, PaneFeed::Capsule, false),
        PaneScreen::Empty
    );

    // `Pending` (no client yet, backend unknown) also defers to a
    // hold when one exists...
    assert_eq!(
        pane_screen_choice(false, false, false, PaneFeed::Pending, true),
        PaneScreen::Hold
    );
    // ...and otherwise the pane is blank — every row is a capsule on
    // this build, so there is no other feed left to fall back to.
    assert_eq!(
        pane_screen_choice(false, false, false, PaneFeed::Pending, false),
        PaneScreen::Empty
    );
}

#[test]
fn pane_shows_terminal_reason_only_for_the_dead_uncheckpointed_case() {
    // ADR 0030 §8 "Where it is shown", widened by ADR 0045 decision 1
    // (Codex review, BLOCKER): `is_attached` alone decides it now —
    // deliberately NOT parameterized by "checkpointed" any more, so a
    // client that already checkpointed (its own frozen content is
    // what `pane_screen_choice` separately paints) but is CURRENTLY
    // failing or retrying (typed `Unreachable`, a refusal, a post-
    // checkpoint failure — anything that isn't honestly "attached")
    // shows a reason exactly the same as the original dead-
    // uncheckpointed case did. The old three-argument version hid
    // this; this one-boolean signature cannot.
    assert!(pane_shows_terminal_reason(true, false));
    // A live, honestly-attached client — nothing to explain.
    assert!(!pane_shows_terminal_reason(true, true));
    // No client at all, either way.
    assert!(!pane_shows_terminal_reason(false, false));
    assert!(!pane_shows_terminal_reason(false, true));
}

#[test]
fn pane_terminal_reason_text_reads_only_the_clients_own_status_never_self_status() {
    // Codex review, should-fix: `pane_terminal_reason_text` takes the
    // client's OWN `status_line()` as a plain `Option<&str>` argument
    // and nothing resembling a shared, event-loop-wide status field —
    // there is no `self.status` parameter for a later, unrelated
    // write to reach through. Proven by construction (the signature),
    // demonstrated here: two calls with the SAME `client_status` give
    // the SAME text regardless of anything else that could have
    // happened in between.
    let text = pane_terminal_reason_text(true, Some("checkpoint restore failed: some detail"));
    assert_eq!(text.as_deref(), Some("checkpoint restore failed: some detail"));

    // A DIFFERENT string arriving elsewhere in the caller's own state
    // (what `self.status` becoming "already attached …" would be)
    // cannot change the ALREADY-COMPUTED reason above, and a fresh
    // call with the client's status UNCHANGED reproduces it exactly.
    let unrelated_later_status = "already attached — nothing to do";
    assert_ne!(unrelated_later_status, "checkpoint restore failed: some detail");
    let text_again = pane_terminal_reason_text(true, Some("checkpoint restore failed: some detail"));
    assert_eq!(text_again, text);
}

#[test]
fn pane_terminal_reason_text_is_none_when_no_reason_is_shown() {
    assert_eq!(pane_terminal_reason_text(false, Some("attached")), None);
}

#[test]
fn pane_reason_line_leads_with_a_protocol_refusal() {
    let s = |x: &str| Some(x.to_string());
    assert_eq!(
        pane_reason_line(Some("frontend out of date"), s("connecting…"), Some("dial failed")),
        s("frontend out of date")
    );
    assert_eq!(
        pane_reason_line(None, s("connecting…"), Some("dial failed")),
        s("connecting…")
    );
    assert_eq!(pane_reason_line(None, None, Some("dial failed")), s("dial failed"));
    assert_eq!(pane_reason_line(None, None, None), None);
}
