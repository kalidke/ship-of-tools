// paths.rs — conventional paths for backend sessions per ADR 0013.
//
// One backend per project. Each backend listens on a per-session socket
// derived from a stable label (project name, etc.) so multiple backends
// coexist on the same host. Sessions mode in the frontend uses the same
// derivations when spawning daemons via `tmux.create_session`, which is
// why the rules need to be deterministic and documented.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Crate-wide serialization lock for every test that mutates process-
/// global env vars this crate's resolvers read (`XDG_CONFIG_HOME`,
/// `XDG_STATE_HOME`, `HOME`, `LOCALAPPDATA`, `USERPROFILE`, `SystemDrive`,
/// `SOT_SELF_HOST`, ...) — `cargo test` runs tests in parallel within one
/// process by default, and several DIFFERENT modules
/// (`paths::state_dir_tests`, `rows::store::tests`)
/// each exercise resolvers that read the SAME vars. One shared lock, not
/// one per module (Codex review, PR #175: two separate mutexes — this
/// file's own and `rows/store/`'s — meant a test in one module could
/// still race a test in the other over the same env vars).
///
/// No test takes the system folders out of the process `PATH` or changes `SHELL`, so a bare program name always
/// resolves; code under test takes both from its caller (`agents::argv::AgentEnv`, `sidecars::julia::resolve_bin_on`).
/// The one `PATH` writer left, `topology::dial::tests::prepend_to_path`, puts a stub `ssh` first under this lock, and
/// only the tests that call it run a bare `ssh`. The SSH fixtures' behavior test,
/// `ssh_fixtures_preserve_parent_path_and_shell` in `tests/lane_bridge/dial.rs`, observes the parent `PATH` and
/// `SHELL` around every fixture run.
#[cfg(test)]
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Restores one variable on drop, to its value at `capture` or unset.
#[cfg(test)]
pub(crate) struct EnvGuard(&'static str, Option<std::ffi::OsString>);
#[cfg(test)]
impl EnvGuard {
    pub(crate) fn capture(key: &'static str) -> Self {
        Self(key, std::env::var_os(key))
    }
}
#[cfg(test)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.1.take() {
            Some(v) => std::env::set_var(self.0, v),
            None => std::env::remove_var(self.0),
        }
    }
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

#[cfg(all(test, windows))]
mod verbatim_tests {
    use super::simplify_verbatim;
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
    fn strips_single_backslash_drive_verbatim() {
        // The shape `toml_unquote` (`rows/store/codec.rs`) produces for a raw,
        // never-escaped legacy write of `\\?\C:\...`: its leading `\\`
        // reads as one escaped backslash, halving the prefix to `\?\`.
        assert_eq!(
            simplify_verbatim(PathBuf::from(r"\?\C:\Users\k\proj")),
            PathBuf::from(r"C:\Users\k\proj")
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
}

/// `simplify_verbatim` is a Windows-only rewrite — everywhere else (this
/// crate's Linux/macOS CI leg included) it must be a true no-op, verbatim
/// prefix or not, so a Windows-formatted string passed through on a
/// non-Windows host round-trips unchanged rather than being mangled.
#[cfg(all(test, not(windows)))]
mod non_windows_noop_tests {
    use super::simplify_verbatim;
    use std::path::PathBuf;

    #[test]
    fn leaves_plain_path_unchanged() {
        let p = PathBuf::from("/home/u/proj");
        assert_eq!(simplify_verbatim(p.clone()), p);
    }

    #[test]
    fn leaves_windows_style_verbatim_string_unchanged() {
        let p = PathBuf::from(r"\\?\C:\Users\k\proj");
        assert_eq!(simplify_verbatim(p.clone()), p);
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
///
/// ADR 0042 L2b: `slug`, `session_socket_path` and the private-runtime-dir
/// resolution it needs (`runtime_sot_dir`, `is_private_dir`, `current_uid`)
/// moved to `sot_protocol::topology::endpoint` — the ONE derivation of a
/// daemon's per-user endpoint, so the frontend can call the exact same
/// function for its implicit "local" connection (`hosts::resolve_connections`)
/// instead of guessing. Re-exported here (except `is_private_dir`, which
/// nothing in this crate calls directly any more — `runtime_sot_dir` is its
/// only caller, and that moved too) so every existing `paths::slug(...)` /
/// `paths::session_socket_path(...)` call site in this crate, and the
/// `session_name`/`tmux_socket_path`/`secure_private_dir`/
/// `secure_socket_dir` below that still need the shared helpers, keep
/// working unchanged. See that module for the Windows named-pipe branch and
/// the moved doc comments/tests.
pub use sot_protocol::{current_uid, local_daemon_label, runtime_sot_dir, session_socket_path, slug};

/// Resolves the Windows per-machine state root, or fails startup with a
/// clear message. On Windows this is the ONLY root `state_dir()` below and
/// `rows::store::app_config_dir` derive from — no POSIX (`XDG_*`/`HOME`/
/// `/tmp`) fallback chain is reachable on this platform any more (Codex
/// review, PR #175: silently falling back to a `$HOME`-shaped path on
/// Windows — which depends on which shell launched the daemon — is
/// exactly the bug class this whole fix exists to close).
/// `sot_log::host::state_dir::sot_state_dir()` itself derives
/// `%USERPROFILE%\AppData\Local` when `%LOCALAPPDATA%` is unset or empty,
/// so this only panics in the genuinely exceptional case where NEITHER is
/// set — not a normal Windows login.
#[cfg(windows)]
pub(crate) fn windows_state_root() -> PathBuf {
    sot_log::host::state_dir::sot_state_dir().unwrap_or_else(|| {
        panic!(
            "cannot resolve the Windows state root: neither %LOCALAPPDATA% nor \
             %USERPROFILE% is set — sotd cannot start without one"
        )
    })
}

/// `${XDG_STATE_HOME:-~/.local/state}/sot` — private, persistent runtime
/// artifacts sotd owns itself (its log file today; a natural home for more
/// later). Security review: this replaces relying on the LAUNCHER to
/// redirect stdout to a world-readable `/tmp/sotd.log` — sotd now owns a
/// private copy of its own log regardless of how it's launched. Falls back
/// to `/tmp/.local/state/sot` if `$HOME` is unset (very rare; parallels
/// `rows::store::app_config_dir`'s fallback) — Unix only; see `windows_state_root`
/// for the Windows resolution (`%LOCALAPPDATA%\sot\state`, joined with
/// `state` so it sits beside `rows::store::app_config_dir`'s `config`
/// without colliding with the capsule runtime's own `workspaces\<id>`
/// subtree — `rows::spawn::state_root::state_dir_for`). Unlike the config
/// registry, this log directory holds no durable data worth migrating — a
/// fresh one on first post-fix boot is fine, so there is no Windows
/// migration step here (contrast
/// `rows::store::migrate::migrate_legacy_windows_config_dir`).
pub fn state_dir() -> PathBuf {
    #[cfg(windows)]
    return windows_state_root().join("state");
    #[cfg(not(windows))]
    {
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
            // A fresh box has no `~/.local/state` yet (the release smoke's
            // container had none): the parents are ordinary directories,
            // only the leaf is the private one.
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create parent of {}", dir.display()))?;
            }
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

/// Strict allowlist for names (security review): `1..=64` ASCII alphanumerics, `.`, `_`, `-` only — no shell
/// metacharacters, no `|`, no whitespace/control bytes.
pub(crate) fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `is_private_dir`'s own tests moved to `sot_protocol::topology::endpoint`
/// with the function (ADR 0042 L2b) — see that module's
/// `is_private_dir_tests`. `secure_private_dir`'s tests (below) still
/// exercise `current_uid` (re-exported above) directly.
///
/// `secure_private_dir` — the create-or-verify guard for the tmux socket's
/// parent dir (F1: this is the function that actually closes the hijack
/// hole, since `tmux.rs`/`pty.rs` call THIS, not `is_private_dir` directly).
#[cfg(all(test, unix))]
mod secure_private_dir_tests {

    #[test]
    fn a_missing_parent_is_created_and_the_leaf_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join("a").join("b").join("sot");
        secure_private_dir(&leaf).unwrap();
        assert_eq!(std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777, 0o700);
        secure_private_dir(&leaf).unwrap(); // idempotent on the second boot
    }
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

/// `state_dir()`'s platform dispatch. The Unix branch is unchanged
/// behaviour (still `$XDG_STATE_HOME` / `$HOME/.local/state/sot` /
/// `/tmp/.local/state/sot`); the precedence of `%LOCALAPPDATA%` over
/// `$XDG_STATE_HOME` on Windows is `sot_log::host::state_dir::sot_state_dir`'s
/// own contract (tested there) — this only proves `state_dir()` appends
/// `state` beneath it and still falls back when `%LOCALAPPDATA%` is
/// unset. Env-guard shape matches `sot_log::host::state_dir`'s own tests
/// (`state_dir.rs`) so the two stay easy to compare.
#[cfg(test)]
mod state_dir_tests {
    use super::*;

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        xdg_state_home: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
        localappdata: Option<std::ffi::OsString>,
        userprofile: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("XDG_STATE_HOME", &self.xdg_state_home),
                ("HOME", &self.home),
                ("LOCALAPPDATA", &self.localappdata),
                ("USERPROFILE", &self.userprofile),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
            home: std::env::var_os("HOME"),
            localappdata: std::env::var_os("LOCALAPPDATA"),
            userprofile: std::env::var_os("USERPROFILE"),
            _serial: serial,
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn unix_still_prefers_xdg_state_home() {
        let _guard = guarded();
        std::env::set_var("XDG_STATE_HOME", "/xdg-state");
        std::env::set_var("HOME", "/home/someone");
        assert_eq!(state_dir(), PathBuf::from("/xdg-state/sot"));
    }

    #[test]
    #[cfg(not(windows))]
    fn unix_falls_back_to_home_when_xdg_state_home_unset() {
        let _guard = guarded();
        std::env::remove_var("XDG_STATE_HOME");
        std::env::set_var("HOME", "/home/someone");
        assert_eq!(state_dir(), PathBuf::from("/home/someone/.local/state/sot"));
    }

    #[test]
    #[cfg(windows)]
    fn windows_uses_localappdata_state_subdir() {
        let _guard = guarded();
        std::env::set_var("LOCALAPPDATA", r"C:\Users\someone\AppData\Local");
        assert_eq!(
            state_dir(),
            PathBuf::from(r"C:\Users\someone\AppData\Local\sot\state")
        );
    }

    #[test]
    #[cfg(windows)]
    fn windows_ignores_xdg_state_home() {
        let _guard = guarded();
        std::env::set_var("XDG_STATE_HOME", r"C:\should\be\ignored");
        std::env::set_var("LOCALAPPDATA", r"C:\Users\someone\AppData\Local");
        assert_eq!(
            state_dir(),
            PathBuf::from(r"C:\Users\someone\AppData\Local\sot\state")
        );
    }

    #[test]
    #[cfg(windows)]
    fn windows_falls_back_to_userprofile_when_localappdata_unset() {
        let _guard = guarded();
        std::env::remove_var("LOCALAPPDATA");
        std::env::remove_var("XDG_STATE_HOME");
        std::env::set_var("USERPROFILE", r"C:\Users\someone");
        assert_eq!(
            state_dir(),
            PathBuf::from(r"C:\Users\someone\AppData\Local\sot\state")
        );
    }

    #[test]
    #[cfg(windows)]
    #[should_panic(expected = "cannot resolve the Windows state root")]
    fn windows_panics_when_localappdata_and_userprofile_are_both_unset() {
        let _guard = guarded();
        std::env::remove_var("LOCALAPPDATA");
        std::env::remove_var("USERPROFILE");
        let _ = state_dir();
    }
}

#[cfg(test)]
mod valid_name_tests {
    use super::valid_name;

    #[test]
    fn accepts_typical_names() {
        assert!(valid_name("sot-be-myhost"));
        assert!(valid_name("myhost-dev"));
        assert!(valid_name("MyPackage.jl"));
        assert!(valid_name("a"));
        assert!(valid_name(&"a".repeat(64)));
    }

    #[test]
    fn rejects_empty_and_oversize() {
        assert!(!valid_name(""));
        assert!(!valid_name(&"a".repeat(65)));
    }

    #[test]
    fn rejects_shell_and_parser_metacharacters() {
        // The pipe is the specific `tmux.rs` list-parsing corruption vector;
        // the rest are generic shell-injection/whitespace rejects.
        for bad in [
            "a|b",
            "a;b",
            "a b",
            "a'b",
            "a$b",
            "a`b",
            "a\nb",
            "/etc/passwd",
        ] {
            assert!(!valid_name(bad), "expected {bad:?} to be rejected");
        }
    }
}
