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
pub(crate) mod test_env {
    use std::path::PathBuf;

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        xdg_state: Option<std::ffi::OsString>,
        #[cfg(windows)]
        local_app_data: Option<std::ffi::OsString>,
        #[cfg(windows)]
        user_profile: Option<std::ffi::OsString>,
        dir: PathBuf,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.xdg_state.take() {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
            #[cfg(windows)]
            {
                match self.local_app_data.take() {
                    Some(v) => std::env::set_var("LOCALAPPDATA", v),
                    None => std::env::remove_var("LOCALAPPDATA"),
                }
                match self.user_profile.take() {
                    Some(v) => std::env::set_var("USERPROFILE", v),
                    None => std::env::remove_var("USERPROFILE"),
                }
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    pub(crate) fn set_test_env() -> EnvGuard {
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
            #[cfg(windows)]
            local_app_data: std::env::var_os("LOCALAPPDATA"),
            #[cfg(windows)]
            user_profile: std::env::var_os("USERPROFILE"),
            dir: dir.clone(),
        };
        std::env::set_var("XDG_STATE_HOME", &dir);
        // On Windows the state dir resolves LOCALAPPDATA, then USERPROFILE.
        #[cfg(windows)]
        {
            std::env::set_var("LOCALAPPDATA", &dir);
            std::env::set_var("USERPROFILE", &dir);
        }
        g
    }
}

#[cfg(test)]
mod tests {
    use super::test_env::{serial, set_test_env};
    use super::*;

    // ADR 0042 L2a codex review, item H: the legacy session.json ->
    // session-local.json migration touches a real env var (XDG_STATE_HOME
    // for the state dir) and does real file I/O. Every test in this module
    // takes the SAME serial lock: the three path-shape tests below don't
    // need env isolation themselves, but without the lock they can read
    // XDG_STATE_HOME mid-mutation from a migration test running
    // concurrently on another thread (observed: state_path called twice in
    // the same test resolving to two DIFFERENT dirs). Deliberately does NOT
    // touch XDG_CONFIG_HOME/HOME — those are shared with
    // ui/persist/resume.rs's OWN (differently-locked) test env mutation,
    // and cross-module interference there was observed directly (a flaky
    // failure in load_tolerates_garbage_lines while this module's tests ran
    // concurrently).
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

    #[test]
    fn load_adopts_legacy_session_json_for_the_local_host_only() {
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

        // "local" adopts it -- no session-local.json exists yet.
        let adopted = load(&"local".to_string());
        assert_eq!(adopted.client_id, "client-legacy");
        assert_eq!(adopted.session_id.as_deref(), Some("sess-legacy"));
        assert_eq!(adopted.last_seen_revision, 42);

        // A DIFFERENT host (a new L2a addition) must NOT adopt the same
        // legacy file -- it has no prior connection to inherit from.
        let fresh = load(&"otherhost".to_string());
        assert_ne!(
            fresh.client_id, "client-legacy",
            "a non-local host must start fresh, never adopt the legacy file"
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
        // session-local.json already exists (a prior L2a launch already
        // ran and saved its own memory) -- the migration must not
        // override it with the older legacy file.
        save(
            &"local".to_string(),
            &SessionMemory {
                session_id: Some("sess-current".to_string()),
                client_id: "client-current".to_string(),
                last_seen_revision: 99,
            },
        )
        .unwrap();

        let m = load(&"local".to_string());
        assert_eq!(m.client_id, "client-current");
        assert_eq!(m.last_seen_revision, 99);
    }

    // --- Field incident 2026-09-08, defect (b): the read path must never
    // stall on synchronous per-frame disk I/O. ---

    #[test]
    fn state_save_gate_is_due_on_a_fresh_connection() {
        // A brand-new connection's first `rev`-bearing frame (hello / first
        // tree.root / first preview.get) must still save immediately, same
        // as before this fix — only a later burst gets coalesced.
        let gate = StateSaveGate::new();
        assert!(gate.due(std::time::Instant::now()));
    }

    #[test]
    fn state_save_gate_coalesces_within_min_interval_then_fires_again() {
        let t0 = std::time::Instant::now();
        let mut gate = StateSaveGate::new();
        gate.mark_saved(t0, 1);
        assert!(
            !gate.due(t0 + std::time::Duration::from_millis(50)),
            "a save 50ms after the last one must be coalesced"
        );
        assert!(
            !gate.due(t0 + StateSaveGate::MIN_INTERVAL - std::time::Duration::from_millis(1)),
            "still coalesced 1ms short of MIN_INTERVAL"
        );
        assert!(
            gate.due(t0 + StateSaveGate::MIN_INTERVAL),
            "due again once MIN_INTERVAL has fully elapsed"
        );
    }

    /// The lossless-flush guarantee that makes the throttle safe to keep: a
    /// frame lands INSIDE the 2s coalescing window (so `note_revision_with`
    /// bumps the in-memory revision but does not persist it — `gate` is
    /// still fresh from a prior save), then the connection ends. The
    /// end-of-connection `flush_revision_with` (what `SessionState::drop`
    /// calls in production) must still get the latest revision onto disk —
    /// this is the flush, not a timer, `SessionState`'s `Drop` performs.
    #[test]
    fn flush_revision_with_persists_a_revision_the_throttle_held_back() {
        let mut memory = crate::net::state::SessionMemory::fresh();
        let host = "flush-test-host".to_string();
        let mut gate = StateSaveGate::new();

        // An earlier frame already saved revision 1 (gate now fresh — the
        // very next `due()` check is inside MIN_INTERVAL).
        note_revision_with(Some(1), &mut memory, &host, &mut gate, |_h, m| {
            assert_eq!(m.last_seen_revision, 1);
        });
        assert_eq!(gate.last_saved_revision, 1);

        // A second frame, revision 2, arrives well inside MIN_INTERVAL — so
        // it must NOT persist on its own.
        let mut persisted_during_note = false;
        note_revision_with(Some(2), &mut memory, &host, &mut gate, |_h, _m| {
            persisted_during_note = true;
        });
        assert!(
            !persisted_during_note,
            "a frame inside the throttle window must not save on its own"
        );
        assert_eq!(memory.last_seen_revision, 2, "the in-memory value still bumps");
        assert_eq!(
            gate.last_saved_revision, 1,
            "disk is still behind — this is exactly what the flush must catch"
        );

        // The connection ends here. The flush must catch the gap.
        let mut flushed_revision = None;
        flush_revision_with(&memory, &host, &mut gate, |_h, m| {
            flushed_revision = Some(m.last_seen_revision);
        });
        assert_eq!(
            flushed_revision,
            Some(2),
            "revision 2 must reach disk on connection end, not be lost to the throttle"
        );
        assert_eq!(gate.last_saved_revision, 2);

        // A second flush with nothing new must be a no-op — nothing to lose
        // by calling it more than once (e.g. an exit path racing a save).
        let mut second_flush_ran = false;
        flush_revision_with(&memory, &host, &mut gate, |_h, _m| {
            second_flush_ran = true;
        });
        assert!(!second_flush_ran, "flushing twice with nothing new must not re-save");
    }

    /// The manager's literal measurement: a synthetic burst of 50
    /// `workspace.list`-sized replies (each carrying a `rev`) must drain in
    /// well under a second. Drives the REAL `note_revision_with` (the same
    /// function `note_revision` — and so the read arm — calls) with a
    /// stand-in `persist` that sleeps 50ms, standing in for the blocking
    /// `write`+`rename` `crate::net::state::save` performs: uncoalesced, 50
    /// frames would cost 2.5s; the gate must bring that down to one write.
    #[test]
    fn note_revision_coalesces_a_burst_of_fifty_replies_onto_one_slow_write() {
        let mut memory = crate::net::state::SessionMemory::fresh();
        let host = "burst-test-host".to_string();
        let mut gate = StateSaveGate::new();
        let mut persisted_revisions: Vec<u64> = Vec::new();
        let start = std::time::Instant::now();
        for rev in 1..=50u64 {
            note_revision_with(Some(rev), &mut memory, &host, &mut gate, |_h, m| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                persisted_revisions.push(m.last_seen_revision);
            });
        }
        let elapsed = start.elapsed();
        assert_eq!(
            memory.last_seen_revision, 50,
            "the in-memory revision must still bump on every frame, coalescing or not"
        );
        assert_eq!(
            persisted_revisions,
            vec![1],
            "a tight burst must coalesce onto exactly the FIRST (immediate) save"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "burst of 50 replies must drain in well under a second, took {elapsed:?}"
        );
    }

    #[test]
    fn note_revision_with_ignores_a_frame_with_no_rev() {
        let mut memory = crate::net::state::SessionMemory::fresh();
        let host = "no-rev-test-host".to_string();
        let mut gate = StateSaveGate::new();
        let mut save_calls = 0u32;
        note_revision_with(None, &mut memory, &host, &mut gate, |_h, _m| {
            save_calls += 1;
        });
        assert_eq!(memory.last_seen_revision, 0);
        assert_eq!(save_calls, 0, "no rev means nothing to persist");
    }
}
