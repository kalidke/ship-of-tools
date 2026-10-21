// paths.rs — conventional paths for backend sessions per ADR 0013.
//
// One backend per project. Each backend listens on a per-session socket
// derived from a stable label (project name, etc.) so multiple backends
// coexist on the same host. Sessions mode in the frontend uses the same
// derivations when spawning daemons via `tmux.create_session`, which is
// why the rules need to be deterministic and documented.

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





/// `simplify_verbatim` is a Windows-only rewrite — everywhere else (this
/// crate's Linux/macOS CI leg included) it must be a true no-op, verbatim
/// prefix or not, so a Windows-formatted string passed through on a
/// non-Windows host round-trips unchanged rather than being mangled.
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
pub use sot_protocol::{local_daemon_label, session_socket_path, slug};

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
/// `state_dir()`'s platform dispatch. The Unix branch is unchanged
/// behaviour (still `$XDG_STATE_HOME` / `$HOME/.local/state/sot` /
/// `/tmp/.local/state/sot`); the precedence of `%LOCALAPPDATA%` over
/// `$XDG_STATE_HOME` on Windows is `sot_platform::host::state_dir::sot_state_dir`'s
/// own contract (tested there) — this only proves `state_dir()` appends
/// `state` beneath it and still falls back when `%LOCALAPPDATA%` is
/// unset. Env-guard shape matches `sot_platform::host::state_dir`'s own tests
/// (`state_dir.rs`) so the two stay easy to compare.
#[cfg(test)]
#[path = "paths_valid_name_tests.rs"]
mod valid_name_tests;
