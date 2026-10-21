use super::*;

// Switch-latency Phase 1: `reply_is_current` is the whole stale-reply
// guard for the preview pane and the concept/annotation slot — a
// single free function shared by both `IncomingEvt` match arms, so one
// set of cases covers both consumers.

#[test]
fn reply_is_current_accepts_the_latest_generation_for_the_active_owner() {
    assert!(reply_is_current(
        3,
        3,
        &"h".to_string(),
        &"h".to_string(),
        &Some("ws".to_string()),
        &Some("ws".to_string()),
    ));
    // `None` (the daemon-default workspace) matches itself too.
    assert!(reply_is_current(1, 1, &"h".to_string(), &"h".to_string(), &None, &None));
}

#[test]
fn reply_is_current_drops_an_older_generation() {
    // A slower earlier request's reply landing after a newer one has
    // already been fired for the same slot — the core switch-latency
    // repro (an obsolete preview overwriting a newer cursor's target).
    assert!(!reply_is_current(
        1,
        3,
        &"h".to_string(),
        &"h".to_string(),
        &Some("ws".to_string()),
        &Some("ws".to_string()),
    ));
}

#[test]
fn reply_is_current_drops_a_generation_ahead_of_the_latest_issued() {
    // Shouldn't happen (a reply can't answer a request this session
    // never sent), but the check is a strict equality, not `<=`, so a
    // forged/corrupt generation is rejected too rather than silently
    // becoming the new "latest".
    assert!(!reply_is_current(
        5,
        3,
        &"h".to_string(),
        &"h".to_string(),
        &Some("ws".to_string()),
        &Some("ws".to_string()),
    ));
}

#[test]
fn reply_is_current_drops_a_non_active_host_even_at_the_latest_generation() {
    // `workspace_id: None` names "the default workspace" on EVERY
    // host, so the host leg of the owner check has to be independent
    // of the workspace leg — a stale reply from a host the session has
    // since switched away from must not be mistaken for the active one
    // just because both happen to be on their own default workspace.
    assert!(!reply_is_current(
        1,
        1,
        &"old-host".to_string(),
        &"active-host".to_string(),
        &None,
        &None,
    ));
}

#[test]
fn reply_is_current_drops_a_non_active_workspace_even_at_the_latest_generation() {
    assert!(!reply_is_current(
        1,
        1,
        &"h".to_string(),
        &"h".to_string(),
        &Some("old-ws".to_string()),
        &Some("active-ws".to_string()),
    ));
}
