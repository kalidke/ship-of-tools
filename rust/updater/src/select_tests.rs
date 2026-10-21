use super::*;

/// The exact fixture from the field incident: a stable and two release
/// candidates on the 0.6.0 line.
fn releases() -> Vec<(String, bool)> {
    vec![
        ("v0.5.10".to_string(), false),
        ("v0.6.0-rc.11".to_string(), true),
        ("v0.6.0-rc.13".to_string(), true),
    ]
}

#[test]
fn stable_install_ignores_rcs() {
    // The newest STABLE release (v0.5.10) is exactly what's installed —
    // no update, and the rcs are never even candidates.
    assert_eq!(select_target("0.5.10", &releases()), None);
}

#[test]
fn stable_install_takes_a_newer_stable_ignoring_rcs() {
    let mut rs = releases();
    rs.push(("v0.6.1".to_string(), false));
    // A stable install must move to the newer stable, not to either rc
    // (even though both rcs are numerically newer than 0.5.10 too).
    assert_eq!(select_target("0.5.10", &rs).as_deref(), Some("v0.6.1"));
}

#[test]
fn rc_install_picks_newest_rc() {
    assert_eq!(
        select_target("0.6.0-rc.11", &releases()).as_deref(),
        Some("v0.6.0-rc.13")
    );
}

#[test]
fn rc_install_picks_the_stable_that_supersedes_it() {
    let mut rs = releases();
    rs.push(("v0.6.0".to_string(), false));
    // A final 0.6.0 outranks every 0.6.0-rc.N of the same core version.
    assert_eq!(
        select_target("0.6.0-rc.13", &rs).as_deref(),
        Some("v0.6.0")
    );
}

#[test]
fn no_downgrade() {
    // Only an older rc is on offer — never move backwards.
    let rs = vec![("v0.6.0-rc.11".to_string(), true)];
    assert_eq!(select_target("0.6.0-rc.13", &rs), None);
}

/// The invariant stated by the owner -- an install must never move
/// backwards -- at the form THIS line actually ships. `no_downgrade`
/// above covers the dotted `rc.N` form; the live 0.6.6 line is the
/// undotted `rc9.N` continuation, where `rc9` sorts BELOW `rc9.1`
/// (same first identifier, fewer fields). That is what makes the
/// continuation legal, and it is also the one place an ordering slip
/// would read as an update and hand a box an older build.
#[test]
fn no_downgrade_on_the_undotted_rc_line() {
    let rs = vec![
        ("v0.6.6-rc7".to_string(), true),
        ("v0.6.6-rc9".to_string(), true),
        ("v0.6.6-rc9.1".to_string(), true),
    ];
    assert_eq!(select_target("0.6.6-rc9.2", &rs), None);
    // The SAME list does offer an older install an update, so the
    // `None` above is the guard talking and not an unparsable line.
    assert_eq!(
        select_target("0.6.6-rc9", &rs).as_deref(),
        Some("v0.6.6-rc9.1")
    );
}

#[test]
fn equal_is_none() {
    let rs = vec![("v0.6.0-rc.13".to_string(), true)];
    assert_eq!(select_target("0.6.0-rc.13", &rs), None);
    let rs = vec![("v0.5.10".to_string(), false)];
    assert_eq!(select_target("0.5.10", &rs), None);
}

#[test]
fn malformed_tags_are_skipped() {
    let rs = vec![
        ("not-a-version".to_string(), false),
        ("v0.6.0".to_string(), false),
        ("vgarbage-rc.1".to_string(), true),
    ];
    assert_eq!(select_target("0.5.0", &rs).as_deref(), Some("v0.6.0"));
    // Only malformed entries on offer → no candidate at all.
    let rs = vec![("nope".to_string(), false), ("also-nope".to_string(), true)];
    assert_eq!(select_target("0.5.0", &rs), None);
}

#[test]
fn empty_release_list_is_none() {
    assert_eq!(select_target("0.5.0", &[]), None);
}

#[test]
fn unparsable_installed_defaults_to_stable_channel() {
    // A garbage installed string can't declare a channel — treat it as
    // stable-only rather than opening the door to prereleases.
    let rs = vec![("v0.6.0-rc.1".to_string(), true), ("v0.5.0".to_string(), false)];
    assert_eq!(select_target("garbage", &rs).as_deref(), Some("v0.5.0"));
}
