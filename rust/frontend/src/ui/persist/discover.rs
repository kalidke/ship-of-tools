//! Config-file discovery: the `$SOT_SETTINGS` / `$SOT_KEYBINDINGS` override, then `.sot/` up the cwd, then platform's
//! per-user config folder (`sot_config_dir`: `$XDG_CONFIG_HOME/sot`, `$HOME/.config/sot`, `%LOCALAPPDATA%\sot\config`).

use std::path::{Path, PathBuf};

use sot_platform::host::env::Env;
use sot_platform::host::state_dir::sot_config_dir_in;

pub(crate) fn find_config_file(env_var: &str, file_name: &str) -> Option<PathBuf> {
    find_config_file_in(&Env::process(), std::env::current_dir().ok().as_deref(), env_var, file_name)
}

/// [`find_config_file`] reading the variables from `env` and walking up from `cwd`.
pub(crate) fn find_config_file_in(
    env: &Env,
    cwd: Option<&Path>,
    env_var: &str,
    file_name: &str,
) -> Option<PathBuf> {
    if let Some(p) = env.get_nonempty(env_var) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    // repo-local: walk up from cwd looking for .sot/<file_name>.
    let mut cur = cwd;
    while let Some(dir) = cur {
        let candidate = dir.join(".sot").join(file_name);
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = dir.parent();
    }
    sot_config_dir_in(env).map(|d| d.join(file_name)).filter(|p| p.is_file())
}

#[cfg(test)]
#[path = "discover_tests.rs"]
mod tests;
