// state.rs — persisted reconnect memory.
//
// Per ADR 0010, reconnect carries `(session_id, client_id, last_seen_revision)`.
// The frontend writes those out on every revision-bumping frame so that a
// fresh process — after a kill, an SSH drop, a host reboot — can hand them
// back to the backend in the next `hello` and pick up where it left off.
//
// Tiny JSON blob under a platform-appropriate per-user state dir
// (`$XDG_STATE_HOME` if set, else `%LOCALAPPDATA%` on Windows, else
// `$HOME/.local/state` on Unix, else cwd). Atomic write through a temp file +
// rename so a crashing frontend never leaves half-written state.

use std::path::PathBuf;

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

pub fn state_path() -> PathBuf {
    // Precedence:
    //   1. $XDG_STATE_HOME — explicit user override on either OS.
    //   2. %LOCALAPPDATA%  — Windows per-user app-data root. Only set on
    //                        Windows, so adding it before HOME is harmless on
    //                        Unix (the var doesn't exist there).
    //   3. $HOME/.local/state — XDG default on Unix.
    //   4. "."             — last resort; previously state landed in cwd on
    //                        Windows because neither XDG_STATE_HOME nor HOME
    //                        was set.
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".local/state");
                p
            })
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let mut p = base.join("sot");
    p.push("session.json");
    p
}

pub fn load() -> SessionMemory {
    let path = state_path();
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

pub fn save(m: &SessionMemory) -> Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {parent:?}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(m).context("serialize session memory")?;
    std::fs::write(&tmp, &body).with_context(|| format!("write {tmp:?}"))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename {tmp:?} -> {path:?}"))?;
    Ok(())
}
