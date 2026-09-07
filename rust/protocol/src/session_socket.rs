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

// L1-unix LU1b (ADR 0043 decision 1): `runtime_sot_dir`/`is_private_dir`/
// `current_uid` moved DOWN into `sot-log`'s `state_dir` module — the Unix
// domain-socket transport (`sot_log::socket_unix`) needs the identical
// derivation `session_socket_path`'s own Unix branch already used, and
// `sot-log` is the lower crate (this crate gains a dependency on it, never
// the reverse — `sot-log` has none on `sot-protocol`). Re-exported
// verbatim so every existing `runtime_sot_dir()` / `is_private_dir(...)` /
// `current_uid()` call site in this module (and every downstream
// `sot_protocol::{runtime_sot_dir, current_uid}` re-export in
// `rust/backend/src/paths.rs`) keeps compiling unchanged. See
// `sot_log::state_dir` for the doc comments, the tests, and the new
// `SOT_RUNTIME_DIR` propagation seam (`state_dir::runtime_dir`) this move
// exists to enable.
pub use sot_log::state_dir::{current_uid, is_private_dir, runtime_sot_dir};

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
