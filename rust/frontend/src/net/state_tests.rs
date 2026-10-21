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
fn state_path_lives_in_sot_logs_state_dir() {
    let _g = set_test_env();
    assert_eq!(
        state_path(&"alpha".to_string()),
        sot_log::host::state_dir::sot_state_dir().unwrap().join("session-alpha.json")
    );
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

/// The literal measurement: a synthetic burst of 50
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
