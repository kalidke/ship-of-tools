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

/// The bare pre-L2a filename, same directory rule as `state_path`. Every
/// launch before ADR 0042 L2a made exactly one connection, so this file
/// held that connection's ENTIRE reconnect memory — there was no host
/// concept to suffix it with.
fn legacy_state_path() -> PathBuf {
    crate::paths::sot_state_dir()
        .unwrap_or_else(|| PathBuf::from(".").join("sot"))
        .join("session.json")
}

/// Whether `host` is the one a pre-L2a install's single connection would
/// have been — the ONLY host eligible to adopt the legacy file (every
/// other host is a NEW L2a addition with no prior connection to have
/// reconnect memory for in the first place). Mirrors
/// `hosts::resolve_connections`'s own default-name resolution: the
/// configured `hosts.toml default_host`, or `"default"` for the CLI-only
/// synthesis case (no `hosts.toml` at all, or no matching entry).
fn is_the_pre_l2a_default_host(host: &HostKey) -> bool {
    let default_name = crate::hosts::load()
        .default_host
        .unwrap_or_else(|| "default".to_string());
    host == &default_name
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

#[cfg(test)]
mod tests {
    use super::*;

    // ADR 0042 L2a codex review, item H: the legacy session.json ->
    // session-<default>.json migration touches real env vars (XDG_STATE_HOME
    // for the state dir; SOT_HOSTS to pin a deterministic hosts.toml so
    // `default_host` resolves to `None` -> "default") and does real file
    // I/O. Every test in this module takes the SAME serial lock: the
    // three path-shape tests below don't need env isolation themselves,
    // but without the lock they can read XDG_STATE_HOME mid-mutation from
    // a migration test running concurrently on another thread (observed:
    // state_path called twice in the same test resolving to two DIFFERENT
    // dirs). Deliberately does NOT touch XDG_CONFIG_HOME/HOME — those are
    // shared with state_persistence.rs's OWN (differently-locked) test
    // env mutation, and cross-module interference there was observed
    // directly (a flaky failure in load_tolerates_garbage_lines while
    // this module's tests ran concurrently). SOT_HOSTS alone is
    // sufficient: hosts::load()'s candidate search stops at the FIRST
    // path that reads successfully, and pointing it at a real (if empty)
    // file wins outright before XDG_CONFIG_HOME/HOME are ever tried.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn state_path_differs_per_host() {
        let _serial = serial();
        let a = state_path(&"alpha".to_string());
        let b = state_path(&"beta".to_string());
        assert_ne!(a, b, "two hosts must not share a session-memory file");
        assert!(a.to_string_lossy().contains("alpha"));
        assert!(b.to_string_lossy().contains("beta"));
    }

    #[test]
    fn state_path_is_stable_for_the_same_host() {
        let _serial = serial();
        let a1 = state_path(&"alpha".to_string());
        let a2 = state_path(&"alpha".to_string());
        assert_eq!(a1, a2);
    }

    #[test]
    fn state_path_sanitizes_unsafe_filename_characters() {
        let _serial = serial();
        let p = state_path(&"weird/host:name".to_string());
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.contains('/'));
        assert!(!name.contains(':'));
    }

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        xdg_state: Option<std::ffi::OsString>,
        sot_hosts: Option<std::ffi::OsString>,
        dir: PathBuf,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.xdg_state.take() {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
            match self.sot_hosts.take() {
                Some(v) => std::env::set_var("SOT_HOSTS", v),
                None => std::env::remove_var("SOT_HOSTS"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn set_test_env() -> EnvGuard {
        let _serial = serial();
        let dir = std::env::temp_dir().join(format!(
            "sot-state-migration-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let g = EnvGuard {
            _serial,
            xdg_state: std::env::var_os("XDG_STATE_HOME"),
            sot_hosts: std::env::var_os("SOT_HOSTS"),
            dir: dir.clone(),
        };
        std::env::set_var("XDG_STATE_HOME", &dir);
        // `hosts::load()`'s candidate search tries SOT_HOSTS, then
        // cwd/.sot/hosts.toml, then $XDG_CONFIG_HOME/sot/hosts.toml, then
        // $HOME/.config/sot/hosts.toml, IN ORDER, and stops at the FIRST
        // candidate that reads successfully. Point SOT_HOSTS at a real,
        // EMPTY file: it wins outright at the FIRST candidate, so
        // XDG_CONFIG_HOME/HOME are never even tried -- no need to touch
        // either (and touching them would race with
        // state_persistence.rs's own, differently-locked, XDG_CONFIG_HOME
        // mutation in its tests).
        let empty_hosts = dir.join("empty-hosts.toml");
        std::fs::write(&empty_hosts, b"").unwrap();
        std::env::set_var("SOT_HOSTS", &empty_hosts);
        g
    }

    #[test]
    fn load_adopts_legacy_session_json_for_the_default_host_only() {
        let _g = set_test_env();
        // No hosts.toml -> default_host resolves to "default".
        let legacy = legacy_state_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(
            &legacy,
            serde_json::to_vec(&SessionMemory {
                session_id: Some("sess-legacy".to_string()),
                client_id: "client-legacy".to_string(),
                last_seen_revision: 42,
            })
            .unwrap(),
        )
        .unwrap();

        // The default host adopts it -- no session-default.json exists yet.
        let adopted = load(&"default".to_string());
        assert_eq!(adopted.client_id, "client-legacy");
        assert_eq!(adopted.session_id.as_deref(), Some("sess-legacy"));
        assert_eq!(adopted.last_seen_revision, 42);

        // A DIFFERENT host (a new L2a addition) must NOT adopt the same
        // legacy file -- it has no prior connection to inherit from.
        let fresh = load(&"otherhost".to_string());
        assert_ne!(
            fresh.client_id, "client-legacy",
            "a non-default host must start fresh, never adopt the legacy file"
        );
    }

    #[test]
    fn load_ignores_legacy_session_json_once_the_hosted_file_exists() {
        let _g = set_test_env();
        let legacy = legacy_state_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(
            &legacy,
            serde_json::to_vec(&SessionMemory {
                session_id: Some("sess-legacy".to_string()),
                client_id: "client-legacy".to_string(),
                last_seen_revision: 42,
            })
            .unwrap(),
        )
        .unwrap();
        // session-default.json already exists (a prior L2a launch already
        // ran and saved its own memory) -- the migration must not
        // override it with the older legacy file.
        save(
            &"default".to_string(),
            &SessionMemory {
                session_id: Some("sess-current".to_string()),
                client_id: "client-current".to_string(),
                last_seen_revision: 99,
            },
        )
        .unwrap();

        let m = load(&"default".to_string());
        assert_eq!(m.client_id, "client-current");
        assert_eq!(m.last_seen_revision, 99);
    }
}
