// paths.rs — shared filesystem path resolution.
//
// ADR 0041 build-order step 1: ONE resolution rule for the per-machine state
// dir, replacing two copies that had drifted apart (`gpu.rs`'s
// `sot_state_dir()` checked `%LOCALAPPDATA%` only on Windows; `state.rs`'s
// `state_path()` checked `$XDG_STATE_HOME` first on *both* platforms). With
// `XDG_STATE_HOME` set on Windows that split session state from the relaunch
// sentinel into two different directories. LOCALAPPDATA wins on Windows
// because the launcher scripts (`scripts/launch-sot.ps1`,
// `scripts/relaunch-sot.ps1`) write the relaunch sentinel and staged binary
// under `%LOCALAPPDATA%\sot` unconditionally, and ADR 0041 pins the voyages
// subtree there too — so the state dir has to agree with them, not with an
// optional override.

/// Per-machine Ship of Tools state directory: `%LOCALAPPDATA%\sot\` on Windows,
/// `$XDG_STATE_HOME/sot` (or `$HOME/.local/state/sot`) elsewhere. Home for the
/// staged binary + logs (ADR 0017), the relaunch sentinel, the FE control
/// channel (ADR 0019), and session reconnect memory (state.rs).
pub(crate) fn sot_state_dir() -> Option<std::path::PathBuf> {
    let dir = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".local").join("state"))
            })
    }?;
    Some(dir.join("sot"))
}
