// state.rs — persisted reconnect memory.
//
// Per ADR 0010, reconnect carries `(session_id, client_id, last_seen_revision)`.
// The frontend writes those out on every revision-bumping frame so that a
// fresh process — after a kill, an SSH drop, a host reboot — can hand them
// back to the backend in the next `hello` and pick up where it left off.
//
// Tiny JSON blob under a platform-appropriate per-user state dir:
// `%LOCALAPPDATA%\sot` on Windows; `$XDG_STATE_HOME/sot` else
// `$HOME/.local/state/sot` on Unix; cwd as last resort. Atomic write through
// a temp file + rename so a crashing frontend never leaves half-written
// state.
//
// ADR 0042 L2a: one connection per host means one reconnect memory per
// host — a resume of one host's session must not clobber another's
// `last_seen_revision`. `state_path(host)` files each host's memory under
// its own name (`session-<host>.json`); the bare `session.json` name is
// reserved for nothing post-L2a (every caller now names a host).

use std::path::PathBuf;

use crate::hosts::HostKey;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMemory {
    pub session_id: Option<String>,
    pub client_id: String,
    pub last_seen_revision: u64,
}

impl SessionMemory {
    pub fn fresh() -> Self {
        Self {
            session_id: None,
            client_id: format!(
                "client-{:016x}",
                std::process::id() as u64
                    ^ std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0)
            ),
            last_seen_revision: 0,
        }
    }
}

/// Filesystem-safe rendering of a `HostKey` for use in a filename: hosts.toml
/// names are typically already `[a-zA-Z0-9_-]`, but the registry format
/// doesn't enforce that, so anything else collapses to `_` rather than
/// producing a path-separator or reserved character in a filename built
/// from user-editable config.
fn sanitize_for_filename(host: &HostKey) -> String {
    host.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn state_path(host: &HostKey) -> PathBuf {
    // One shared rule, via `crate::paths` (ADR 0041 step 1). This copy used
    // to resolve `$XDG_STATE_HOME` ahead of `%LOCALAPPDATA%`, which split
    // session state away from the relaunch sentinel on Windows whenever
    // XDG_STATE_HOME was set. The "." fallback is the pre-existing last
    // resort for when no env var resolves.
    crate::paths::sot_state_dir()
        .unwrap_or_else(|| PathBuf::from(".").join("sot"))
        .join(format!("session-{}.json", sanitize_for_filename(host)))
}

pub fn load(host: &HostKey) -> SessionMemory {
    let path = state_path(host);
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<SessionMemory>(&s) {
            Ok(mut m) => {
                // Always preserve the persisted client_id (so the backend
                // can keep multi-client policy stable across reconnects).
                if m.client_id.is_empty() {
                    m.client_id = SessionMemory::fresh().client_id;
                }
                m
            }
            Err(e) => {
                tracing::warn!(error = %e, ?path, "session memory parse failed; using fresh");
                SessionMemory::fresh()
            }
        },
        Err(_) => SessionMemory::fresh(),
    }
}

pub fn save(host: &HostKey, m: &SessionMemory) -> Result<()> {
    let path = state_path(host);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create_dir_all {parent:?}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(m).context("serialize session memory")?;
    std::fs::write(&tmp, &body).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename {tmp:?} -> {path:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_path_differs_per_host() {
        let a = state_path(&"alpha".to_string());
        let b = state_path(&"beta".to_string());
        assert_ne!(a, b, "two hosts must not share a session-memory file");
        assert!(a.to_string_lossy().contains("alpha"));
        assert!(b.to_string_lossy().contains("beta"));
    }

    #[test]
    fn state_path_is_stable_for_the_same_host() {
        let a1 = state_path(&"alpha".to_string());
        let a2 = state_path(&"alpha".to_string());
        assert_eq!(a1, a2);
    }

    #[test]
    fn state_path_sanitizes_unsafe_filename_characters() {
        let p = state_path(&"weird/host:name".to_string());
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.contains('/'));
        assert!(!name.contains(':'));
    }
}
