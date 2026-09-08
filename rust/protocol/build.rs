// build.rs — embed git-derived build metadata for the product version
// (ADR 0030 §1). Lives in the protocol crate so both binaries and the
// (future) handshake `app_version` field read the same string.
//
// Emitted rustc-env vars (all default to ""/"0" when git or the repo is
// unavailable, e.g. release-tarball builds):
//   SOT_BUILD_SHA     short commit sha
//   SOT_BUILD_DATE    committer date, YYYY-MM-DD
//   SOT_BUILD_ON_TAG  "1" iff HEAD sits exactly on tag v{CARGO_PKG_VERSION}
//   SOT_BUILD_DIRTY   "1" iff the working tree had uncommitted changes AT
//                     THE MOMENT this metadata was generated (ADR 0030 §8
//                     decision 31a) — a SNAPSHOT, not a live-tracked flag:
//                     Cargo only reruns this script on the triggers below
//                     (HEAD/refs moving), so an unstaged edit made after
//                     the last run can leave a stale "0" until something
//                     else forces a rebuild. This is why a dev build's
//                     suffix is a hint, never a promise — the provenance
//                     GUARANTEE is a clean, tagged release build (§1),
//                     which by construction never carries this suffix at
//                     all. `"0"` when git is unavailable (nothing to be
//                     dirty against): `app_version()` never appends a sha
//                     at all in that case, so this value goes unread.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// `true` iff the working tree has uncommitted changes, OR cleanliness
/// itself could not be determined — fails closed, same reasoning as
/// `rust/log/build.rs`'s own `is_dirty` (an unverifiable tree is never
/// silently assumed clean): a version string that CAN'T prove it is the
/// commit it names must not claim to be.
fn is_dirty() -> bool {
    match Command::new("git").args(["status", "--porcelain"]).output() {
        Ok(out) if out.status.success() => !out.stdout.is_empty(),
        _ => true,
    }
}

fn main() {
    let sha = git(&["rev-parse", "--short=9", "HEAD"]);
    let date = git(&["log", "-1", "--format=%cs"]);
    let on_tag = match git(&["describe", "--tags", "--exact-match", "HEAD"]) {
        Some(tag) => {
            tag == format!("v{}", std::env::var("CARGO_PKG_VERSION").unwrap_or_default())
        }
        None => false,
    };
    // No git at all (release tarball) → `sha` is empty and `app_version()`
    // never appends anything, so the dirty check would be meaningless (and
    // `is_dirty()`'s fail-closed default would otherwise wrongly stamp a
    // tarball build "1"). Only probed when there's a sha to qualify.
    let dirty = sha.is_some() && is_dirty();

    // Re-stamp when HEAD moves (commit, checkout, tag). Best-effort: absent
    // paths make these directives inert. Deliberately NOT watching the
    // index: `git add`ing a file changes it, but an ordinary unstaged edit
    // does not, so it would catch some dirty transitions and not others
    // while looking like a live tracker — SOT_BUILD_DIRTY is a snapshot
    // (see its own doc above), and a knob that sometimes rebuilds and
    // sometimes doesn't is worse than one that plainly never does.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/refs");
    }

    println!("cargo:rustc-env=SOT_BUILD_SHA={}", sha.unwrap_or_default());
    println!("cargo:rustc-env=SOT_BUILD_DATE={}", date.unwrap_or_default());
    println!(
        "cargo:rustc-env=SOT_BUILD_ON_TAG={}",
        if on_tag { "1" } else { "0" }
    );
    println!(
        "cargo:rustc-env=SOT_BUILD_DIRTY={}",
        if dirty { "1" } else { "0" }
    );
}
