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

/// The shapes the SHIPPED release lines actually use, pinned here because a
/// comparator that stops offering a candidate fails INVISIBLY: it degrades to
/// "no update available", which is byte-identical to what a healthy
/// up-to-date box reports. The 0.6.6 line proved every case below on real
/// hardware (rc9 → rc9.1, 2026-09-27); this turns that evidence into a guard,
/// since a field measurement cannot catch a later refactor.
#[test]
fn semver_undotted_rc_line_and_its_continuation() {
    // Undotted `rcN` is ONE alphanumeric identifier, so it compares as text.
    // Within a single digit that reads naturally:
    assert_eq!(compare_versions("0.6.6-rc9", "0.6.6-rc7"), Ordering::Greater);
    // …and this is the trap that caps such a line: `rc10` sorts BELOW `rc7`,
    // so a tenth undotted candidate would never be offered. Pinned as a fact
    // rather than folklore, because the remedy depends on it being true.
    assert_eq!(compare_versions("0.6.6-rc10", "0.6.6-rc7"), Ordering::Less);
    // The escape hatch, and why it works: a common prefix is broken on
    // identifier COUNT, so `rc9` (one identifier) loses to `rc9.1` (two).
    assert_eq!(compare_versions("0.6.6-rc9.1", "0.6.6-rc9"), Ordering::Greater);
    // The second identifier is all digits, so it compares NUMERICALLY — which
    // is what gives the `rc9.N` form no ceiling at all.
    assert_eq!(
        compare_versions("0.6.6-rc9.2", "0.6.6-rc9.1"),
        Ordering::Greater
    );
    assert_eq!(
        compare_versions("0.6.6-rc9.10", "0.6.6-rc9.9"),
        Ordering::Greater
    );
    // Never switch form mid-line: `rc` alone sorts below `rc9`, so a `rc.1`
    // published onto an undotted line is LOWER than every candidate before it
    // and nobody could update to it.
    assert_eq!(compare_versions("0.6.6-rc.1", "0.6.6-rc9"), Ordering::Less);
    // A release still outranks every candidate of either form.
    assert_eq!(compare_versions("0.6.6", "0.6.6-rc9.10"), Ordering::Greater);
}

#[test]
fn src_marked_build_is_ordering_identical_to_its_release() {
    // A clean local build of a release tag stamps `X.Y.Z+src` (ADR 0030
    // §8 decision 31c). That marker is BUILD metadata by choice: it must
    // never read as newer or older than the release it was built from,
    // or the updater would offer such a box an "upgrade" onto its own
    // version — and, since the launcher runs the local build anyway,
    // that upgrade would apply forever without ever taking effect.
    assert_eq!(compare_versions("0.6.0+src", "0.6.0"), Ordering::Equal);
    assert_eq!(
        compare_versions("0.6.0-rc.15+src", "0.6.0-rc.15"),
        Ordering::Equal
    );
    // A real successor is still strictly newer than the marked build.
    assert_eq!(compare_versions("0.6.1", "0.6.0+src"), Ordering::Greater);
    // And the marker must not disturb channel selection (select.rs keys
    // on `pre.is_empty()`): a marked stable stays stable, a marked
    // prerelease stays a prerelease.
    assert!(parse_semver("0.6.0+src").unwrap().pre.is_empty());
    assert!(!parse_semver("0.6.0-rc.15+src").unwrap().pre.is_empty());
}

#[test]
fn semver_unparsable_never_wins() {
    // A garbage "latest" must never read as an available update.
    assert_eq!(compare_versions("garbage", "0.2.0"), Ordering::Less);
    assert_eq!(compare_versions("0.2", "0.2.0"), Ordering::Less); // two components → unparsable
    assert_eq!(compare_versions("0.2.0.1", "0.2.0"), Ordering::Less); // four → unparsable
    assert_eq!(compare_versions("junk", "also-junk"), Ordering::Equal);
}

#[test]
fn semver_dev_line_sits_between_its_tags() {
    // `<stem>-dev` is the line's stamp between two tags: it compares as its
    // stem, and below it on a tie.
    let table = [
        ("0.6.6-rc9.13", "0.6.6-rc9.14-dev+abc"),
        ("0.6.6-rc9.14-dev+abc", "0.6.6-rc9.14"),
        ("0.6.6-rc9.14", "0.6.6-rc9.14.1"),
        ("0.6.6-rc9.14-dev", "0.6.6-rc9.14.1"),
        ("0.6.6-rc9", "0.6.6-rc9.1-dev"),
        ("0.6.6-rc9.1-dev", "0.6.6-rc9.1"),
        ("0.6.7-rc.1", "0.6.7-rc.2-dev"),
        ("0.6.7-rc.2-dev", "0.6.7-rc.2"),
        ("0.6.6", "0.6.7-dev"),
        ("0.6.7-dev", "0.6.7-rc.1"),
        ("0.6.7-rc.1", "0.6.7"),
        ("0.6.6-rc9.15-dev", "0.6.6"),
    ];
    for (lo, hi) in table {
        assert_eq!(compare_versions(lo, hi), Ordering::Less, "{lo} < {hi}");
        assert_eq!(compare_versions(hi, lo), Ordering::Greater, "{hi} > {lo}");
    }
    // What release.sh's preflight refuses: a cut that sorts below the stamp it replaces.
    for (cut, stamp) in [
        ("0.6.7-alpha.1", "0.6.7-dev"),
        ("0.6.7-beta.1", "0.6.7-dev"),
        ("0.6.6-rc9.14.1", "0.6.6-rc9.15-dev"),
    ] {
        assert_eq!(compare_versions(cut, stamp), Ordering::Less, "{cut} < {stamp}");
    }
    assert_eq!(
        compare_versions("0.6.6-rc9.14-dev+a", "0.6.6-rc9.14-dev+b"),
        Ordering::Equal
    );
}

#[test]
fn is_dev_names_the_lines_stamps() {
    for v in ["0.6.7-dev", "0.6.6-rc9.14-dev+abc", "v0.6.6-rc9.14-dev"] {
        assert!(is_dev(v), "{v}");
    }
    for v in ["0.6.6-rc9.14", "0.6.6+src", "0.6.6-devel", "0.6.6", "garbage"] {
        assert!(!is_dev(v), "{v}");
    }
}
