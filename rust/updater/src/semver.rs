//! Semver parsing and ordering for release tags — moved verbatim from the
//! backend's `update.rs` (ADR 0030 §4). Deliberately hand-rolled: the updater
//! must never report a garbage "latest" as an available update, so an
//! unparsable side always ranks below a parsable one.

use std::cmp::Ordering;

/// Strip a single leading `v` from a tag (`v0.2.0` → `0.2.0`).
pub fn strip_v(s: &str) -> &str {
    s.strip_prefix('v').unwrap_or(s)
}

#[derive(Debug, PartialEq, Eq)]
pub struct SemVer {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Pre-release identifiers (empty = a release, which outranks any
    /// pre-release of the same core version).
    pub pre: Vec<String>,
}

/// Parse `X.Y.Z` with an optional `-prerelease` and optional `+build`
/// (build metadata is ignored). A leading `v` and a `-dev+<sha>` suffix both
/// parse — `0.2.0-dev+abc` → core 0.2.0, pre `["dev"]`. `None` on anything
/// that isn't three numeric core components.
pub fn parse_semver(s: &str) -> Option<SemVer> {
    let s = strip_v(s.trim());
    // Drop build metadata first so a `-dev+sha` doesn't fold the sha into pre.
    let s = s.split('+').next().unwrap_or(s);
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, p.split('.').map(str::to_string).collect()),
        None => (s, Vec::new()),
    };
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None; // more than three core components
    }
    Some(SemVer {
        major,
        minor,
        patch,
        pre,
    })
}

/// Compare two pre-release identifier lists per semver §11: a release (empty)
/// outranks any pre-release; otherwise compare identifiers left-to-right
/// (numeric < alphanumeric; numeric compared as ints; more identifiers wins a
/// common prefix).
fn cmp_pre(a: &[String], b: &[String]) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater, // a is a release, b a pre-release
        (false, true) => Ordering::Less,
        (false, false) => {
            for (x, y) in a.iter().zip(b.iter()) {
                let o = cmp_ident(x, y);
                if o != Ordering::Equal {
                    return o;
                }
            }
            a.len().cmp(&b.len())
        }
    }
}

fn cmp_ident(x: &str, y: &str) -> Ordering {
    match (x.parse::<u64>(), y.parse::<u64>()) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        (Ok(_), Err(_)) => Ordering::Less, // numeric identifiers rank lower
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => x.cmp(y),
    }
}

/// Order two version strings. An unparsable side ranks below a parsable one
/// (and two unparsable are Equal) so a garbage "latest" can never be reported
/// as an available update.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    match (parse_semver(a), parse_semver(b)) {
        (Some(x), Some(y)) => x
            .major
            .cmp(&y.major)
            .then(x.minor.cmp(&y.minor))
            .then(x.patch.cmp(&y.patch))
            .then_with(|| cmp_pre(&x.pre, &y.pre)),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_core_ordering() {
        assert_eq!(compare_versions("0.2.0", "0.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.2.1", "0.2.0"), Ordering::Greater);
        assert_eq!(compare_versions("0.2.0", "0.2.1"), Ordering::Less);
        assert_eq!(compare_versions("0.3.0", "0.2.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.10.0", "0.9.0"), Ordering::Greater); // numeric, not lexical
    }

    #[test]
    fn semver_v_prefix_and_whitespace() {
        assert_eq!(compare_versions("v0.3.0", "0.2.0"), Ordering::Greater);
        assert_eq!(compare_versions(" v0.2.0 ", "0.2.0"), Ordering::Equal);
    }

    #[test]
    fn semver_prerelease_ordering() {
        // A pre-release is lower than its release.
        assert_eq!(compare_versions("1.0.0-alpha", "1.0.0"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0", "1.0.0-rc.1"), Ordering::Greater);
        // Identifier comparison, left to right.
        assert_eq!(compare_versions("1.0.0-alpha", "1.0.0-beta"), Ordering::Less);
        assert_eq!(
            compare_versions("1.0.0-alpha.1", "1.0.0-alpha"),
            Ordering::Greater // more identifiers on a common prefix
        );
        // Numeric identifiers rank below alphanumeric ones.
        assert_eq!(compare_versions("1.0.0-1", "1.0.0-alpha"), Ordering::Less);
        // Numeric identifiers compare as integers, not strings.
        assert_eq!(
            compare_versions("1.0.0-alpha.2", "1.0.0-alpha.10"),
            Ordering::Less
        );
    }

    #[test]
    fn semver_dev_marker_is_stripped_to_base() {
        // A `-dev+<sha>` running build compares by its base X.Y.Z against a
        // real release: 0.3.0 > 0.2.0-dev, and 0.2.0 release > 0.2.0-dev.
        assert_eq!(
            compare_versions("0.3.0", "0.2.0-dev+abc1234"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("0.2.0", "0.2.0-dev+abc1234"),
            Ordering::Greater
        );
        // Build metadata does not affect ordering.
        assert_eq!(
            compare_versions("0.2.0-dev+aaa", "0.2.0-dev+bbb"),
            Ordering::Equal
        );
    }

    #[test]
    fn semver_unparsable_never_wins() {
        // A garbage "latest" must never read as an available update.
        assert_eq!(compare_versions("garbage", "0.2.0"), Ordering::Less);
        assert_eq!(compare_versions("0.2", "0.2.0"), Ordering::Less); // two components → unparsable
        assert_eq!(compare_versions("0.2.0.1", "0.2.0"), Ordering::Less); // four → unparsable
        assert_eq!(compare_versions("junk", "also-junk"), Ordering::Equal);
    }
}
