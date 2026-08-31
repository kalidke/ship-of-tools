// build.rs — embed a git-derived build identity for the supervisor
// lane's build boundary (ADR 0041 step 6 U2; Codex review round 1,
// finding 7: `SUPERVISOR_LANE_BUILD_ID` must distinguish distinct
// commits at the SAME `Cargo.toml` version, not just carry
// `CARGO_PKG_VERSION`).
//
// `sot-log` cannot depend on `sot-protocol` (whose own `build.rs` already
// does exactly this for `app_version`/`version_line`) without a real
// crate-layering violation: `sot-log` is the low-level voyage-store crate
// (ADR 0037/0039/0041), architecturally beneath the frontend/backend wire
// protocol crate, and `sot-protocol` pulls in `tokio`/`anyhow` this crate
// has no other reason to need. This is the SAME git-sha-capture pattern
// (`rust/protocol/build.rs`), copied rather than shared, so each crate's
// own build stays self-contained.
//
// Emitted rustc-env var (defaults to "" when git or the repo is
// unavailable, e.g. a release-tarball build — `exchange.rs` falls back
// to the bare package version in that case, matching `app_version`'s own
// on-tag/no-repo behavior):
//   SOT_LOG_BUILD_SHA   short commit sha

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

    // Re-stamp when HEAD moves (commit, checkout, tag). Best-effort:
    // absent paths make these directives inert.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/refs");
    }

    println!("cargo:rustc-env=SOT_LOG_BUILD_SHA={}", sha.unwrap_or_default());
}
