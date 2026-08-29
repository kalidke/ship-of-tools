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
//
// ADR 0041 step 6, unit U0: the rule itself moved to `sot_log::state_dir`
// (the single owner now — a "later" process needing it, e.g. a capsule
// supervisor, must agree with the FE, not maintain a third copy). This
// function is a pure delegation: same signature, same resolved paths,
// byte-identical — `sot_log::state_dir`'s own tests pin the rule
// (including the XDG_STATE_HOME/%LOCALAPPDATA% precedence) that used to
// have no test coverage here at all.

/// Per-machine Ship of Tools state directory: `%LOCALAPPDATA%\sot\` on Windows,
/// `$XDG_STATE_HOME/sot` (or `$HOME/.local/state/sot`) elsewhere. Home for the
/// staged binary + logs (ADR 0017), the relaunch sentinel, the FE control
/// channel (ADR 0019), and session reconnect memory (state.rs).
pub(crate) fn sot_state_dir() -> Option<std::path::PathBuf> {
    sot_log::state_dir::sot_state_dir()
}
