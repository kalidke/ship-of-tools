//! How a process names this box's own daemon: the session socket or pipe path, the local label, the slug, the
//! `unix:`/`pipe:` spelling, and the plain host-name grammar.
// endpoint.rs — the ONE derivation of a Ship of Tools daemon's
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
// domain-socket transport (`sot_log::lane::socket_unix`) needs the identical
// derivation `session_socket_path`'s own Unix branch already used, and
// `sot-log` is the lower crate (this crate gains a dependency on it, never
// the reverse — `sot-log` has none on `sot-protocol`). Re-exported
// verbatim so every existing `runtime_sot_dir()` / `is_private_dir(...)` /
// `current_uid()` call site in this module (and every downstream
// `sot_protocol::{runtime_sot_dir, current_uid}` re-export in
// `rust/backend/src/paths.rs`) keeps compiling unchanged. See
// `sot_platform::host::state_dir` for the doc comments, the tests, and the
// `SOT_RUNTIME_DIR` override (`state_dir::runtime_dir`; nothing in
// production sets it).
pub use sot_platform::host::state_dir::{current_uid, is_private_dir, runtime_sot_dir};

/// Conventional per-user session endpoint for a backend with the given
/// label: a Unix socket path, or (Windows) a named pipe path. `sotd
/// --label <name>` uses this to derive `--socket` when the flag is omitted
/// (ADR 0013); the frontend's `hosts::resolve_connections` calls it
/// directly for the implicit "local" connection (ADR 0042 L2b design B) so
/// the two processes can never derive two different paths for the same
/// machine.
pub fn session_socket_path(label: &str) -> PathBuf {
    session_socket_path_in(label, &sot_platform::host::env::Env::process())
}

/// [`session_socket_path`] reading the variables from `env`.
pub fn session_socket_path_in(label: &str, env: &sot_platform::host::env::Env) -> PathBuf {
    #[cfg(windows)]
    {
        let user = env.get("USERNAME").and_then(|u| u.to_str()).unwrap_or("user").to_string();
        PathBuf::from(format!(r"\\.\pipe\sot-{user}-{}", slug(label)))
    }
    #[cfg(not(windows))]
    {
        let mut p = sot_platform::host::state_dir::runtime_sot_dir_in(env);
        p.push("sessions");
        p.push(format!("{}.sock", slug(label)));
        p
    }
}

/// The label THIS box's own persistent daemon runs under, and the one place
/// that says so. `sot` on Unix (`deploy/sotd.service`, `scripts/install.sh`:
/// `sotd --project-root $HOME --label sot`); `local` on Windows
/// (`scripts/sot-local-daemon.ps1`'s `--label local`, which the launcher and
/// `sot-fe` both ask for by name — `sotd session-socket-path local` — rather
/// than constructing the pipe name themselves).
///
/// A caller that wants a NAMED backend passes its own label to
/// [`session_socket_path`]; a caller that means the local daemon asks here and
/// never spells the label itself, because the two platforms disagree about it.
pub fn local_daemon_label() -> &'static str {
    if cfg!(windows) {
        "local"
    } else {
        "sot"
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

/// The host-name grammar `[a-z0-9][a-z0-9._-]*`; hosts.toml, `SshRecipe::new`
/// and the window's `--dial` parser all check a host against it.
pub fn is_plain_host_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// This box's own control endpoint for its own daemon, in the `unix:`/`pipe:`
/// spelling the comm scripts and the frontend already speak. The label is
/// never a caller's: every caller here means the one local daemon, and a
/// `label` parameter they all passed `"sot"` to was how a Windows box came
/// to dial a pipe name nothing listens on. The precedence: `$SOT_SOCKET`, a
/// bare path, beats a label, as in the daemon's own arg parsing (`main.rs`);
/// else `session_socket_path($SOT_BACKEND_LABEL)` when that is set, a step
/// of a client: the daemon reads no environment variable for its label, and
/// this function and the shell's `sot_daemon_endpoint` take that step
/// (`scripts/restart-backend.sh` passes the variable to the daemon as
/// `--label`); else
/// `session_socket_path(`[`crate::local_daemon_label`]`())`, byte-for-byte
/// what this returned before the overrides existed. **Never** the literal
/// `sot` as a default — on Windows the daemon's label is `local`, and
/// `sot` derives a pipe nothing listens on.
pub fn local_endpoint() -> String {
    local_endpoint_in(&sot_platform::host::env::Env::process())
}

/// [`local_endpoint`] reading the variables from `env`.
pub fn local_endpoint_in(env: &sot_platform::host::env::Env) -> String {
    let p = env
        .get_nonempty("SOT_SOCKET")
        .map(PathBuf::from)
        .or_else(|| env.get_nonempty("SOT_BACKEND_LABEL").map(|label| session_socket_path_in(&label.to_string_lossy(), env)))
        .unwrap_or_else(|| session_socket_path_in(crate::local_daemon_label(), env));
    if cfg!(windows) { format!("pipe:{}", p.display()) } else { format!("unix:{}", p.display()) }
}

#[cfg(test)]
#[path = "endpoint_tests.rs"]
mod tests;

/// Windows-only: `session_socket_path`'s named-pipe branch (ADR 0042 L2b
/// design A). Mirrors the `#[cfg(all(test, windows))]` convention
/// `rust/backend/src/paths.rs::verbatim_tests` already uses for its own
/// Windows-only path logic.
#[cfg(all(test, windows))]
#[path = "endpoint_windows_tests.rs"]
mod windows_tests;
