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

/// Compare two pre-release identifier lists per semver §11, with the `-dev` rule
/// in `cmp_ident`: a release (empty) outranks any pre-release; otherwise compare
/// identifiers left-to-right (numeric < alphanumeric; numeric compared as ints;
/// more identifiers wins a common prefix).
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

/// A pre-release identifier `<stem>-dev` is the line's stamp between two tags (`14-dev` in `0.6.6-rc9.14-dev`): it
/// compares as its stem, and below it on a tie, so `rc9.13 < rc9.14-dev < rc9.14`.
fn cmp_ident(x: &str, y: &str) -> Ordering {
    let ((xs, xdev), (ys, ydev)) = (dev_stem(x), dev_stem(y));
    cmp_plain(xs, ys).then(ydev.cmp(&xdev))
}

fn dev_stem(s: &str) -> (&str, bool) {
    match s.strip_suffix("-dev") {
        Some(stem) if !stem.is_empty() => (stem, true),
        _ => (s, false),
    }
}

/// True for a version on the line between tags: its last pre-release identifier is `dev` or ends in `-dev`
/// (`0.6.7-dev`, `0.6.6-rc9.14-dev+abc`), so no tag names it.
pub fn is_dev(version: &str) -> bool {
    parse_semver(version).is_some_and(|v| v.pre.last().is_some_and(|id| id == "dev" || id.ends_with("-dev")))
}

fn cmp_plain(x: &str, y: &str) -> Ordering {
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
#[path = "semver_tests.rs"]
mod tests;
