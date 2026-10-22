// layer.rs — the Claude layer: the skills and the work-state hooks a leg of the launch recipe sees and a plain `claude` does not.

use std::path::{Path, PathBuf};

/// The layer's folder under the comm home, written by the installer (`ShipTools.update_comm`) and read by the recipe.
pub(super) const LAYER_DIR: &str = "claude-layer";

/// The hooks a leg carries: (Claude Code event, script in `<comm home>/bin`, tool matcher).
pub(super) const HOOKS: &[(&str, &str, Option<&str>)] = &[];

/// The layer under `comm_home`, when it holds a skills folder.
pub(super) fn installed(_comm_home: &Path) -> Option<PathBuf> {
    None
}

/// The one-token `--add-dir` flag for `layer`.
pub(super) fn add_dir_arg(_layer: &Path) -> String {
    String::new()
}

/// The `{"hooks": ...}` settings object for the hooks, each running its script under `comm_home` by a quoted absolute path.
pub(super) fn hooks_settings(_comm_home: &Path) -> serde_json::Value {
    serde_json::Value::Null
}

/// The one `--settings` JSON: the auto-memory object and the hooks object, merged; `None` when both are.
pub(super) fn merged_settings(_memory: Option<String>, _hooks: Option<serde_json::Value>) -> Option<String> {
    None
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod tests;
