// session_socket.rs — the ONE derivation of a Ship of Tools daemon's
// per-user session endpoint (ADR 0042 L2b design A).
//
// Moved here from `rust/backend/src/paths.rs` (ADR 0013's original home)
// so the frontend can call the SAME function the daemon uses for its own
// `--label`-derived socket/pipe — before this move the frontend had no way
// to compute a local daemon's endpoint itself, and `session_socket_path`
// had no Windows branch at all (it silently fell through to a POSIX-shaped
// `/tmp/sot-0/...` path that is not a real Windows pipe name). `sotd
// --label <name>` and `sotd session-socket-path <name>` both call straight
// through to `session_socket_path` below; `rust/backend/src/paths.rs`
// re-exports it (and the handful of helpers it needs) rather than keeping
// a second copy, so there is exactly one place this logic can drift.
//
// Unix: unchanged from ADR 0013 — `$XDG_RUNTIME_DIR/sot/sessions/<slug>.sock`
// when that directory is private, else `/run/user/<uid>/sot/sessions/...`,
// else a private `/tmp/sot-<uid>/sessions/...` fallback.
//
// Windows: a named pipe, `\\.\pipe\sot-<user>-<slug>`, with `<user>` =
// `%USERNAME%` and `<slug>` run through the same `slug()` every platform
// uses (so a label containing characters a pipe name would rather not carry
// — spaces, dots — degrades the same way it already does on Unix, rather
// than needing a second sanitization rule). This is the exact shape
// `scripts/sot-local-daemon.ps1` used to construct by hand
// (`sot-$env:USERNAME-local`); that construction is deleted in favour of
// asking the daemon (`sotd session-socket-path local`) — see that script's
// header.

use std::path::PathBuf;

/// Conventional per-user session endpoint for a backend with the given
/// label: a Unix socket path, or (Windows) a named pipe path. `sotd
/// --label <name>` uses this to derive `--socket` when the flag is omitted
/// (ADR 0013); the frontend's `hosts::resolve_connections` calls it
/// directly for the implicit "local" connection (ADR 0042 L2b design B) so
/// the two processes can never derive two different paths for the same
/// machine.
pub fn session_socket_path(label: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string());
        PathBuf::from(format!(r"\\.\pipe\sot-{user}-{}", slug(label)))
    }
    #[cfg(not(windows))]
    {
        let mut p = runtime_sot_dir();
        p.push("sessions");
        p.push(format!("{}.sock", slug(label)));
        p
    }
}

/// Filesystem/pipe-name-safe slug: lowercased, `.` folded to `_` (tmux
/// silently substitutes `.` in session names, so the substituted form is
/// produced up front rather than round-tripping through a mismatch), every
/// other non-alnum/`_`/`-` run collapsed to a single `-`, trailing `-`
/// stripped, and empty input mapped to `"default"`.
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

/// Per-user runtime root for Ship of Tools Unix sockets. Unix-only in
/// effect (Windows's own `session_socket_path` branch above never calls
/// this), but left uncompiled-out so the non-Unix stub helpers below don't
/// need a matching `#[cfg]` split of their own.
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

/// `$XDG_RUNTIME_DIR` if it's set, exists, and is owner-only (no group/other
/// bits). A world/group-accessible or missing runtime dir falls through to
/// the next resolution tier rather than being trusted. Split into an env
/// read (this) + a pure path check (`is_private_dir`) so the safety logic is
/// unit-testable against real temp dirs without mutating `$XDG_RUNTIME_DIR`
/// — a process-global env var other tests in this crate touch concurrently.
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
pub fn is_private_dir(dir: &std::path::Path) -> bool {
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
pub fn is_private_dir(dir: &std::path::Path) -> bool {
    dir.is_dir()
}

/// Numeric uid for path derivation (`/run/user/<uid>`, `/tmp/sot-<uid>`) and
/// the ownership check in `is_private_dir`/`rust/backend`'s
/// `secure_private_dir`. `0` on non-Unix, where these callers are never
/// reached in practice (Windows sessions don't run tmux, and
/// `session_socket_path` takes the named-pipe branch above without calling
/// any of this) — this function exists so the module compiles everywhere,
/// not because the value is meaningful there.
#[cfg(unix)]
pub fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments, has no preconditions, and cannot
    // fail.
    unsafe { libc::getuid() }
}
#[cfg(not(unix))]
pub fn current_uid() -> u32 {
    0
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
    #[cfg(unix)]
    fn session_socket_path_honours_xdg_runtime_dir() {
        // Temporarily override XDG_RUNTIME_DIR so the test is hermetic.
        // SAFETY: no other test in this crate mutates this env var, so
        // there is nothing to race against even under a parallel test
        // runner — restored on drop regardless.
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

/// `tmux_socket_path`'s (rust/backend) tier-1 safety check, exercised
/// against real temp dirs with controlled permissions — deliberately NOT
/// via `$XDG_RUNTIME_DIR` mutation, for the same env-var-race reason noted
/// on `private_xdg_runtime_dir`'s doc comment.
#[cfg(all(test, unix))]
mod is_private_dir_tests {
    use super::is_private_dir;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir(name: &str) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "sot-protocol-session-socket-test-{}-{}-{name}",
            std::process::id(),
            n
        ))
    }

    #[test]
    fn owner_only_dir_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch_dir("owner-only");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(is_private_dir(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn group_readable_dir_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch_dir("group-readable");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(!is_private_dir(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn world_readable_dir_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
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
        use std::os::unix::fs::PermissionsExt;
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

/// Windows-only: `session_socket_path`'s named-pipe branch (ADR 0042 L2b
/// design A). Mirrors the `#[cfg(all(test, windows))]` convention
/// `rust/backend/src/paths.rs::verbatim_tests` already uses for its own
/// Windows-only path logic.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::session_socket_path;
    use std::path::PathBuf;

    #[test]
    fn session_socket_path_is_a_named_pipe_per_user_and_label() {
        // SAFETY: no other test in this crate mutates USERNAME.
        struct Guard(Option<std::ffi::OsString>);
        impl Drop for Guard {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(v) => std::env::set_var("USERNAME", v),
                    None => std::env::remove_var("USERNAME"),
                }
            }
        }
        let _g = Guard(std::env::var_os("USERNAME"));
        std::env::set_var("USERNAME", "TestUser");
        assert_eq!(
            session_socket_path("local"),
            PathBuf::from(r"\\.\pipe\sot-TestUser-local")
        );
        // The label goes through the same `slug()` every platform uses: a
        // `.` (illegal in a tmux session name, and a source of round-trip
        // mismatches elsewhere) is substituted here too, not just on Unix.
        assert_eq!(
            session_socket_path("MyPackage.jl"),
            PathBuf::from(r"\\.\pipe\sot-TestUser-mypackage_jl")
        );
    }
}
