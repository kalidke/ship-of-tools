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
#[path = "select_tests.rs"]
mod tests;
