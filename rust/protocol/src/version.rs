//! The product version string and the one release predicate behind every self-update decision (`is_release_build`).

use super::*;

/// Product version embedded at build time (ADR 0030 §1, §8 decisions 31a
/// and 31c). Exactly one form means "this is an official release build":
/// the bare `X.Y.Z`, emitted only when the build was produced by the release
/// workflow, sits exactly on its release tag `vX.Y.Z`, and had a clean tree.
/// Everything else is that stamp plus build metadata: `X.Y.Z+src` for a
/// build off a tag that CI did not produce (or one built with no git at all),
/// `X.Y.Z+<sha>` for an ordinary clean build, `X.Y.Z+<sha>-dirty` when the
/// tree was dirty. The stamp itself is the line's: between tags it is the
/// next version with `-dev` (`0.6.6-rc9.14-dev+<sha>`, ADR 0030's 2026-10-16
/// amendment). A version is the release artifact if and only if it has no `+`.
///
/// **Do not test this string to decide policy** — call [`is_release_build`],
/// which answers the same question from the flags directly. The string is
/// for humans and for the version stamp; a substring test on it is how the
/// `contains("-dev")` guards used to let a clean on-tag source build through
/// as if it were a release.
///
/// `+src` is semver BUILD METADATA, which is ignored for ordering (see
/// `sot_updater::semver`), so a marked build compares equal to the release
/// it was built from and can never present itself as newer or older.
///
/// The `-dirty` suffix is a SNAPSHOT taken when `build.rs` last ran, not a
/// live-tracked flag (see that file's own doc on `SOT_BUILD_DIRTY`) — an
/// edit made after the last build can leave it stale until something else
/// forces a rebuild. It is a hint for a dev build, never a promise; the
/// actual provenance GUARANTEE this string leans on is a clean, exactly-
/// on-tag release build, which never carries this suffix at all (a tree
/// on-tag but ALSO dirty is not that release, so it still falls through to
/// the `-dirty` form rather than collapsing to the bare tag version). Two
/// distinct dirty trees at one HEAD still alias on this sha — deferred,
/// same as `rust/log/build.rs` already documents for its own build id.
pub fn app_version() -> String {
    format_app_version(
        env!("CARGO_PKG_VERSION"),
        env!("SOT_BUILD_SHA"),
        env!("SOT_BUILD_ON_TAG") == "1",
        env!("SOT_BUILD_DIRTY") == "1",
        env!("SOT_BUILD_ORIGIN") == "ci",
    )
}

/// Whether this binary is an official release artifact — the single
/// authority for every "may this install self-update?" decision (ADR 0030
/// §8 decision 31c).
///
/// Policy lives here rather than in a test on the version string. The two
/// agree today — the bare version is emitted on exactly this condition, and
/// a unit test holds them to it — but that equivalence is a property of the
/// current string shape, not something a caller should have to know. The
/// previous guards did have to know it, spelled `contains("-dev")`, and a
/// clean checkout parked on a release tag defeated them: it builds the same
/// source as the release, so it printed the same bare `X.Y.Z`, passed as a
/// release install, and took updates the launcher's dev-pair-first rule
/// then discarded. The box "updated" and silently stayed put.
///
/// Fails closed: anything it cannot prove is not a release.
pub fn is_release_build() -> bool {
    is_release(
        env!("SOT_BUILD_ON_TAG") == "1",
        env!("SOT_BUILD_DIRTY") == "1",
        env!("SOT_BUILD_ORIGIN") == "ci",
    )
}

/// The shared predicate behind [`is_release_build`] and the bare-version
/// branch of [`format_app_version`], stated once so the string and the
/// policy can never drift apart: the version is bare if and only if this is
/// true.
fn is_release(on_tag: bool, dirty: bool, ci: bool) -> bool {
    ci && on_tag && !dirty
}

/// The pure formatting core of [`app_version`], pulled out so each shape is
/// unit-tested without needing distinct compile-time `env!` values (those
/// are baked in once per build, so `app_version()` itself can only ever
/// exercise ONE branch per test run).
fn format_app_version(pkg: &str, sha: &str, on_tag: bool, dirty: bool, ci: bool) -> String {
    if is_release(on_tag, dirty, ci) {
        return pkg.to_string();
    }
    // No git (a source tarball): nothing can be proven about this tree, and
    // it is not the CI build above, so it is marked like any other
    // unverifiable source build rather than borrowing the bare version. A
    // clean build on the tag but not from CI is the same source as the
    // release, a different artifact: `+src` says exactly that.
    if sha.is_empty() || (on_tag && !dirty) {
        return format!("{pkg}+src");
    }
    if dirty {
        format!("{pkg}+{sha}-dirty")
    } else {
        format!("{pkg}+{sha}")
    }
}

/// One-line `--version` output: `<bin> <version> (<sha> <date>, protocol
/// <PROTOCOL_VERSION>)`, or `<bin> <version> (protocol <PROTOCOL_VERSION>)`
/// when built without git.
///
/// The trailing `protocol <N>` exists so nothing outside this crate ever
/// has to hard-code `PROTOCOL_VERSION` to learn what a built binary speaks
/// — the release smoke test asks `sotd --version` instead of writing the
/// number into its own hello frame, and any other out-of-tree client
/// (install scripts, health checks) can do the same. It is appended inside
/// the existing parenthetical rather than as a new subcommand or field
/// because every parser found in-tree (the release/CI smoke jobs, the
/// installer, `scripts/install-manifest.ps1`, `scripts/launch-sot.ps1`)
/// only reads the second whitespace-delimited token (the version) or
/// checks a `"<bin> <version> ("` prefix — appending text at the end of
/// the parens is invisible to all of them. Keep it that way: don't insert
/// new tokens *before* `protocol`, and don't drop the leading space before
/// the opening paren.
pub fn version_line(bin: &str) -> String {
    let sha = env!("SOT_BUILD_SHA");
    let date = env!("SOT_BUILD_DATE");
    if sha.is_empty() {
        format!("{bin} {} (protocol {PROTOCOL_VERSION})", app_version())
    } else {
        format!("{bin} {} ({sha} {date}, protocol {PROTOCOL_VERSION})", app_version())
    }
}

#[cfg(test)]
#[path = "version_app_version_tests.rs"]
mod app_version_tests;
