// paths.rs — conventional paths for backend sessions per ADR 0013.
//
// One backend per project. Each backend listens on a per-session socket
// derived from a stable label (project name, etc.) so multiple backends
// coexist on the same host. Sessions mode in the frontend uses the same
// derivations when spawning daemons via `tmux.create_session`, which is
// why the rules need to be deterministic and documented.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Canonicalize the longest EXISTING ancestor of `p`, walking up past
/// components that don't exist yet (e.g. a `file.write`/`concept.write`
/// target that hasn't been created). Used by the workspace-confinement
/// symlink-escape guards in `files_mode.rs` and `concept.rs`: string-level
/// `..`/absolute-path checks on a node id can't catch a symlink INSIDE the
/// root pointing outside it (a real risk on NFS-shared homes, where a
/// symlink can legitimately cross machines/mounts). A target that doesn't
/// exist yet can't itself BE a symlink escaping the root, so checking its
/// nearest existing ancestor is exactly as strong a guarantee as checking
/// the full path once it's created — any escape has to go through a
/// component that already exists. Returns `None` only if not even the
/// filesystem root canonicalizes, which shouldn't happen.
pub fn canonicalize_existing_ancestor(p: &Path) -> Option<PathBuf> {
    let mut cur = p;
    loop {
        if let Ok(c) = cur.canonicalize() {
            return Some(simplify_verbatim(c));
        }
        cur = cur.parent()?;
    }
}

/// True when `candidate` is exactly `root` or a descendant of it, after
/// applying the same Windows verbatim-prefix normalization used for canonical
/// project roots. The comparison is component-wise, so `/a/bc` is not treated
/// as being under `/a/b`.
pub fn path_within_root(candidate: &Path, root: &Path) -> bool {
    let candidate = simplify_verbatim(candidate.to_path_buf());
    let root = simplify_verbatim(root.to_path_buf());
    let mut candidate_components = candidate.components();
    for root_component in root.components() {
        match candidate_components.next() {
            Some(candidate_component) if candidate_component == root_component => {}
            _ => return false,
        }
    }
    true
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
        p
    }
    #[cfg(not(windows))]
    {
        p
    }
}

#[cfg(all(test, windows))]
mod verbatim_tests {
    use super::{path_within_root, simplify_verbatim};
    use std::path::PathBuf;

    #[test]
    fn strips_drive_verbatim() {
        assert_eq!(
            simplify_verbatim(PathBuf::from(r"\\?\C:\Users\k\proj")),
            PathBuf::from(r"C:\Users\k\proj")
        );
    }

    #[test]
    fn strips_unc_verbatim() {
        assert_eq!(
            simplify_verbatim(PathBuf::from(r"\\?\UNC\srv\share\x")),
            PathBuf::from(r"\\srv\share\x")
        );
    }

    #[test]
    fn leaves_plain_and_odd_verbatim_alone() {
        assert_eq!(
            simplify_verbatim(PathBuf::from(r"C:\plain")),
            PathBuf::from(r"C:\plain")
        );
        // A verbatim path whose remainder isn't a drive path stays verbatim
        // rather than being mangled.
        assert_eq!(
            simplify_verbatim(PathBuf::from(r"\\?\Volume{guid}\x")),
            PathBuf::from(r"\\?\Volume{guid}\x")
        );
    }

    #[test]
    fn path_within_root_accepts_verbatim_child() {
        assert!(path_within_root(
            &PathBuf::from(r"\\?\C:\Users\k\proj\src\lib.rs"),
            &PathBuf::from(r"C:\Users\k\proj")
        ));
    }
}

#[cfg(test)]
mod path_tests {
    use super::path_within_root;
    use std::path::Path;

    #[test]
    fn path_within_root_is_component_based() {
        assert!(path_within_root(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(path_within_root(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!path_within_root(Path::new("/a/bc"), Path::new("/a/b")));
    }
}

/// Filesystem-safe slug derived from an arbitrary label. Lowercased; runs of
/// non-`[a-z0-9_-]` characters collapse to a single `-`; dots are replaced
/// with `_` so tmux session names (which silently substitute `.` and `:`)
/// round-trip through `tmux ls`; leading/trailing dashes stripped. Empty
/// input → `default`.
///
/// Examples:
///   "MyPackage.jl" → "mypackage_jl"
///   "Foo Bar"      → "foo-bar"
///   "/abs/path"    → "abs-path"
///   "  "           → "default"
pub fn slug(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut last_dash = false;
    for ch in label.chars() {
        let c = ch.to_ascii_lowercase();
        // Translate `.` to `_` *before* the keep check — tmux session
        // names can't contain `.` so a slug with a dot would be
        // mis-named on creation and unfindable on reverse lookup.
        let c = if c == '.' { '_' } else { c };
        let keep = c.is_ascii_alphanumeric() || c == '_' || c == '-';
        if keep {
            out.push(c);
            last_dash = c == '-';
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

/// Conventional tmux session name for a backend with the given label.
/// Not currently consumed inside the backend — kept here as the
/// authoritative naming rule so future code (and the frontend, when it
/// picks up Sessions mode B2-B5) has one place to look.
#[allow(dead_code)]
pub fn tmux_session_name(label: &str) -> String {
    format!("sot-be-{}", slug(label))
}

/// Conventional Unix socket path for a backend with the given label.
/// The root is per-user, not just per-machine: `$XDG_RUNTIME_DIR/sot` when
/// that directory is private, then `/run/user/<uid>/sot`, then a private
/// `/tmp/sot-<uid>` fallback.
pub fn session_socket_path(label: &str) -> PathBuf {
    let mut p = runtime_sot_dir();
    p.push("sessions");
    p.push(format!("{}.sock", slug(label)));
    p
}

/// Per-user runtime root for Ship of Tools sockets.
pub fn runtime_sot_dir() -> PathBuf {
    if let Some(dir) = private_xdg_runtime_dir() {
        return dir.join("sot");
    }
    let uid = current_uid();
    let run_user_dir = PathBuf::from(format!("/run/user/{uid}"));
    if is_private_dir(&run_user_dir) {
        return run_user_dir.join("sot");
    }
    PathBuf::from(format!("/tmp/sot-{uid}"))
}

/// Private per-user tmux server socket (security review): tmux's OWN
/// default server lives at `/tmp/tmux-<uid>/default`. On this system that
/// directory is `0700`, but the socket FILE itself can end up group-
/// readable (`srwxrwx---`) — on a shared host where multiple human accounts
/// share a primary/supplementary GID (a common `useradd` default), that lets
/// another local user `tmux -S /tmp/tmux-<uid>/default attach` straight into
/// this daemon's live agent sessions. A FIXED, well-known path (not a
/// randomized one) so other tooling — the sot-comm shell scripts, and
/// `sotd tmux-socket-path` (the single-source-of-truth CLI query they use to
/// stay in sync) — can target the exact same server.
///
/// Set `SOT_TMUX_SOCK=/path/to/socket` to intentionally target an existing
/// tmux server during migration. The caller still verifies the socket parent
/// directory before spawning tmux.
///
/// Resolution order — deterministic, NO randomness (a prior version of this
/// fell back to `state_dir()`, i.e. `${XDG_STATE_HOME:-~/.local/state}/sot`;
/// Codex flagged that on this lab's boxes `$HOME` is an NFS-shared mount
/// across a shared-$HOME NFS cohort, and a unix-domain socket does
/// not work over NFS — that fallback was silently non-functional on any box
/// without `$XDG_RUNTIME_DIR` set, exactly the case it exists to cover):
///   1. `$XDG_RUNTIME_DIR/sot/tmux.sock` — only when `$XDG_RUNTIME_DIR` is
///      set, exists, and is itself owner-only (no group/other permission
///      bits — same "don't trust it if others can get at it" posture as
///      `main.rs`'s token-file check). Normally a tmpfs mounted per-login-
///      session by systemd-logind: private, and always a LOCAL mount.
///   2. `/run/user/<uid>/sot/tmux.sock` — the well-known path behind that
///      same env var, for a shell that didn't inherit it (cron, some
///      su/sudo paths) but is still on a logind-managed box. Used only when
///      `/run/user/<uid>` exists and is owner-only.
///   3. `/tmp/sot-<uid>/tmux.sock` — last-resort local fallback. `/tmp` is
///      always a local mount (never NFS-shared, unlike `$HOME`), so this
///      stays correct even though, unlike tier 1, it isn't cleared on
///      logout. The parent dir is created `0700` by the caller
///      (`ensure_private_dir`) since `/tmp` itself is world-writable+sticky.
pub fn tmux_socket_path() -> PathBuf {
    if let Some(sock) = std::env::var_os("SOT_TMUX_SOCK") {
        return PathBuf::from(sock);
    }
    runtime_sot_dir().join("tmux.sock")
}

/// `$XDG_RUNTIME_DIR` if it's set, exists, and is owner-only (no group/other
/// bits). A world/group-accessible or missing runtime dir falls through to
/// the next resolution tier rather than being trusted. Split into an env
/// read (this) + a pure path check (`is_private_dir`) so the safety logic is
/// unit-testable against real temp dirs without mutating `$XDG_RUNTIME_DIR`
/// — a process-global env var other tests in this binary touch concurrently
/// (see `main.rs`'s `trim_token_contents`/`read_token_file_at` split, done
/// for the identical reason).
#[cfg(unix)]
fn private_xdg_runtime_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    if is_private_dir(&dir) {
        Some(dir)
    } else {
        None
    }
}
#[cfg(not(unix))]
fn private_xdg_runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
}

/// `true` if `dir` exists, is a REAL directory (not a symlink to one),
/// owned by THIS process's uid, and owner-only (no group/other bits).
///
/// Uses `symlink_metadata` (lstat — does NOT follow a symlink) rather than
/// `metadata`, and checks ownership, not just mode (security review, F1: a
/// hostile local user who can write into `$XDG_RUNTIME_DIR`'s parent could
/// otherwise plant a symlink there, or — if `$XDG_RUNTIME_DIR` itself were
/// ever attacker-writable, e.g. a misconfigured shared runtime dir — an
/// attacker-owned 0700 directory, and the old `metadata`-plus-mode-only
/// check would have followed/trusted either).
#[cfg(unix)]
fn is_private_dir(dir: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return false;
    }
    if !meta.is_dir() {
        return false;
    }
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if meta.uid() != current_uid() {
        return false;
    }
    meta.permissions().mode() & 0o077 == 0
}
#[cfg(not(unix))]
fn is_private_dir(dir: &Path) -> bool {
    dir.is_dir()
}

/// Numeric uid for path derivation (`/run/user/<uid>`, `/tmp/sot-<uid>`).
/// `0` on non-Unix, where these two tiers are never reached in practice
/// (Windows sessions don't run tmux at all — this function exists so the
/// module compiles everywhere, not because the value is meaningful there).
#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments, has no preconditions, and cannot
    // fail.
    unsafe { libc::getuid() }
}
#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// `${XDG_STATE_HOME:-~/.local/state}/sot` — private, persistent runtime
/// artifacts sotd owns itself (its log file today; a natural home for more
/// later). Security review: this replaces relying on the LAUNCHER to
/// redirect stdout to a world-readable `/tmp/sotd.log` — sotd now owns a
/// private copy of its own log regardless of how it's launched. Falls back
/// to `/tmp/.local/state/sot` if `$HOME` is unset (very rare; parallels
/// `workspaces::config_dir`'s fallback).
pub fn state_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(v).join("sot");
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".local");
        p.push("state");
        p.push("sot");
        return p;
    }
    PathBuf::from("/tmp/.local/state/sot")
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
pub fn secure_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(dir)
                .with_context(|| format!("create private dir {}", dir.display()))?;
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("stat {}", dir.display())),
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                anyhow::bail!(
                    "refusing to use {} as a private dir — it's a symlink \
                     (possible hijack by another local user)",
                    dir.display()
                );
            }
            if !meta.is_dir() {
                anyhow::bail!(
                    "refusing to use {} as a private dir — not a directory",
                    dir.display()
                );
            }
            if meta.uid() != current_uid() {
                anyhow::bail!(
                    "refusing to use {} as a private dir — owned by uid {} \
                     (expected {}; possible hijack by another local user)",
                    dir.display(),
                    meta.uid(),
                    current_uid(),
                );
            }
            if meta.permissions().mode() & 0o077 != 0 {
                anyhow::bail!(
                    "refusing to use {} as a private dir — mode {:o} is \
                     group/other-accessible",
                    dir.display(),
                    meta.permissions().mode() & 0o777,
                );
            }
            Ok(())
        }
    }
}
#[cfg(not(unix))]
pub fn secure_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create dir {}", dir.display()))
}

/// Create/verify a socket parent directory. For the canonical runtime tree,
/// create each private component (`.../sot`, then `.../sot/sessions`) with the
/// same symlink/owner/mode checks as `secure_private_dir`. Custom socket paths
/// still get their immediate parent verified; callers using deeper custom
/// paths should create the higher private parent explicitly.
pub fn secure_socket_dir(dir: &Path) -> Result<()> {
    let runtime = runtime_sot_dir();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_lowercases_alnum_preserved() {
        assert_eq!(slug("MyPackage"), "mypackage");
        assert_eq!(slug("Foo123"), "foo123");
    }

    #[test]
    fn slug_collapses_separators() {
        assert_eq!(slug("Foo Bar"), "foo-bar");
        assert_eq!(slug("a / b"), "a-b");
        assert_eq!(slug("a___b"), "a___b"); // underscores kept
        assert_eq!(slug("a   b"), "a-b");
    }

    #[test]
    fn slug_strips_trailing_dashes() {
        assert_eq!(slug("foo / "), "foo");
        assert_eq!(slug("foo /// "), "foo");
    }

    #[test]
    fn slug_replaces_dots_with_underscore() {
        // Tmux silently substitutes `.` with `_` in session names, so we
        // produce the substituted form up-front and avoid a round-trip
        // mismatch between the registry and `tmux ls`.
        assert_eq!(slug("MyPackage.jl"), "mypackage_jl");
        assert_eq!(slug("foo-bar"), "foo-bar");
    }

    #[test]
    fn slug_defaults_on_empty() {
        assert_eq!(slug(""), "default");
        assert_eq!(slug("   "), "default");
        assert_eq!(slug("///"), "default");
    }

    #[test]
    fn tmux_session_name_uses_slug() {
        assert_eq!(tmux_session_name("MyPackage.jl"), "sot-be-mypackage_jl");
    }

    #[test]
    fn tmux_socket_path_honours_explicit_env_override() {
        struct Guard(Option<std::ffi::OsString>);
        impl Drop for Guard {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(v) => std::env::set_var("SOT_TMUX_SOCK", v),
                    None => std::env::remove_var("SOT_TMUX_SOCK"),
                }
            }
        }
        let _g = Guard(std::env::var_os("SOT_TMUX_SOCK"));
        let expected = PathBuf::from("/tmp/sot-test-tmux/default");
        std::env::set_var("SOT_TMUX_SOCK", &expected);
        assert_eq!(tmux_socket_path(), expected);
    }

    #[test]
    fn session_socket_path_honours_xdg_runtime_dir() {
        // Temporarily override XDG_RUNTIME_DIR so the test is hermetic.
        // SAFETY: tests in this module don't run concurrently against
        // each other in a single test process (they're &self with no
        // shared mutable state) — but we restore the env on drop.
        struct Guard(Option<std::ffi::OsString>);
        impl Drop for Guard {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                    None => std::env::remove_var("XDG_RUNTIME_DIR"),
                }
            }
        }
        let _g = Guard(std::env::var_os("XDG_RUNTIME_DIR"));
        let runtime = std::env::temp_dir().join(format!("sot-runtime-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runtime);
        std::fs::create_dir_all(&runtime).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);
        let p = session_socket_path("MyPackage.jl");
        assert_eq!(
            p,
            runtime
                .join("sot")
                .join("sessions")
                .join("mypackage_jl.sock")
        );
        let _ = std::fs::remove_dir_all(&runtime);
    }
}

/// `tmux_socket_path`'s tier-1 safety check (`is_private_dir`), exercised
/// against real temp dirs with controlled permissions — deliberately NOT
/// via `$XDG_RUNTIME_DIR` mutation, for the same env-var-race reason noted
/// on `private_xdg_runtime_dir`'s doc comment.
#[cfg(all(test, unix))]
mod is_private_dir_tests {
    use super::is_private_dir;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "sot-paths-test-{}-{}-{name}",
            std::process::id(),
            n
        ))
    }

    #[test]
    fn owner_only_dir_is_private() {
        let d = scratch_dir("owner-only");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(is_private_dir(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn group_readable_dir_is_not_private() {
        let d = scratch_dir("group-readable");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(!is_private_dir(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn world_readable_dir_is_not_private() {
        let d = scratch_dir("world-readable");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!is_private_dir(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_dir_is_not_private() {
        let d = scratch_dir("missing");
        assert!(!is_private_dir(&d));
    }

    #[test]
    fn a_file_is_not_a_private_dir() {
        let d = scratch_dir("a-file");
        std::fs::write(&d, b"not a dir").unwrap();
        assert!(!is_private_dir(&d));
        let _ = std::fs::remove_file(&d);
    }

    #[test]
    fn symlink_to_a_private_dir_is_rejected() {
        // The hijack case (F1): even a symlink pointing at an otherwise-
        // valid owner-only dir must be rejected — trusting it would let an
        // attacker who controls the symlink redirect us anywhere later by
        // repointing it, and `is_private_dir` must reject based on the
        // PATH's own type (lstat), not what it resolves to.
        let target = scratch_dir("symlink-target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = scratch_dir("symlink-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(!is_private_dir(&link));
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&target);
    }
}

/// `secure_private_dir` — the create-or-verify guard for the tmux socket's
/// parent dir (F1: this is the function that actually closes the hijack
/// hole, since `tmux.rs`/`pty.rs` call THIS, not `is_private_dir` directly).
#[cfg(all(test, unix))]
mod secure_private_dir_tests {
    use super::secure_private_dir;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_path(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "sot-secure-dir-test-{}-{}-{name}",
            std::process::id(),
            n
        ))
    }

    #[test]
    fn absent_dir_is_created_owner_only() {
        let d = scratch_path("absent");
        assert!(secure_private_dir(&d).is_ok());
        let meta = std::fs::symlink_metadata(&d).unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn existing_owner_only_dir_is_accepted() {
        let d = scratch_path("existing-ok");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(secure_private_dir(&d).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn existing_group_readable_dir_is_rejected() {
        let d = scratch_path("existing-group-readable");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(secure_private_dir(&d).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn existing_symlink_is_rejected_even_to_a_valid_dir() {
        let target = scratch_path("symlink-target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = scratch_path("symlink-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(secure_private_dir(&link).is_err());
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn existing_regular_file_is_rejected() {
        let d = scratch_path("a-file");
        std::fs::write(&d, b"not a dir").unwrap();
        assert!(secure_private_dir(&d).is_err());
        let _ = std::fs::remove_file(&d);
    }
}
