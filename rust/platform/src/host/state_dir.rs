//! The per-machine Ship of Tools state directory (ADR 0041 step 6, unit
//! U0 — promoted from the frontend, build-order step 1's rule): ONE resolution rule, owned here so every process that
//! needs it shares it rather than drifting copies. That drift already
//! happened once — `ui/`'s old `sot_state_dir()` checked
//! `%LOCALAPPDATA%` only on Windows while `state.rs`'s old `state_path()`
//! checked `$XDG_STATE_HOME` first on *both* platforms, so a Windows box
//! with `XDG_STATE_HOME` set split session state from the relaunch
//! sentinel into two different directories.
//!
//! `%LOCALAPPDATA%\sot` wins on Windows because the launcher scripts
//! (`scripts/launch-sot.ps1`, `scripts/relaunch-sot.ps1`) write the
//! relaunch sentinel and staged binary under `%LOCALAPPDATA%\sot`
//! unconditionally, and this crate's own Windows-side voyage/supervisor
//! state (ADR 0041) is pinned to the same subtree — so a second env var
//! winning on Windows would let the FE's resume state and the capsule's
//! durable state disagree about which machine-local directory is
//! authoritative.
//!
//! Pure refactor: this is a byte-for-byte port of the frontend's own
//! resolution logic (no behavior change); the window calls it directly.

/// Resolve the per-user CONFIG directory: `%LOCALAPPDATA%\sot\config` on
/// Windows (a sibling of [`sot_state_dir`]'s `state` under the same root —
/// no `$HOME`-shaped fallback there, same reasoning as below), else
/// `$XDG_CONFIG_HOME/sot` or `$HOME/.config/sot`. Home for `hosts.toml`
/// (`sot-protocol`'s `topology`), the workspace/session registries and the
/// frontend's own settings. `None` when no base env var is set.
pub fn sot_config_dir() -> Option<std::path::PathBuf> {
    sot_config_dir_in(&super::env::Env::process())
}

/// [`sot_config_dir`] reading the variables from `env`.
pub fn sot_config_dir_in(env: &super::env::Env) -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        return sot_state_dir_in(env).map(|d| d.join("config"));
    }
    env.get_nonempty("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| env.get("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .map(|d| d.join("sot"))
}

/// The folder the daemon keeps its logs, its lease and its rows' capsule state in: the state folder itself on Unix, its
/// `state` subfolder under `%LOCALAPPDATA%\sot` on Windows (a sibling of `config`, so neither collides with a capsule's
/// `workspaces\<id>` subtree). Windows panics when no base variable is set, because a `$HOME`-shaped guess there depends
/// on which shell launched the daemon; Unix falls back to `/tmp/.local/state/sot` for a login with neither
/// `$XDG_STATE_HOME` nor `$HOME`.
pub fn daemon_state_dir() -> std::path::PathBuf {
    daemon_state_dir_in(&super::env::Env::process())
}

/// [`daemon_state_dir`] reading the variables from `env`.
pub fn daemon_state_dir_in(env: &super::env::Env) -> std::path::PathBuf {
    let root = sot_state_dir_in(env);
    if cfg!(windows) {
        return root
            .unwrap_or_else(|| {
                panic!(
                    "cannot resolve the Windows state root: neither %LOCALAPPDATA% nor \
                     %USERPROFILE% is set — sotd cannot start without one"
                )
            })
            .join("state");
    }
    root.unwrap_or_else(|| std::path::PathBuf::from("/tmp/.local/state/sot"))
}

/// Resolve the per-machine state directory: `%LOCALAPPDATA%\sot` (falling
/// back to `%USERPROFILE%\AppData\Local\sot` — the same location
/// `%LOCALAPPDATA%` names, derived directly for the rare login where the
/// env var itself is unset or empty) on Windows, `$XDG_STATE_HOME/sot` (or
/// `$HOME/.local/state/sot`) elsewhere. Home for the staged binary + logs
/// (ADR 0017), the relaunch sentinel, the FE control channel (ADR 0019),
/// session reconnect memory, and — from ADR 0041 on — `drawer.voyage`/
/// `supervisor.lock` and the capsule's own durable state.
///
/// No POSIX (`XDG_*`/`HOME`/`/tmp`) fallback exists on Windows — deriving
/// a `$HOME`-shaped path there depends on which shell launched the
/// process (a git-bash shell exports `HOME`, PowerShell doesn't), which
/// is exactly the class of bug this whole resolver exists to close (see
/// this module's own doc for the FE/capsule drift it already caused
/// once). `None` on Windows only when NEITHER `%LOCALAPPDATA%` nor
/// `%USERPROFILE%` is set — not a normal Windows login; callers there
/// should treat it as a hard failure (see e.g.
/// `sot-backend`'s `paths::windows_state_root`), not a silent fallback.
/// `None` elsewhere only when neither `$XDG_STATE_HOME` nor `$HOME` is
/// set.
pub fn sot_state_dir() -> Option<std::path::PathBuf> {
    sot_state_dir_in(&super::env::Env::process())
}

/// [`sot_state_dir`] reading the variables from `env`.
pub fn sot_state_dir_in(env: &super::env::Env) -> Option<std::path::PathBuf> {
    let dir = if cfg!(windows) {
        env.get_nonempty("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                env.get_nonempty("USERPROFILE")
                    .map(|home| std::path::PathBuf::from(home).join("AppData").join("Local"))
            })
    } else {
        env.get("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| env.get("HOME").map(|h| std::path::PathBuf::from(h).join(".local").join("state")))
    }?;
    Some(dir.join("sot"))
}

/// The declared identity's `host` component (ADR 0046 decision 1): one
/// resolver, replacing the five independent guesses that used to disagree
/// (`ui/`'s per-call mint, the backend's own `gethostname` call, this
/// crate's old `state_host()`, the comm digest, and the frontend's `hosts.rs`
/// label) — every one of them fixed a wrong answer with another resolver
/// tier or override env instead of declaring the fact once.
///
/// `SOT_SELF_HOST` wins when set and non-empty, taken verbatim (an
/// explicit override needs no further massaging) — a NEW variable, not
/// `SOT_HOST`: the launchers already use `SOT_HOST` for the SSH target a
/// remote frontend dials (`scripts/launch-sot.ps1`, `launch-sot.sh`), and
/// this declaration is a different fact (who a process IS, not where it
/// dials). Otherwise `gethostname`'s first `.`-delimited label, lowercased.
/// Also names the daemon's per-host state dirs (`workspaces-<host>`), since
/// topology plan step 8 folded the daemon's second resolver into this one.
///
/// `Err` — never a fallback string — when neither source yields a usable
/// label: a process with no nameable host has nothing honest to declare.
pub fn host_name() -> Result<String, String> {
    host_name_in(&super::env::Env::process())
}

/// [`host_name`] reading `SOT_SELF_HOST` from `env`.
pub fn host_name_in(env: &super::env::Env) -> Result<String, String> {
    if let Some(v) = env.get("SOT_SELF_HOST") {
        let v = v.to_string_lossy().into_owned();
        if !v.is_empty() {
            return Ok(v);
        }
    }
    kernel_host_name().map_err(|e| format!("host_name: no SOT_SELF_HOST override and {e}"))
}

/// This box's name as the kernel gives it, which is all a program outside Ship of Tools sees: `gethostname`'s first
/// `.`-delimited label, lowercased. [`host_name`] without its `SOT_SELF_HOST` override; the host pin that systemd's
/// `ConditionHost=` compares names a box this way. `Err` when the kernel gives no usable label.
pub fn kernel_host_name() -> Result<String, String> {
    host_label(&gethostname::gethostname().to_string_lossy())
}

/// The name a kernel's raw host name gives: its first `.`-delimited label, trimmed and lowercased; `Err` when none is left.
fn host_label(raw: &str) -> Result<String, String> {
    let label = raw.split('.').next().unwrap_or("").trim().to_lowercase();
    if label.is_empty() {
        return Err("gethostname returned no usable label".to_string());
    }
    Ok(label)
}

/// L1-unix LU3b: moved here from `supervisor/` (a pure hash of a path,
/// with no OS-facing mechanism of its own) — the supervisor lane's own
/// per-state-dir pipe/socket name, shared by every caller that connects
/// to it: `attach_client::client`'s worker, `supervisor_client`'s `connect`, the
/// authority's own `PipeServer::bind_supervisor`/`SocketServer::
/// bind_supervisor` call in `supervisor/`, and both platforms' real-
/// process integration tests. Stable for the SAME path (canonicalized
/// first, so a symlinked or relative `state_dir` still names the same
/// lane), different for different paths — proven by this module's own
/// tests below.
pub fn state_dir_hash(state_dir: &std::path::Path) -> String {
    use sha2::{Digest as _, Sha256};
    let canonical = std::fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
#[path = "state_dir_hash_tests.rs"]
mod state_dir_hash_tests;

// ---------------------------------------------------------------------
// L1-unix LU1b (ADR 0043 decision 1): the per-user RUNTIME dir — where
// live sockets/pipes go, as opposed to `sot_state_dir`'s durable-state
// tree above. Moved DOWN here from `sot-protocol`'s `topology::endpoint`
// module (ADR 0042 L2b's original home): the daemon's own session
// sockets and the Ship's Log Unix transport (`lane/socket_unix/`) both need
// the identical derivation, and `sot-log` is the lower crate in the
// dependency graph (the frontend and backend already depend on both;
// `sot-protocol` gains a dependency on `sot-log`, never the reverse).
// `topology/endpoint.rs` now re-exports these three names verbatim so every
// existing `sot_protocol::runtime_sot_dir()` / `topology::endpoint::
// is_private_dir(...)` / `current_uid()` call site keeps compiling
// unchanged.
// ---------------------------------------------------------------------

use std::path::PathBuf;

/// Per-user runtime root for Ship of Tools Unix sockets. Unix-only in
/// effect (Windows's own `session_socket_path` branch never calls this),
/// but left uncompiled-out so the non-Unix stub helpers below don't need
/// a matching `#[cfg]` split of their own.
///
/// NOT a pure function of (uid): it consults `$XDG_RUNTIME_DIR` and
/// filesystem state, so two same-uid processes with different
/// environments can disagree. [`runtime_dir`] is the validated
/// `SOT_RUNTIME_DIR` override (the capsule lane sockets read it); no
/// production code sets that variable, so each process resolves its own.
pub fn runtime_sot_dir() -> PathBuf {
    runtime_sot_dir_in(&super::env::Env::process())
}

/// [`runtime_sot_dir`] reading `XDG_RUNTIME_DIR` from `env`.
pub fn runtime_sot_dir_in(env: &super::env::Env) -> PathBuf {
    if let Some(dir) = private_xdg_runtime_dir(env) {
        return dir.join("sot");
    }
    let uid = current_uid();
    let run_user_dir = PathBuf::from(format!("/run/user/{uid}"));
    if is_private_dir(&run_user_dir) {
        return run_user_dir.join("sot");
    }
    PathBuf::from(format!("/tmp/sot-{uid}"))
}

/// L1-unix LU1b (ADR 0043 decision 1): the override seam. A set `SOT_RUNTIME_DIR`
/// pins the runtime dir for every process that reads it, so they cannot disagree
/// about `$XDG_RUNTIME_DIR` the way two independently-launched processes' own
/// [`runtime_sot_dir`] discovery could. No production code sets it today: the
/// daemon does not export it to the capsules and clients it spawns, so tests and
/// an operator are its only writers. A set `SOT_RUNTIME_DIR` is trusted only after
/// it is ABSOLUTE (a relative one would resolve against whatever the CALLER's own
/// current directory happens to be at the moment, a silent source of
/// disagreement) and passes the SAME [`is_private_dir`] check `runtime_sot_dir`'s
/// own discovery applies to its candidates: a stale or maliciously-set var
/// pointing at a group/world-accessible or symlinked directory is a loud, named
/// error, never a silent fallback to discovery (which would let a mismatched env
/// var produce a silent SECOND endpoint no caller intended). Without it,
/// discovery ([`runtime_sot_dir`]) answers.
pub fn runtime_dir() -> std::io::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("SOT_RUNTIME_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "SOT_RUNTIME_DIR must be an absolute path",
            ));
        }
        return if is_private_dir(&dir) {
            Ok(dir)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "SOT_RUNTIME_DIR={} is not a private, owner-only directory",
                    dir.display()
                ),
            ))
        };
    }
    Ok(runtime_sot_dir())
}

/// `$XDG_RUNTIME_DIR` if it's set, exists, and is owner-only (no group/other
/// bits). A world/group-accessible or missing runtime dir falls through to
/// the next resolution tier rather than being trusted. Split into an env
/// read (this) + a pure path check ([`is_private_dir`]) so the safety logic
/// is unit-testable against real temp dirs without mutating
/// `$XDG_RUNTIME_DIR` — a process-global env var other tests in this crate
/// touch concurrently.
#[cfg(unix)]
fn private_xdg_runtime_dir(env: &super::env::Env) -> Option<PathBuf> {
    let dir = PathBuf::from(env.get("XDG_RUNTIME_DIR")?);
    if is_private_dir(&dir) {
        Some(dir)
    } else {
        None
    }
}
#[cfg(not(unix))]
fn private_xdg_runtime_dir(env: &super::env::Env) -> Option<PathBuf> {
    env.get("XDG_RUNTIME_DIR").map(PathBuf::from)
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
/// `session_socket_path` takes the named-pipe branch without calling any
/// of this) — this function exists so the module compiles everywhere, not
/// because the value is meaningful there.
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
#[path = "state_dir_tests.rs"]
mod tests;

/// Moved verbatim from `sot-protocol`'s `topology::endpoint` module (ADR 0043
/// decision 1, L1-unix LU1b) with `is_private_dir` itself — deliberately
/// NOT via `$XDG_RUNTIME_DIR` mutation, for the same env-var-race reason
/// noted on `private_xdg_runtime_dir`'s doc comment.
#[cfg(all(test, unix))]
#[path = "state_dir_is_private_dir_tests.rs"]
mod is_private_dir_tests;

/// L1-unix LU1b: [`runtime_dir`]'s own propagation-seam contract — a set
/// `SOT_RUNTIME_DIR` wins over discovery only once it passes
/// `is_private_dir`, and a failing one is a loud, named `PermissionDenied`,
/// never a silent fallback. Serialized against a private mutex (not
/// `tests::SERIAL`, a different module's private static) for the same
/// concurrent-env-var-mutation reason every other env-touching test module
/// in this crate uses one.
#[cfg(all(test, unix))]
#[path = "state_dir_runtime_dir_tests.rs"]
mod runtime_dir_tests;
