// build.rs — stamp SOT_LOG_BUILD_SHA, the build id the supervisor lane's hello carries.
// The id is informational: ADR 0045 decision 7 retired the build-boundary gate it was
// made for (src/identity/exchange.rs says "never compared"); the rules below outlive that gate.
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
// A caller-supplied `SOT_BUILD_ID` is bounded here (see `MAX_BUILD_ID_LEN`): the row's hello cuts an oversized build
// to a length that keeps its request far inside the wire's cap, so a longer id would make this exact binary's
// transmitted id disagree with its own locally-held constant. Bounded and FAILED here instead, at build time, rather
// than discovered later. The bound cannot be imported into a build script (a separate compilation unit with no access
// to the crate's own source), so it is a literal below.
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
//

use std::process::Command;

/// The longest build id the row's hello carries whole (`MAX_BUILD_BYTES` in `src/row/proto.rs`) -- see this file's own top comment on why a build script cannot
/// import it.
const MAX_BUILD_ID_LEN: usize = 256;

fn git(args: &[&str]) -> Option<String> {
    #[allow(clippy::disallowed_methods, reason = "a build script runs git at compile time; no daemon runs here")]
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
    #[allow(clippy::disallowed_methods, reason = "a build script runs git at compile time; no daemon runs here")]
    let status = Command::new("git").args(["status", "--porcelain"]).output();
    match status {
        Ok(out) if out.status.success() => !out.stdout.is_empty(),
        _ => true,
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=SOT_BUILD_ID");

    let sha = git(&["rev-parse", "HEAD"]); // the FULL sha, never --short

    // Re-stamp when HEAD, refs or the index change (a staged edit; an unstaged
    // edit is not seen). Best-effort: absent paths make these directives inert.
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
                 bound the row's hello keeps a build id whole to (an oversized \
                 id would make this exact binary's OWN transmitted id disagree with its own \
                 locally-held constant). Use a shorter SOT_BUILD_ID.",
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
