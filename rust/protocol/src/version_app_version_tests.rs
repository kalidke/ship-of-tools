use super::{format_app_version, is_release};

// Argument order throughout: (pkg, sha, on_tag, dirty, ci).

#[test]
fn dirty_build_differs_from_clean_at_the_same_sha() {
    let clean = format_app_version("0.6.0", "abc1234", false, false, false);
    let dirty = format_app_version("0.6.0", "abc1234", false, true, false);
    assert_eq!(clean, "0.6.0+abc1234");
    assert_eq!(dirty, "0.6.0+abc1234-dirty");
    assert_ne!(clean, dirty, "a dirty build must never alias a clean one's version string");
}

#[test]
fn dirty_on_tag_still_shows_dirty_not_the_bare_tag() {
    // A tree that sits ON the release tag but carries local edits is not
    // the release it claims to be — must not collapse to the bare
    // `X.Y.Z` a clean on-tag build reports. True even in CI: a dirty
    // release build is not a release.
    assert_eq!(
        format_app_version("0.6.0", "abc1234", true, true, false),
        "0.6.0+abc1234-dirty"
    );
    assert_eq!(
        format_app_version("0.6.0", "abc1234", true, true, true),
        "0.6.0+abc1234-dirty"
    );
}

#[test]
fn clean_on_tag_is_the_bare_version_only_from_ci() {
    // THE regression this whole predicate exists for. Same source, same
    // sha, same clean tree, on the same tag — the only difference is who
    // built it, and that difference must be visible.
    let ci = format_app_version("0.6.0", "abc1234", true, false, true);
    let local = format_app_version("0.6.0", "abc1234", true, false, false);
    assert_eq!(ci, "0.6.0");
    assert_eq!(local, "0.6.0+src");
    assert_ne!(
        ci, local,
        "a local build of a tagged tree must never alias the release artifact"
    );
}

#[test]
fn no_git_is_marked_src() {
    // Source tarball: no sha, so nothing about the tree can be proven,
    // and it does not get to borrow the bare version. `build.rs` cannot
    // find a tag without git either, so on_tag is false in every
    // reachable no-git build (rust/protocol/build.rs) — including under
    // CI, which is why there is no bare-version case here to test.
    assert_eq!(format_app_version("0.6.0", "", false, false, false), "0.6.0+src");
    assert_eq!(format_app_version("0.6.0", "", false, true, false), "0.6.0+src");
    assert_eq!(format_app_version("0.6.0", "", false, false, true), "0.6.0+src");
}

#[test]
fn the_bare_version_and_the_release_predicate_never_disagree() {
    // The string is documentation; `is_release` is policy. They are
    // derived from one condition precisely so a future edit cannot let
    // a build print the bare version while the updater refuses it, or
    // the reverse — which is the failure mode that produced this fix.
    for &on_tag in &[true, false] {
        for &dirty in &[true, false] {
            for &ci in &[true, false] {
                let v = format_app_version("0.6.0", "abc1234", on_tag, dirty, ci);
                let bare = v == "0.6.0";
                assert_eq!(bare, !v.contains('+'), "a version is the release artifact iff it has no '+'");
                assert_eq!(
                    bare,
                    is_release(on_tag, dirty, ci),
                    "bare-version and is_release disagree at on_tag={on_tag} dirty={dirty} ci={ci}"
                );
            }
        }
    }
}

#[test]
fn a_dev_stamp_builds_as_the_stamp_and_its_commit() {
    // Between tags the line is stamped `X.Y.Z-pre-dev` in Cargo.toml; a build adds only build metadata.
    assert_eq!(
        format_app_version("0.6.6-rc9.14-dev", "abc1234", false, false, false),
        "0.6.6-rc9.14-dev+abc1234"
    );
    assert_eq!(
        format_app_version("0.6.6-rc9.14-dev", "abc1234", false, true, false),
        "0.6.6-rc9.14-dev+abc1234-dirty"
    );
}
