//! Release selection (ADR 0030 §4 amendment 2026-09-24: every install tracks
//! the newest release, rc or not). Stable and prerelease installs alike
//! consider every published release and track the newest by semver, so a
//! stable `0.6.5` install rides onto `0.6.6-rc1` as soon as it is cut, then
//! onto `0.6.6`. Never a downgrade: a candidate only counts when it compares
//! strictly greater than what's installed.
//!
//! Pure and side-effect-free on purpose: both fetch backends list raw
//! `(tag, prerelease)` pairs from GitHub and hand them here — this is the
//! ONLY place the decision is made, so there is exactly one thing to get
//! right and one thing to unit-test.

use std::cmp::Ordering;

use crate::semver::{compare_versions, parse_semver};

/// Pick the release to move to, or `None` when nothing published is a
/// genuine update.
///
/// - Prereleases are candidates for every install; the `prerelease` flag is
///   carried in the listing but no longer gates selection.
/// - Entries with an unparsable tag are skipped, never guessed at.
/// - The winner is the highest-semver candidate, and it is returned only
///   when it is strictly greater than `installed` — an exact match or
///   anything older is "nothing to do", not a target.
pub fn select_target(installed: &str, releases: &[(String, bool)]) -> Option<String> {
    let best = releases
        .iter()
        .filter(|(tag, _)| parse_semver(tag).is_some())
        .max_by(|(a, _), (b, _)| compare_versions(a, b))?;
    (compare_versions(&best.0, installed) == Ordering::Greater).then(|| best.0.clone())
}

#[cfg(test)]
mod tests {
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
    fn stable_install_takes_the_newest_rc() {
        // A stable install on the newest stable moves onto the rc line.
        assert_eq!(
            select_target("0.5.10", &releases()).as_deref(),
            Some("v0.6.0-rc.13")
        );
    }

    #[test]
    fn stable_install_takes_a_newer_stable_over_older_rcs() {
        let mut rs = releases();
        rs.push(("v0.6.1".to_string(), false));
        // The newest by semver wins, stable or not.
        assert_eq!(select_target("0.5.10", &rs).as_deref(), Some("v0.6.1"));
    }

    #[test]
    fn undotted_rc_tag_is_a_candidate() {
        // The tag shape actually cut since 0.6.5: `vX.Y.Z-rcN`.
        let rs = vec![
            ("v0.6.5".to_string(), false),
            ("v0.6.6-rc1".to_string(), true),
        ];
        assert_eq!(select_target("0.6.4", &rs).as_deref(), Some("v0.6.6-rc1"));
        assert_eq!(select_target("0.6.5", &rs).as_deref(), Some("v0.6.6-rc1"));
        assert_eq!(select_target("0.6.6-rc1", &rs), None);
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
    fn unparsable_installed_takes_the_newest() {
        let rs = vec![("v0.6.0-rc.1".to_string(), true), ("v0.5.0".to_string(), false)];
        assert_eq!(select_target("garbage", &rs).as_deref(), Some("v0.6.0-rc.1"));
    }
}
