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
// Codex review round 2, finding M9: an earlier version silently emitted
// an EMPTY sha when git was unavailable, and `exchange.rs` then silently
// reused `CARGO_PKG_VERSION` -- two different commits sharing one
// pre-release `Cargo.toml` version would then be INDISTINGUISHABLE,
// exactly the version-skew hole this build identity exists to close. A
// short sha is also not collision-proof across a shallow clone or a
// large enough repo. This build now:
//   - uses the FULL sha, never `--short`;
//   - appends `-dirty` when the working tree has uncommitted changes, so
//     a locally-modified build never claims to BE the commit it was
//     merely built from;
//   - FAILS the build outright when no git repository is found (a
//     release tarball, a checkout with git unavailable), UNLESS the
//     caller supplies one explicitly via the `SOT_BUILD_ID` environment
//     variable -- an empty/absent identity is not a safe default here,
//     it is exactly the "two builds nobody can tell apart" case.
//
// Codex review round 3, N12/M9 (the SOT_BUILD_ID half, fixed here now):
// `exchange.rs` truncates an oversized build id on the WIRE
// (`wire::MAX_SUPERVISOR_STRING_LEN`, 128 bytes) before ever comparing
// it, so a caller-supplied `SOT_BUILD_ID` longer than that bound would
// make this exact binary's own transmitted id disagree with its own
// locally-held constant -- a binary that can never successfully
// handshake with ITSELF. Bounded and FAILED here instead, at build
// time, rather than discovered as a live version-skew refusal against a
// peer running the identical binary. `wire::MAX_SUPERVISOR_STRING_LEN`
// cannot be imported into a build script (a separate compilation unit
// with no access to the crate's own source), so the bound is
// duplicated as a literal below -- keep the two in sync if either ever
// changes.
//
// The OTHER half of N12/M9 -- two distinct DIRTY trees at the same HEAD
// still sharing one identical `<sha>-dirty` id -- is DEFERRED to U4:
// U4's release-apply transaction never ships a dirty build in the first
// place (the release pipeline builds from a clean, tagged checkout), so
// the aliasing this would prevent cannot occur on a build that actually
// ships. Hashing the dirty tree's own content is real, bounded work
// (walking `git diff`) that belongs with that transaction's own
// integrity checks, not bolted onto this build script speculatively.
//
// Emitted rustc-env var (always nonempty on a successful build):
//   SOT_LOG_BUILD_SHA   full commit sha, "-dirty" suffixed if the
//                       working tree has uncommitted changes, OR the
//                       caller-supplied SOT_BUILD_ID when git is
//                       unavailable.

use std::process::Command;

/// Duplicated from `wire::MAX_SUPERVISOR_STRING_LEN` -- see this file's
/// own top comment on why a build script cannot import it directly.
const MAX_BUILD_ID_LEN: usize = 128;

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
/// itself could not be determined (fails closed: an unverifiable tree is
/// treated as dirty, never silently assumed clean).
fn is_dirty() -> bool {
    match Command::new("git").args(["status", "--porcelain"]).output() {
        Ok(out) if out.status.success() => !out.stdout.is_empty(),
        _ => true,
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=SOT_BUILD_ID");

    let sha = git(&["rev-parse", "HEAD"]); // the FULL sha, never --short

    // Re-stamp when HEAD moves (commit, checkout, tag) OR the tree's
    // dirty/clean state changes. Best-effort: absent paths make these
    // directives inert.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/refs");
        println!("cargo:rerun-if-changed={git_dir}/index");
    }

    let id = match sha {
        Some(sha) => {
            if is_dirty() {
                format!("{sha}-dirty")
            } else {
                sha
            }
        }
        None => match std::env::var("SOT_BUILD_ID") {
            Ok(id) if id.is_empty() => panic!(
                "sot-log build.rs: cannot derive a build identity -- no git repository was found \
                 (a release tarball, or a checkout where git is unavailable) and the SOT_BUILD_ID \
                 environment variable was not set to supply one explicitly. This identity gates \
                 the supervisor lane's own build boundary (ADR 0041 \"Build boundary\"): silently \
                 falling back to the bare package version would make two DIFFERENT commits \
                 sharing one pre-release Cargo.toml version indistinguishable -- exactly the \
                 version-skew hole this exists to close, not a missing convenience to paper over. \
                 Set SOT_BUILD_ID=<some-unique-string> to build outside a git checkout."
            ),
            Ok(id) if id.len() > MAX_BUILD_ID_LEN => panic!(
                "sot-log build.rs: SOT_BUILD_ID is {} bytes, exceeding the {MAX_BUILD_ID_LEN}-byte \
                 wire bound (wire::MAX_SUPERVISOR_STRING_LEN) the supervisor lane's own hello \
                 exchange truncates every build id to (Codex review round 3, N12) -- an oversized \
                 id would make this exact binary's OWN transmitted id disagree with its own \
                 locally-held constant, so it could never successfully handshake with itself. \
                 Use a shorter SOT_BUILD_ID.",
                id.len()
            ),
            Ok(id) => id,
            Err(_) => panic!(
                "sot-log build.rs: cannot derive a build identity -- no git repository was found \
                 (a release tarball, or a checkout where git is unavailable) and the SOT_BUILD_ID \
                 environment variable was not set to supply one explicitly. This identity gates \
                 the supervisor lane's own build boundary (ADR 0041 \"Build boundary\"): silently \
                 falling back to the bare package version would make two DIFFERENT commits \
                 sharing one pre-release Cargo.toml version indistinguishable -- exactly the \
                 version-skew hole this exists to close, not a missing convenience to paper over. \
                 Set SOT_BUILD_ID=<some-unique-string> to build outside a git checkout."
            ),
        },
    };

    println!("cargo:rustc-env=SOT_LOG_BUILD_SHA={id}");
}
