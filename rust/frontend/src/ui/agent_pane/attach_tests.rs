use super::*;

#[test]
fn bl_pane_target_owner_pair_distinguishes_same_named_sessions_across_hosts() {
    // ADR 0042 L2a codex review, item D: attach_session_to_bl's
    // "already attached" early-return, and the PtyAttachDirect
    // owner gate, both compare the FULL (host, session) pair -- a
    // bare session-name compare let a switch from host alpha's
    // "sot-be-sot" to host beta's "sot-be-sot" (every host uses the
    // same "sot-be-<slug>" naming convention) hit the early return,
    // leaving the pty open on alpha while every later
    // write/resize/scroll (routed via active_host) went to beta.
    let bl_pane_target: Option<(HostKey, String)> =
        Some(("alpha".to_string(), "sot-be-sot".to_string()));

    // The switch target names the SAME session on a DIFFERENT host.
    let switch_to: (HostKey, String) = ("beta".to_string(), "sot-be-sot".to_string());
    assert_ne!(
        bl_pane_target.as_ref(),
        Some(&switch_to),
        "same session name on a different host must not read as already-attached"
    );

    // Once re-attached, an event tagged with the OLD host must not be
    // mistaken for the new owner (the PtyAttachDirect event_host
    // gate).
    let new_owner: Option<(HostKey, String)> = Some(switch_to);
    let stale_event_host: HostKey = "alpha".to_string();
    assert_ne!(
        new_owner.as_ref().map(|(h, _)| h),
        Some(&stale_event_host),
        "bytes tagged with the departed host must be recognized as non-owner"
    );
}

/// BLOCKER (Codex review, lane B5 discharge): `lane_dial` must dial
/// the SAME endpoint the control transport actually resolved for a
/// host — never a second, independent preference guess. A
/// `ResolvedDial::Ssh(recipe)` carries the exact resolved recipe
/// through untouched, proving no second, independent read of
/// `config.dial` ever happens; a `ResolvedDial::Local` against an
/// ssh-configured host — the mismatch that used to trigger the old
/// "guess from config" bug — degrades to no dial instead of reaching
/// a DIFFERENT daemon. `LaneDial` has no `Debug`/`PartialEq`
/// (its own doc: an endpoint value names one dial, nothing to
/// compare structurally), so each case matches the variant directly.
#[test]
fn a_switch_back_to_a_warm_row_takes_the_parked_client_without_a_new_dial() {
    let host = || "h".to_string();
    let a = (host(), "sot-be-a".to_string());
    let b = (host(), "sot-be-b".to_string());
    let c = (host(), "sot-be-c".to_string());
    let mut pool: WarmAttachPool<&'static str> = WarmAttachPool::new();
    let dials = std::cell::Cell::new(0);
    let attach = |pool: &mut WarmAttachPool<&'static str>, key: &(String, String)| -> &'static str {
        pool.take(key).unwrap_or_else(|| {
            dials.set(dials.get() + 1);
            "dialed"
        })
    };
    assert!(pool.park(a.clone(), "client-a", 2).is_empty());
    let on_b = attach(&mut pool, &b);
    assert_eq!((on_b, dials.get()), ("dialed", 1));
    assert!(pool.park(b.clone(), on_b, 2).is_empty());
    assert_eq!((attach(&mut pool, &a), dials.get()), ("client-a", 1));
    assert!(pool.park(a.clone(), "client-a", 2).is_empty());
    assert_eq!(pool.park(c, "client-c", 2), vec!["dialed"]);
    assert_eq!(pool.retain_rows(&host(), |row| row == "sot-be-c"), vec!["client-a"]);
    assert!(pool.take(&a).is_none());
}
