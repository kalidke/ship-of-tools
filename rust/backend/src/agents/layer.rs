// layer.rs — the Claude layer: the skills and the work-state hooks a leg of the launch recipe sees and a plain `claude` does not.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// The layer's folder under the comm home, written by the installer (`ShipTools.update_comm`) and read by the recipe.
pub(super) const LAYER_DIR: &str = "claude-layer";

/// The hooks a leg carries: (Claude Code event, script in `<comm home>/bin`, tool matcher). Four are the work-state
/// facts a row is reduced from; `comm-subagent-guard.sh` denies a subagent's Bash call that names a comm script.
pub(super) const HOOKS: &[(&str, &str, Option<&str>)] = &[
    ("UserPromptSubmit", "comm-status-working.sh", None),
    ("PreToolUse", "comm-status-blocked.sh", Some("AskUserQuestion")),
    ("PreToolUse", "comm-subagent-guard.sh", Some("Bash")),
    ("Stop", "comm-status-idle.sh", None),
    ("PostToolUse", "comm-status-heartbeat.sh", None),
];

/// The layer under `comm_home`, when it holds a skills folder: Claude Code loads `.claude/skills/` of an added folder.
pub(super) fn installed(comm_home: &Path) -> Option<PathBuf> {
    let layer = comm_home.join(LAYER_DIR);
    layer.join(".claude").join("skills").is_dir().then_some(layer)
}

/// The one-token `--add-dir` flag for `layer`: the flag is variadic, and only the `=` form leaves the next argument
/// (the prompt, when nothing else follows) alone.
pub(super) fn add_dir_arg(layer: &Path) -> String {
    format!("--add-dir={}", layer.display())
}

/// A hook's command: its script under `comm_home` by absolute path, in single quotes so a path with a space or a quote
/// reaches the shell whole. Forward slashes, which Git Bash on Windows reads.
fn hook_command(comm_home: &Path, script: &str) -> String {
    let home = comm_home.to_string_lossy().replace('\\', "/");
    let path = format!("{}/bin/{script}", home.trim_end_matches('/'));
    format!("'{}'", path.replace('\'', r"'\''"))
}

/// The `{"hooks": ...}` settings object for [`HOOKS`]: one group per entry, grouped by event in table order.
pub(super) fn hooks_settings(comm_home: &Path) -> Value {
    let mut events: Map<String, Value> = Map::new();
    for (event, script, matcher) in HOOKS {
        let mut group = Map::new();
        if let Some(matcher) = matcher {
            group.insert("matcher".into(), json!(matcher));
        }
        group.insert("hooks".into(), json!([{"type": "command", "command": hook_command(comm_home, script)}]));
        if let Value::Array(groups) = events.entry(event.to_string()).or_insert_with(|| json!([])) {
            groups.push(Value::Object(group));
        }
    }
    json!({ "hooks": events })
}

/// The one `--settings` JSON: the auto-memory object and the hooks object, merged; `None` when both are.
pub(super) fn merged_settings(memory: Option<String>, hooks: Option<Value>) -> Option<String> {
    let Some(hooks) = hooks else {
        return memory;
    };
    let Some(memory) = memory else {
        return Some(hooks.to_string());
    };
    match serde_json::from_str::<Map<String, Value>>(&memory) {
        Ok(mut object) => {
            object.extend(hooks.as_object().cloned().unwrap_or_default());
            Some(Value::Object(object).to_string())
        }
        Err(error) => {
            tracing::warn!(%error, "layer: the auto-memory settings are not an object; the leg carries them without the hooks");
            Some(memory)
        }
    }
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod tests;
