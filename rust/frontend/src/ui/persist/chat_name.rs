//! The chat name the Chat drawer asks for once: checked, then written as `[chat] name` into the settings file the window
//! loaded (or the per-user one in platform's config folder), every other byte of the file kept.

use std::path::{Path, PathBuf};

use sot_platform::host::write_durable;
use sot_platform::host::env::Env;
use sot_platform::host::state_dir::sot_config_dir_in;
use sot_protocol::chat::MAX_NAME;

use super::settings::{strip_comment, Settings};

/// The name as a post carries it, or the words for why not. What the daemon refuses (`chat::api::post`) is a name that is
/// empty, holds a control character, is not trimmed, or is past `MAX_NAME` bytes; a quote or a backslash is refused here
/// as well, so the written string reads the same in the window's parser (which does not unescape) and the daemon's.
pub(in crate::ui) fn check_chat_name(typed: &str) -> Result<String, String> {
    let name = typed.trim();
    if name.is_empty() {
        return Err("name is empty: type one or press Escape".into());
    }
    if name.chars().any(char::is_control) {
        return Err("name holds a control character".into());
    }
    if name.contains(['"', '\\']) {
        return Err("name cannot hold a quote or a backslash".into());
    }
    if name.len() > MAX_NAME {
        return Err(format!("name is longer than {MAX_NAME} bytes"));
    }
    Ok(name.to_string())
}

/// `contents` with `[chat] name` set to `name` (already checked): an existing line is replaced in place, else the key
/// goes under an existing `[chat]` header, else a `[chat]` table ends the file. Nothing else changes.
pub(super) fn with_chat_name(contents: &str, name: &str) -> String {
    let line = format!("name = \"{name}\"");
    let lines: Vec<&str> = contents.split_inclusive('\n').collect();
    let (mut in_chat, mut header, mut key) = (false, None, None);
    for (i, raw) in lines.iter().enumerate() {
        let text = strip_comment(raw).trim();
        if let Some(inner) = text.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_chat = inner.trim() == "chat";
            if in_chat && header.is_none() {
                header = Some(i);
            }
        } else if in_chat && key.is_none() && text.split_once('=').is_some_and(|(k, _)| k.trim() == "name") {
            key = Some(i);
        }
    }
    let eol = |raw: &str| raw[raw.trim_end_matches(['\r', '\n']).len()..].to_string();
    // What follows the replaced value on its line: its comment, with the space before it, and the line's end.
    let after = |raw: &str| {
        let value = strip_comment(raw);
        if value.len() == raw.len() { eol(raw) } else { format!("{}{}", &value[value.trim_end().len()..], &raw[value.len()..]) }
    };
    let mut out = String::with_capacity(contents.len() + line.len() + 16);
    for (i, raw) in lines.iter().enumerate() {
        if Some(i) == key {
            out.push_str(&line);
            out.push_str(&after(raw));
        } else {
            out.push_str(raw);
        }
        // A header with no key under it, and a header that is the file's last line with no newline.
        if Some(i) == header && key.is_none() {
            if eol(raw).is_empty() {
                out.push('\n');
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    if header.is_none() {
        if !out.is_empty() {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str(&format!("[chat]\n{line}\n"));
    }
    out
}

/// The file `[chat] name` is written to: the one the window loaded, else the per-user one in platform's config folder.
fn target(settings: &Settings, env: &Env) -> Result<PathBuf, String> {
    match &settings.source {
        Some(p) => Ok(p.clone()),
        None => sot_config_dir_in(env).map(|d| d.join("settings.toml")).ok_or_else(|| "no config folder to keep it in".to_string()),
    }
}

/// Writes `name` (checked here) to the settings file whole and durably and sets it in `settings`; the file's path, or
/// the words for a failure, which leaves the file and `settings` as they were.
pub(in crate::ui) fn save_chat_name(settings: &mut Settings, typed: &str, env: &Env) -> Result<PathBuf, String> {
    let name = check_chat_name(typed)?;
    let path = target(settings, env)?;
    let write = |path: &Path| -> std::io::Result<()> {
        let old = match std::fs::read(path) {
            Ok(b) => String::from_utf8(b).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "the file is not UTF-8"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_durable(path, with_chat_name(&old, &name).as_bytes())
    };
    write(&path).map_err(|e| format!("name not saved to {}: {e}", path.display()))?;
    settings.chat_name = Some(name);
    Ok(path)
}

#[cfg(test)]
#[path = "chat_name_tests.rs"]
mod tests;
