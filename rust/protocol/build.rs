// build.rs — embed git-derived build metadata for the product version
// (ADR 0030 §1). Lives in the protocol crate so both binaries and the
// (future) handshake `app_version` field read the same string.
//
// Emitted rustc-env vars (all default to ""/"0" when git or the repo is
// unavailable, e.g. release-tarball builds):
//   SOT_BUILD_SHA     short commit sha
//   SOT_BUILD_DATE    committer date, YYYY-MM-DD
//   SOT_BUILD_ON_TAG  "1" iff HEAD sits exactly on tag v{CARGO_PKG_VERSION}

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

fn main() {
    let sha = git(&["rev-parse", "--short=9", "HEAD"]);
    let date = git(&["log", "-1", "--format=%cs"]);
    let on_tag = match git(&["describe", "--tags", "--exact-match", "HEAD"]) {
        Some(tag) => {
            tag == format!("v{}", std::env::var("CARGO_PKG_VERSION").unwrap_or_default())
        }
        None => false,
    };

    // Re-stamp when HEAD moves (commit, checkout, tag). Best-effort: absent
    // paths make these directives inert.
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
}
