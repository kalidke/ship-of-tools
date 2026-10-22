//! The paths the daemon and the window derive from the machine: where the repo's resources live, the `\\?\` prefix
//! Windows puts on a canonical path, and the checks that a folder only this account can use is one.

use std::path::{Path, PathBuf};

/// An OS error with what was being done; the error itself stays as the source.
fn context(source: std::io::Error, what: String) -> crate::Error {
    crate::Error::Path { what, source }
}

/// Resolve a REPO-ROOT-RELATIVE resource path (e.g. `julia/kernel`,
/// `rust/backend/sidecars/mathjax/render.mjs`) for both deployment layouts
/// (ADR 0030 §4). Resolution order, first EXISTING path wins:
///
///   1. `$SOT_RESOURCE_ROOT/<rel>` — explicit override (tests, exotic setups);
///   2. install layout: `<exe-dir>/../julia/current/<rel>` — a release
///      install is `PREFIX/bin/sotd` with `PREFIX/julia/current` a symlink to
///      the unpacked julia bundle, which is REPO-SHAPED inside (the release
///      workflow packs with `cp --parents`), so the same rel strings work;
///   3. dev checkout: `CARGO_MANIFEST_DIR/../../<rel>` — compile-time repo
///      path, also the fallback when nothing exists so error messages point
///      at the path a developer expects.
pub fn resource_dir(rel: &str) -> PathBuf {
    if let Ok(root) = std::env::var("SOT_RESOURCE_ROOT") {
        let p = PathBuf::from(root).join(rel);
        if p.exists() {
            return p;
        }
        tracing::warn!(rel, root = %p.display(), "SOT_RESOURCE_ROOT set but path missing; falling through");
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(prefix) = exe.parent().and_then(|bin| bin.parent()) {
            // Clone-based install (ADR 0030 addendum): the repo checkout at
            // the release tag is the whole resource tree — repo-shaped by
            // definition, so the same rel strings resolve directly.
            let p = prefix.join("repo").join("current").join(rel);
            if p.exists() {
                tracing::debug!(rel, path = %p.display(), "resource resolved via repo checkout");
                return p;
            }
            // Legacy bundle layout (pre-clone installs, <= v0.2.3).
            let p = prefix.join("julia").join("current").join(rel);
            if p.exists() {
                tracing::debug!(rel, path = %p.display(), "resource resolved via install layout");
                return p;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(rel)
}

/// De-verbatim a canonicalized path on Windows. `std::fs::canonicalize`
/// returns `\\?\C:\...` verbatim paths there, and verbatim paths DISABLE
/// Win32 normalization — any later composition using `/` (FE-composed
/// paths, wire tails, the kernel's joinpath) puts a literal `/` in the
/// filename and fails with "no such file" (found the hard way: the
/// concept-stale drift-badge saga, 2026-07-02). Stripping back to a plain
/// drive path (or `\\server\share` for UNC) restores slash-tolerant
/// semantics for everything downstream. No-op on non-Windows and for
/// non-verbatim paths. Trade-off: plain paths re-gain the MAX_PATH limit —
/// acceptable for project roots.
pub fn simplify_verbatim(p: std::path::PathBuf) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let s = p.as_os_str().to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            let b = rest.as_bytes();
            if b.len() >= 2 && b[1] == b':' {
                return PathBuf::from(rest.to_string());
            }
        }
        // A toml saved before the reader unescaped `toml_quote`'s output
        // (or hand-edited raw) carries this prefix with one fewer leading
        // backslash than the true verbatim form once `toml_unquote` runs:
        // its leading `\\` reads as one escaped backslash, halving `\\?\`
        // to `\?\`. Accept that shape too (`rows/store/codec.rs`'s `toml_unquote`).
        if let Some(rest) = s.strip_prefix(r"\?\") {
            let b = rest.as_bytes();
            if b.len() >= 2 && b[1] == b':' {
                return PathBuf::from(rest.to_string());
            }
        }
        p
    }
    #[cfg(not(windows))]
    {
        p
    }
}

/// Create `dir` (and its parents) if needed, then enforce 0700 permissions
/// UNCONDITIONALLY — not just on creation. A dir left over from before this
/// security fix (or created under a looser umask before `main`'s
/// `apply_umask` ran) won't self-correct otherwise. No-op permission-wise on
/// non-Unix (Windows ACLs are a separate mechanism, out of scope here).
///
/// Trust model: this is only safe to use for a path whose PARENT a hostile
/// local user cannot already write into — today, `state_dir()`'s log dir
/// under `$HOME`/`$XDG_STATE_HOME`, which inherits `$HOME`'s own privacy.
/// `create_dir_all` is a no-op on an already-existing path and
/// `set_permissions` follows symlinks (chmods the symlink's TARGET, not the
/// link), so this does NOT reject a pre-planted symlink or an attacker-owned
/// directory — it would silently trust either. For anything whose parent
/// IS attacker-reachable (`/tmp`, a shared runtime dir), use
/// `secure_private_dir` instead — that's what the tmux socket dir switched
/// to after F1 (security review).
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Create-or-verify `dir` as a directory that is EXCLUSIVELY ours (security
/// review, F1 — closes the "socket-dir hijack" hole `ensure_private_dir`
/// left open for an attacker-reachable parent like `/tmp` or a shared
/// runtime dir). A hostile local user who can write into `dir`'s parent
/// could pre-create `dir` — or plant a SYMLINK at that path — as their own
/// before this daemon ever runs; `ensure_private_dir`'s
/// `create_dir_all`-then-`chmod` sequence would have trusted either
/// unconditionally (create_dir_all no-ops on an existing path; chmod
/// follows a symlink to its target rather than rejecting it) and this
/// daemon would then place its tmux socket inside a directory the attacker
/// controls (DoS at minimum; worse, a foothold into whatever the attacker
/// wired that directory to receive).
///
/// Contract — `Err` on ANY failed check, never a silent fallback:
/// - absent → created EXCLUSIVELY at mode `0700`. Plain (non-`_all`)
///   `create_dir` + `DirBuilderExt::mode` maps straight to a single
///   `mkdir(2)`, which is atomic: the kernel either creates a fresh
///   directory with that mode or fails with `AlreadyExists` — no window
///   between create and chmod for a racing attacker to land a symlink in.
///   Callers only ever reach this for a single trailing path component
///   (the socket's immediate parent); the dir ABOVE it — `$XDG_RUNTIME_DIR`,
///   `/run/user/<uid>`, or `/tmp` — is assumed to already exist.
/// - present → verified via `symlink_metadata` (lstat — does NOT follow a
///   symlink): must be a real directory, owned by THIS process's uid, and
///   owner-only (`mode & 0o077 == 0`). Same checks `is_private_dir` applies
///   to `$XDG_RUNTIME_DIR` itself, for the same reason.
#[cfg(unix)]
pub fn secure_private_dir(dir: &Path) -> crate::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A fresh box has no `~/.local/state` yet (the release smoke's
            // container had none): the parents are ordinary directories,
            // only the leaf is the private one.
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| context(e, format!("create parent of {}", dir.display())))?;
            }
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(dir)
                .map_err(|e| context(e, format!("create private dir {}", dir.display())))?;
            Ok(())
        }
        Err(e) => Err(context(e, format!("stat {}", dir.display()))),
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(crate::Error::State(format!("refusing to use {} as a private dir — it's a symlink \
                     (possible hijack by another local user)",
                    dir.display())));
            }
            if !meta.is_dir() {
                return Err(crate::Error::State(format!("refusing to use {} as a private dir — not a directory",
                    dir.display())));
            }
            if meta.uid() != super::state_dir::current_uid() {
                return Err(crate::Error::State(format!("refusing to use {} as a private dir — owned by uid {} \
                     (expected {}; possible hijack by another local user)",
                    dir.display(),
                    meta.uid(),
                    super::state_dir::current_uid(),)));
            }
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(crate::Error::State(format!("refusing to use {} as a private dir — mode {:o} is \
                     group/other-accessible",
                    dir.display(),
                    meta.permissions().mode() & 0o777,)));
            }
            Ok(())
        }
    }
}
#[cfg(not(unix))]
pub fn secure_private_dir(dir: &Path) -> crate::Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| context(e, format!("create dir {}", dir.display())))
}

/// Create/verify a socket parent directory. For the canonical runtime tree,
/// create each private component (`.../sot`, then `.../sot/sessions`) with the
/// same symlink/owner/mode checks as `secure_private_dir`. Custom socket paths
/// still get their immediate parent verified; callers using deeper custom
/// paths should create the higher private parent explicitly.
pub fn secure_socket_dir(dir: &Path) -> crate::Result<()> {
    let runtime = super::state_dir::runtime_sot_dir();
    if dir == runtime {
        return secure_private_dir(dir);
    }
    if let Ok(rel) = dir.strip_prefix(&runtime) {
        secure_private_dir(&runtime)?;
        let mut cur = runtime;
        for comp in rel.components() {
            cur.push(comp.as_os_str());
            secure_private_dir(&cur)?;
        }
        return Ok(());
    }
    secure_private_dir(dir)
}

#[cfg(all(test, windows))]
#[path = "paths_verbatim_tests.rs"]
mod verbatim_tests;

#[cfg(all(test, not(windows)))]
#[path = "paths_non_windows_noop_tests.rs"]
mod non_windows_noop_tests;

#[cfg(all(test, unix))]
#[path = "paths_secure_private_dir_tests.rs"]
mod secure_private_dir_tests;
