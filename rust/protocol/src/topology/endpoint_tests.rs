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

/// [`local_daemon_label`]'s Windows arm is a claim about a file no Rust
/// caller can ask: `scripts/sot-local-daemon.ps1` starts the box's own
/// daemon with `--label local`. Pin the two sides together here, because
/// no Unix CI leg can execute that arm — and the two drifting apart IS
/// the bug this resolver closed (`sotd status` dialing
/// `sot-<user>-sot` while the daemon listened on `sot-<user>-local`).
///
/// Checked against the `$daemonArgLine` ASSIGNMENT only, not the whole
/// file: `--label local` also appears in a comment elsewhere in the
/// script, and a whole-file `contains` would stay green even if the
/// assignment itself lost the flag or was deleted outright.
#[test]
fn the_windows_launcher_uses_the_label_this_module_derives() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/sot-local-daemon.ps1");
    let text =
        std::fs::read_to_string(&script).expect("the local-daemon launcher is in the tree");
    let assignment = text
        .lines()
        // The literal `$daemonArgLine =` (assignment), not merely a line
        // mentioning the variable — `-ArgumentList $daemonArgLine` further
        // down uses it without assigning it, and must not count.
        .find(|line| line.contains("$daemonArgLine ="))
        .unwrap_or_else(|| {
            panic!(
                "{} no longer assigns $daemonArgLine; local_daemon_label()'s Windows arm has nothing to check against",
                script.display()
            )
        });
    assert!(
        assignment.contains("--label local"),
        "{} no longer starts the daemon with `--label local`; local_daemon_label()'s Windows arm must follow it\n  found: {assignment}",
        script.display()
    );
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
