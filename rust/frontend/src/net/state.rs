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
// its own name (`session-<host>.json`); every caller now names a host.
// The bare `session.json` name (pre-L2a: the ONE connection's file) is
// read-only now, and only as a one-time migration source (codex review
// item H) — see `load`'s `legacy_state_path` fallback.

use std::path::PathBuf;

use crate::net::dial::HostKey;

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

/// Filesystem-safe rendering of a `HostKey` for use in a filename: a
/// `--dial` host name is typically already `[a-z0-9._-]`, but nothing
/// enforces that here, so anything else collapses to `_` rather than
/// producing a path-separator or reserved character in a filename built
/// from a CLI argument.
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
    // One shared rule, via sot-log's `sot_state_dir` (ADR 0041 step 1). This copy used
    // to resolve `$XDG_STATE_HOME` ahead of `%LOCALAPPDATA%`, which split
    // session state away from the relaunch sentinel on Windows whenever
    // XDG_STATE_HOME was set. The "." fallback is the pre-existing last
    // resort for when no env var resolves.
    sot_log::host::state_dir::sot_state_dir()
        .unwrap_or_else(|| PathBuf::from(".").join("sot"))
        .join(format!("session-{}.json", sanitize_for_filename(host)))
}

/// The bare pre-L2a filename, same directory rule as `state_path`. Every
/// launch before ADR 0042 L2a made exactly one connection, so this file
/// held that connection's ENTIRE reconnect memory — there was no host
/// concept to suffix it with.
fn legacy_state_path() -> PathBuf {
    sot_log::host::state_dir::sot_state_dir()
        .unwrap_or_else(|| PathBuf::from(".").join("sot"))
        .join("session.json")
}

/// Whether `host` is the one a pre-L2a install's single connection would
/// have been — the ONLY host eligible to adopt the legacy file (every
/// other host is a NEW L2a addition with no prior connection to have
/// reconnect memory for in the first place). Since lane D (topology
/// plan/`--dial`), the frontend reads no config file at all, so there is
/// no more configured `default_host` to mirror — `"local"` is the one
/// `dial::resolve_connections` names for a CLI-only (`--socket`/`--tcp`,
/// no `--dial`) invocation, matching the pre-L2a single-connection shape
/// this migration exists for.
fn is_the_pre_l2a_default_host(host: &HostKey) -> bool {
    host == "local"
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
        // ADR 0042 L2a codex review, item H: one-time migration. No
        // `session-<host>.json` for this host yet -- if this is the
        // pre-L2a default connection AND its old bare `session.json`
        // still exists, adopt it rather than minting a fresh
        // client_id/session_id, so an upgrade doesn't lose reconnect
        // continuity on the one connection that already existed. A
        // brand-new L2a host (not the pre-L2a default) has no legacy
        // file to adopt from and always starts fresh, same as before.
        Err(_) => {
            if is_the_pre_l2a_default_host(host) {
                if let Ok(s) = std::fs::read_to_string(legacy_state_path()) {
                    if let Ok(m) = serde_json::from_str::<SessionMemory>(&s) {
                        tracing::info!(%host,
                            "adopted legacy session.json for the default host (ADR 0042 L2a migration)");
                        return m;
                    }
                }
            }
            SessionMemory::fresh()
        }
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

/// Coalesces `crate::net::state::save` calls behind a `rev`-bearing frame so a
/// burst of replies doesn't turn into a burst of synchronous disk writes.
/// `crate::net::state::save` does a blocking `write`+`rename`; calling it inline
/// from EVERY `rev`-bearing frame (near enough all of them — see `Frame`'s
/// own doc on `rev`) is what stalled the transport task's read future for
/// the length of a reply burst in the field (2026-09-08: a workspace-switch
/// storm of ~2s-each `workspace.list` replies queued 50+ synchronous writes
/// onto this one async task, long enough that the daemon's own write side
/// gave up: "frame write exceeded 10s; dropping connection (peer not
/// draining)"). Throttling to at most one write per `MIN_INTERVAL` loses
/// nothing durability-sensitive: `SessionState`'s `Drop` flushes whatever
/// this held back the moment the connection ends (see `flush_revision`), so
/// only a hard crash (not a clean disconnect/reconnect) can ever leave the
/// on-disk value behind the truly-seen one — and even then, ADR 0010's
/// replay-from-`last_seen_revision` already tolerates that gap by design.
pub(super) struct StateSaveGate {
    last_saved_at: Option<std::time::Instant>,
    /// The revision the last actual disk write covered. Lets
    /// `flush_revision` tell whether the throttle is currently holding back
    /// something newer than what's on disk.
    last_saved_revision: u64,
}

impl StateSaveGate {
    /// A fresh connection's early frames (hello / first tree.root / first
    /// preview.get) still save immediately (`last_saved_at` starts `None`,
    /// always due) — only a later BURST within this window of the previous
    /// write gets coalesced.
    const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

    pub(super) fn new() -> Self {
        Self {
            last_saved_at: None,
            last_saved_revision: 0,
        }
    }

    fn due(&self, now: std::time::Instant) -> bool {
        match self.last_saved_at {
            None => true,
            Some(t) => now.duration_since(t) >= Self::MIN_INTERVAL,
        }
    }

    fn mark_saved(&mut self, now: std::time::Instant, revision: u64) {
        self.last_saved_at = Some(now);
        self.last_saved_revision = revision;
    }

    /// Whether `revision` is newer than what the last actual disk write
    /// covered — the question `flush_revision` asks at end-of-connection.
    fn is_stale(&self, revision: u64) -> bool {
        revision > self.last_saved_revision
    }
}

/// Bumps `memory.last_seen_revision` from a frame's `rev` (if present) and,
/// when `gate` says it's due, persists it. Shared by the connect-time
/// preamble (tree.root, preview.get) and the steady-state loop so there is
/// exactly one place that decides when a `rev`-bearing frame reaches disk.
pub(super) fn note_revision(
    rev: Option<u64>,
    memory: &mut crate::net::state::SessionMemory,
    host: &HostKey,
    gate: &mut StateSaveGate,
) {
    note_revision_with(rev, memory, host, gate, |h, m| {
        crate::net::state::save(h, m).ok();
    });
}

/// `note_revision`'s real logic, with the persist step as a parameter so a
/// test can substitute a counting/slow stand-in for `crate::net::state::save`
/// without touching a real state file — see the `note_revision_with` tests
/// below for the burst-of-50 measurement this fix was asked to prove.
fn note_revision_with(
    rev: Option<u64>,
    memory: &mut crate::net::state::SessionMemory,
    host: &HostKey,
    gate: &mut StateSaveGate,
    persist: impl FnOnce(&HostKey, &crate::net::state::SessionMemory),
) {
    let Some(r) = rev else { return };
    memory.last_seen_revision = memory.last_seen_revision.max(r);
    let now = std::time::Instant::now();
    if gate.due(now) {
        persist(host, memory);
        gate.mark_saved(now, memory.last_seen_revision);
    }
}

/// The flush half of `StateSaveGate`'s throttle — NOT a timer, a one-shot
/// called exactly once when the transport task ends (`SessionState`'s
/// `Drop`, below). Persists `memory` if `gate` says the last actual disk
/// write is behind the in-memory revision, so a `rev` that landed just
/// inside the `MIN_INTERVAL` coalescing window is never lost to the
/// connection ending before the next periodic write would have happened.
fn flush_revision(memory: &crate::net::state::SessionMemory, host: &HostKey, gate: &mut StateSaveGate) {
    flush_revision_with(memory, host, gate, |h, m| {
        crate::net::state::save(h, m).ok();
    });
}

/// `flush_revision`'s real logic, with the persist step as a parameter —
/// same testability shape as `note_revision`/`note_revision_with`.
fn flush_revision_with(
    memory: &crate::net::state::SessionMemory,
    host: &HostKey,
    gate: &mut StateSaveGate,
    persist: impl FnOnce(&HostKey, &crate::net::state::SessionMemory),
) {
    if gate.is_stale(memory.last_seen_revision) {
        persist(host, memory);
        gate.mark_saved(std::time::Instant::now(), memory.last_seen_revision);
    }
}

/// Owns one connection's reconnect-memory bookkeeping (`SessionMemory` +
/// its `StateSaveGate`) and flushes it exactly once when the connection
/// ends. `run_protocol` has many exit paths — EOF, a write/parse error, a
/// clean return, the drain loop that follows the outgoing channel closing —
/// but every one of them ends the function, which drops `session`, which
/// flushes: the same "flush on any exit" idiom `PendingGuard` already uses
/// in this file for the same reason.
pub(super) struct SessionState {
    pub(super) host: HostKey,
    pub(super) memory: crate::net::state::SessionMemory,
    pub(super) gate: StateSaveGate,
}

impl Drop for SessionState {
    fn drop(&mut self) {
        flush_revision(&self.memory, &self.host, &mut self.gate);
    }
}

/// Process-wide serial lock plus a temp `XDG_STATE_HOME` (and, on Windows,
/// `LOCALAPPDATA` and `USERPROFILE`), for tests that reach `state_path` and
/// must not touch the real state dir.
#[cfg(test)]
#[path = "state_test_env_tests.rs"]
pub(crate) mod test_env;

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
