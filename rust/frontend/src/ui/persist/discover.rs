//! Config-file discovery: the `$SOT_SETTINGS` / `$SOT_KEYBINDINGS` override, then `.sot/` up the cwd, then `$HOME/.config/sot/`.

use std::path::{Path, PathBuf};

pub(crate) fn find_settings_file() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SOT_SETTINGS") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur: &Path = &cwd;
        loop {
            let candidate = cur.join(".sot").join("settings.toml");
            if candidate.is_file() {
                return Some(candidate);
            }
            match cur.parent() {
                Some(parent) => cur = parent,
                None => break,
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home)
            .join(".config")
            .join("sot")
            .join("settings.toml");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

pub(crate) fn find_keybindings_file() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SOT_KEYBINDINGS") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    // repo-local: walk up from cwd looking for .sot/keybindings.toml.
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur: &Path = &cwd;
        loop {
            let candidate = cur.join(".sot").join("keybindings.toml");
            if candidate.is_file() {
                return Some(candidate);
            }
            match cur.parent() {
                Some(parent) => cur = parent,
                None => break,
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home)
            .join(".config")
            .join("sot")
            .join("keybindings.toml");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}
