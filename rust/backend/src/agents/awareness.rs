// awareness.rs — the `SOT_*` awareness env stamped on every capsule
// producer the daemon owns (ADR 0046 decision 1), plus the daemon's own
// listener path that feeds `SOT_SOCKET`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// This daemon's own listener path, BARE (S4: `SOT_SOCKET`
/// keeps its pre-existing meaning, a plain Unix socket path — no typed
/// `unix:`/`pipe:` prefix; that would break every consumer still expecting
/// a bare path, e.g. the frontend/daemon CLI parsers and older comm
/// scripts that already prepend their own `unix:`). Set ONCE at boot
/// (`server::run`, before any session can be spawned) from `Opts.socket`
/// — the daemon binds exactly one listener per process, so a second
/// `set_own_endpoint` call is a no-op by construction, never a real race.
static OWN_ENDPOINT: OnceLock<String> = OnceLock::new();


pub(crate) fn set_own_endpoint(socket_path: &Path) {
    let _ = OWN_ENDPOINT.set(socket_path.display().to_string());
}

/// The `SOT_*` awareness env for a capsule producer the daemon owns:
/// `SOT_SESSION=1` ("you are inside Ship of Tools"), the owning
/// workspace's slug + project root, `SOT_SOCKET` (ADR 0046 decision 1 —
/// so `comm-join.sh`'s `agent.join` can always find its owner daemon),
/// `SOT_WORKSPACE_ID` when the caller has one, and the product checkout
/// for the help persona. No `SOT_SELF_HOST` pin here — a
/// caller that set that override on the DAEMON's own process already has
/// it reach every child this spawns through ordinary env inheritance.
pub(crate) fn awareness_env(
    slug: Option<&str>,
    cwd: Option<&Path>,
    workspace_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut env = vec![("SOT_SESSION".to_string(), "1".to_string())];
    // Unix only (S4): a bare path is meaningless as a Windows named-pipe
    // address for `agent.join`'s resolution, which already falls back to
    // the local pipe there through `sot_daemon_endpoint`'s existing
    // platform order — nothing to pin on that platform.
    if cfg!(unix) {
        if let Some(endpoint) = OWN_ENDPOINT.get() {
            env.push(("SOT_SOCKET".to_string(), endpoint.clone()));
        }
    }
    if let Some(slug) = slug {
        env.push(("SOT_WORKSPACE".to_string(), slug.to_string()));
    }
    if let Some(id) = workspace_id {
        env.push(("SOT_WORKSPACE_ID".to_string(), id.to_string()));
    }
    if let Some(dir) = cwd {
        env.push(("SOT_WORKSPACE_ROOT".to_string(), dir.to_string_lossy().into_owned()));
    }
    if let Some(root) = manual_root() {
        env.push(("SOT_MANUAL".to_string(), root.to_string_lossy().into_owned()));
    }
    env
}

/// Clone-based install (ADR 0030 addendum): the pane's agent gets pointed at
/// the product's own checkout — the repo IS the manual, and docs/USING.md is
/// its entry point for a help+extend persona. Resolved through the same chain
/// as every other resource (dev checkouts get the dev tree, installs get
/// $PREFIX/repo/current); `None` when absent. Cached: the answer can't change
/// under a running daemon, and `awareness_env` is called per session spawn +
/// per workspace in the boot sweep.
fn manual_root() -> Option<&'static Path> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let manual = sot_platform::host::paths::resource_dir("docs");
        if manual.exists() {
            manual.parent().map(Path::to_path_buf)
        } else {
            None
        }
    })
    .as_deref()
}

#[cfg(test)]
#[path = "awareness_tests.rs"]
mod tests;
