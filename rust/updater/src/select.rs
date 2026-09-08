//! Channel selection (ADR 0030 §4 amendment 2026-09-08: "the channel is
//! implied by the installed version"). A prerelease install (a semver with a
//! pre-release identifier, e.g. `0.6.0-rc.11`) considers prereleases too and
//! tracks the newest release by semver, stable or not; a stable install
//! never sees prereleases — GitHub's own `releases/latest` semantics,
//! unchanged. Never a downgrade: a candidate only counts when it compares
//! strictly greater than what's installed.
//!
//! Pure and side-effect-free on purpose: both fetch backends list raw
//! `(tag, prerelease)` pairs from GitHub and hand them here — this is the
//! ONLY place the decision is made, so there is exactly one thing to get
//! right and one thing to unit-test.

use std::cmp::Ordering;

use crate::semver::{compare_versions, parse_semver};

/// Pick the release to move to, or `None` when nothing in the installed
/// version's channel is a genuine update.
///
/// - The channel is derived from `installed`: any pre-release identifier
///   opens the door to `releases` entries marked `prerelease`; a bare
///   `X.Y.Z` install only ever considers `prerelease == false` entries.
/// - Entries with an unparsable tag are skipped, never guessed at.
/// - The winner is the highest-semver candidate in-channel, and it is
///   returned only when it is strictly greater than `installed` — an exact
///   match or anything older is "nothing to do", not a target.
pub fn select_target(installed: &str, releases: &[(String, bool)]) -> Option<String> {
    let prerelease_channel = parse_semver(installed)
        .map(|v| !v.pre.is_empty())
        .unwrap_or(false);
    let best = releases
        .iter()
        .filter(|(tag, prerelease)| (prerelease_channel || !prerelease) && parse_semver(tag).is_some())
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
}
