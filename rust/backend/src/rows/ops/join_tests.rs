use super::*;
use crate::rows::handle::bound_handle;
use crate::rows::Workspace;

// `handle_agent_join` persists through `crate::rows::store::save` —
// isolate every var `app_config_dir` reads (S18:
// Windows persistence ignores XDG_CONFIG_HOME entirely and uses
// LOCALAPPDATA/USERPROFILE instead — guarding only the Unix var let
// this test module write into a real app-config root on Windows).
// Mirrors `rows/store/`'s own round-trip test's exact setup.
struct EnvGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
    localappdata: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
    sot_comm_home: Option<std::ffi::OsString>,
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, val) in [
            ("XDG_CONFIG_HOME", &self.xdg_config_home),
            ("HOME", &self.home),
            ("LOCALAPPDATA", &self.localappdata),
            ("USERPROFILE", &self.userprofile),
            ("SOT_COMM_HOME", &self.sot_comm_home),
        ] {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}
fn env_guarded() -> (EnvGuard, std::path::PathBuf) {
    let serial = crate::paths::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!(
        "sot-agent-join-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let g = EnvGuard {
        xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
        home: std::env::var_os("HOME"),
        localappdata: std::env::var_os("LOCALAPPDATA"),
        userprofile: std::env::var_os("USERPROFILE"),
        sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
        _serial: serial,
    };
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    std::env::set_var("LOCALAPPDATA", &dir);
    // No `scan_disk` in these tests may read a live comm registry.
    std::env::set_var("SOT_COMM_HOME", &dir);
    std::env::remove_var("USERPROFILE");
    (g, dir)
}

fn mk_ws(label: &str) -> Workspace {
    Workspace::from_label(
        label,
        std::path::PathBuf::from("/p"),
        false,
        "none".into(),
        String::new(),
        String::new(),
    )
}

#[tokio::test]
async fn agent_join_persists_and_list_merges_by_the_declared_handle() {
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let id = workspaces.insert(mk_ws("agentjoin")).workspace_id.clone();
    let (ws_events_tx, mut ws_events_rx) = tokio::sync::broadcast::channel(4);

    let payload = serde_json::json!({"workspace_id": id, "handle": "agentjoin-testhost"});
    let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx)
        .await
        .expect("handler must not error");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0.payload.get("ok").and_then(|v| v.as_bool()), Some(true));

    // Persisted in the in-memory registry, readable back via resolve().
    let resolved = workspaces.resolve(Some(&id)).expect("workspace still registered");
    assert_eq!(resolved.agent_handle(), "agentjoin-testhost");

    // `workspace.list`'s own row-binding rule merges by the declared
    // handle — this is what makes the join visible there.
    let merged = bound_handle(&resolved, &workspaces.list());
    assert_eq!(merged, "agentjoin-testhost");

    // workspace.changed published so the FE re-lists.
    let evt = ws_events_rx.try_recv().expect("workspace.changed must be published");
    assert_eq!(evt.workspace_id, id);

    let _ = std::fs::remove_dir_all(&dir);
}

// Round 3 (Codex): the round-2 version of this test was
// ineffective -- it removed the row from the registry (and the toml)
// BEFORE spawning `handle_agent_join`, so the handler's very first
// `capsule_guard` lookup returned `None` immediately and the join
// never actually contended for the held mutex at all; the test passed
// even with the round-2 B1 fix fully reverted. Rebuilt here to match
// the real race: the row stays registered and the toml stays on disk
// while we hold the guard and spawn the REAL handler, we prove that
// spawned join is genuinely PENDING (a bounded timeout that must
// elapse -- deterministic, not a sleep-and-hope, because a task
// blocked on a held `tokio::sync::Mutex` cannot complete regardless of
// how long we wait), and only THEN do we perform destroy's own two
// actions (toml removal, `remove_by_id`) while STILL holding the
// guard, exactly the way `handle_workspace_destroy` holds
// `destroy_guard` across that same sequence for both its capsule arm
// and (round 3 finding B) its tmux arm. Releasing the guard only after
// that is what proves the join, once unblocked, refuses cleanly
// instead of resurrecting what destroy just removed. Run for both a
// capsule row and a tmux row: `capsule_guard` mints/returns a guard
// for any registered row regardless of runtime (see its own doc
// comment), and round 3's fix made the tmux destroy arm take it too,
// so the same interleaving must be closed for both.
async fn agent_join_blocks_on_held_guard_then_destroy_wins(runtime: &str, slug: &str) {
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let mut ws = mk_ws(slug);
    ws.runtime = runtime.to_string();
    let id = workspaces.insert(ws).workspace_id.clone();
    crate::rows::store::save(&workspaces.resolve(Some(&id)).unwrap()).expect("seed save");
    let toml_path = crate::rows::store::toml_path_for(slug);
    assert!(toml_path.exists(), "test setup: the seed toml must exist");

    // 1 + 2: the row stays registered; take and hold its guard exactly
    // the way `handle_workspace_destroy` does (both arms, since round
    // 3) across its own toml-delete + remove_by_id sequence.
    let guard = workspaces.capsule_guard(&id).expect("row is registered");
    let held = guard.lock().await;

    // 3: spawn the REAL handler while the row is STILL registered and
    // the guard is STILL held -- it must block trying to acquire the
    // same guard.
    let workspaces_for_join = workspaces.clone();
    let (ws_events_tx, mut ws_events_rx) = tokio::sync::broadcast::channel(4);
    let id_for_join = id.clone();
    let handle_for_join = format!("{slug}-testhost");
    let mut join_task = tokio::spawn(async move {
        let payload = serde_json::json!({"workspace_id": id_for_join, "handle": handle_for_join});
        handle_agent_join(1, payload, &workspaces_for_join, &ws_events_tx).await
    });

    // Prove it is genuinely PENDING: this bounded wait must elapse,
    // not race-and-hope -- a task truly blocked on the held guard
    // cannot complete no matter how long we wait, so a timeout here
    // is a deterministic proof, not a flaky one.
    let still_pending =
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut join_task).await;
    assert!(
        still_pending.is_err(),
        "join must still be blocked on the held guard, not completed"
    );

    // 4: perform destroy's own two actions while STILL holding the
    // guard -- the exact interleaving the guard exists to serialize
    // against.
    std::fs::remove_file(&toml_path).expect("remove seed toml");
    workspaces.remove_by_id(&id);
    assert!(!toml_path.exists(), "test setup: toml must be gone before the guard is released");
    assert!(workspaces.resolve(Some(&id)).is_none(), "test setup: the row must be gone from the registry");

    // 5: release the guard -- only now can the join proceed.
    drop(held);

    let out = join_task.await.expect("join task must not panic").expect("handler must not error");
    assert_eq!(
        out[0].0.payload.get("code").and_then(|v| v.as_str()),
        Some("unknown_workspace"),
        "a join that loses the race to a destroy must refuse, never resurrect: {:?}",
        out[0].0.payload
    );
    assert!(
        !toml_path.exists(),
        "the destroyed row's toml must never be recreated by a losing join"
    );
    assert!(
        workspaces.resolve(Some(&id)).is_none(),
        "the destroyed row must stay gone from the registry"
    );
    assert!(ws_events_rx.try_recv().is_err(), "no workspace.changed for a refused join");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn agent_join_interleaved_with_destroy_never_recreates_the_toml_capsule() {
    agent_join_blocks_on_held_guard_then_destroy_wins("capsule", "agentjoin-race-capsule").await;
}

// One join, then the task that saves the rows it took the handle from.
async fn join(workspaces: &Workspaces, id: &str, handle: &str) -> serde_json::Value {
    let (ws_events_tx, _rx) = tokio::sync::broadcast::channel(4);
    let payload = serde_json::json!({"workspace_id": id, "handle": handle});
    let (out, saves) = join_row(1, payload, workspaces, &ws_events_tx).await.expect("handler must not error");
    if let Some(saves) = saves {
        saves.await.expect("the moved-row saves must not panic");
    }
    out[0].0.payload.clone()
}

fn handles_by_slug(workspaces: &Workspaces) -> Vec<(String, String)> {
    let mut v: Vec<_> = workspaces.list().iter().map(|w| (w.slug.clone(), w.agent_handle())).collect();
    v.sort();
    v
}

// ADR 0049 (B5): a newer declaration moves the handle off the older row,
// in memory and in that row's toml.
#[tokio::test]
async fn a_newer_join_moves_the_handle_off_the_older_row_in_memory_and_on_disk() {
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let other = workspaces.insert(mk_ws("m5-other")).workspace_id.clone();
    let older = workspaces.insert(mk_ws("m5-older")).workspace_id.clone();
    let newer = workspaces.insert(mk_ws("m5-newer")).workspace_id.clone();
    for (id, handle) in [(&other, "m5-someone"), (&older, "m5-shared"), (&newer, "m5-shared")] {
        let res = join(&workspaces, id, handle).await;
        assert_eq!(res.get("ok").and_then(|v| v.as_bool()), Some(true), "{res:?}");
    }
    let want = |a: &str, b: &str, c: &str| {
        vec![
            ("m5-newer".to_string(), a.to_string()),
            ("m5-older".to_string(), b.to_string()),
            ("m5-other".to_string(), c.to_string()),
        ]
    };
    let mut expected = want("m5-shared", "", "m5-someone");
    expected.sort();
    assert_eq!(handles_by_slug(&workspaces), expected);

    // The same three values load back from the tomls.
    let fresh = Workspaces::new();
    assert_eq!(crate::rows::store::scan_disk(&fresh, false).unwrap(), 3);
    assert_eq!(handles_by_slug(&fresh), expected);

    let _ = std::fs::remove_dir_all(&dir);
}

// A row that lost the handle is saved after the join's reply, only under
// its own guard and only while registered: the reply never waits on that
// guard, no save lands while another holder has it, and a destroy that
// wins the guard is never undone.
#[tokio::test]
async fn a_join_never_recreates_the_toml_of_a_destroyed_row_it_took_the_handle_from() {
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let older = workspaces.insert(mk_ws("m5-race-older")).workspace_id.clone();
    let newer = workspaces.insert(mk_ws("m5-race-newer")).workspace_id.clone();
    let res = join(&workspaces, &older, "m5-race").await;
    assert_eq!(res.get("ok").and_then(|v| v.as_bool()), Some(true));
    let toml_path = crate::rows::store::toml_path_for("m5-race-older");
    assert!(toml_path.exists(), "test setup: the older row's toml must exist");

    let guard = workspaces.capsule_guard(&older).expect("row is registered");
    let held = guard.lock().await;
    let (ws_events_tx, _rx) = tokio::sync::broadcast::channel(4);
    let payload = serde_json::json!({"workspace_id": newer, "handle": "m5-race"});
    let (out, saves) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        join_row(1, payload, &workspaces, &ws_events_tx),
    )
    .await
    .expect("the reply must not wait on the older row's held guard")
    .expect("handler must not error");
    assert_eq!(out[0].0.payload.get("ok").and_then(|v| v.as_bool()), Some(true), "{:?}", out[0].0.payload);
    let saves = saves.expect("a row lost the handle");
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert!(!saves.is_finished(), "the save must wait for the older row's guard");
    assert!(
        std::fs::read_to_string(&toml_path).unwrap().contains("= \"m5-race\""),
        "the older row must not be saved while another holder has its guard"
    );

    std::fs::remove_file(&toml_path).expect("remove the older toml");
    workspaces.remove_by_id(&older);
    drop(held);
    saves.await.expect("the moved-row saves must not panic");
    assert!(!toml_path.exists(), "the destroyed row's toml must never be recreated");
    assert!(workspaces.resolve(Some(&older)).is_none(), "the destroyed row must stay gone");
    assert_eq!(workspaces.resolve(Some(&newer)).unwrap().agent_handle(), "m5-race");

    let _ = std::fs::remove_dir_all(&dir);
}

// The joiner's own save failing does not keep the older row's toml on
// the handle: its client's retry would find nothing left to move.
#[tokio::test]
#[cfg(unix)]
async fn a_failed_save_of_the_joiner_still_saves_the_row_that_lost_the_handle() {
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let older = workspaces.insert(mk_ws("m5-fail-older")).workspace_id.clone();
    let newer = workspaces.insert(mk_ws("m5-fail-newer")).workspace_id.clone();
    let res = join(&workspaces, &older, "m5-fail").await;
    assert_eq!(res.get("ok").and_then(|v| v.as_bool()), Some(true));
    // A directory where the newer row's toml goes makes only its save fail.
    std::fs::create_dir_all(crate::rows::store::toml_path_for("m5-fail-newer")).unwrap();

    let res = join(&workspaces, &newer, "m5-fail").await;
    assert_eq!(res.get("code").and_then(|v| v.as_str()), Some("persist_failed"), "{res:?}");
    let text = std::fs::read_to_string(crate::rows::store::toml_path_for("m5-fail-older")).unwrap();
    assert!(!text.contains("= \"m5-fail\""), "the older row's toml still names the moved handle: {text}");

    let _ = std::fs::remove_dir_all(&dir);
}

// Crossing joins of one handle: A re-joins while B joins. If a join kept
// its own guard while saving the row it took the handle from, each would
// wait for the other's guard for good (tokio's Mutex is FIFO, so this
// order is deterministic on the test runtime).
#[tokio::test]
async fn crossing_joins_of_one_handle_never_deadlock() {
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let a = workspaces.insert(mk_ws("m5-cross-a")).workspace_id.clone();
    let b = workspaces.insert(mk_ws("m5-cross-b")).workspace_id.clone();
    let res = join(&workspaces, &a, "m5-cross").await;
    assert_eq!(res.get("ok").and_then(|v| v.as_bool()), Some(true));

    let guard = workspaces.capsule_guard(&a).expect("row is registered");
    let held = guard.lock().await;
    let (wa, a2) = (workspaces.clone(), a.clone());
    let join_a = tokio::spawn(async move { join(&wa, &a2, "m5-cross").await });
    tokio::task::yield_now().await;
    let (wb, b2) = (workspaces.clone(), b.clone());
    let join_b = tokio::spawn(async move { join(&wb, &b2, "m5-cross").await });
    tokio::task::yield_now().await;
    assert_eq!(workspaces.resolve(Some(&b)).unwrap().agent_handle(), "m5-cross", "B's join must have taken the handle");
    drop(held);

    for task in [join_a, join_b] {
        let res = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("crossing joins of one handle deadlocked")
            .expect("join task must not panic");
        assert_eq!(res.get("ok").and_then(|v| v.as_bool()), Some(true), "{res:?}");
    }
    let expected = vec![("m5-cross-a".to_string(), "m5-cross".to_string()), ("m5-cross-b".to_string(), String::new())];
    assert_eq!(handles_by_slug(&workspaces), expected);
    let fresh = Workspaces::new();
    assert_eq!(crate::rows::store::scan_disk(&fresh, false).unwrap(), 2);
    assert_eq!(handles_by_slug(&fresh), expected);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn agent_join_refuses_unknown_workspace() {
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let (ws_events_tx, _rx) = tokio::sync::broadcast::channel(4);

    let payload = serde_json::json!({"workspace_id": "ws-does-not-exist", "handle": "somehandle"});
    let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx)
        .await
        .expect("handler must not error");
    assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("unknown_workspace"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn agent_join_refuses_bad_handle_and_persists_nothing() {
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let id = workspaces.insert(mk_ws("badhandle")).workspace_id.clone();
    let (ws_events_tx, _rx) = tokio::sync::broadcast::channel(4);

    let payload = serde_json::json!({"workspace_id": id, "handle": "not a valid handle!"});
    let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx)
        .await
        .expect("handler must not error");
    assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("bad_handle"));

    let resolved = workspaces.resolve(Some(&id)).expect("workspace still registered");
    assert_eq!(resolved.agent_handle(), "", "a refused join must persist nothing");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[cfg(unix)]
async fn agent_join_reports_persist_failed_and_never_returns_ok_on_a_save_failure() {
    // (S7, Codex finding B9): ok:true must depend on
    // the save actually succeeding. Occupy the "sot" config
    // subdirectory with a plain FILE instead of a directory --
    // `crate::rows::store::save`'s `create_dir_all` necessarily fails
    // under it, regardless of the host-derived `workspaces-<host>`
    // segment.
    let (_g, dir) = env_guarded();
    std::fs::write(dir.join("sot"), b"not a directory").unwrap();

    let workspaces = Workspaces::new();
    let id = workspaces.insert(mk_ws("agentjoin-persist-fail")).workspace_id.clone();
    let (ws_events_tx, mut ws_events_rx) = tokio::sync::broadcast::channel(4);

    let payload =
        serde_json::json!({"workspace_id": id, "handle": "agentjoin-persist-fail-testhost"});
    let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx)
        .await
        .expect("a save failure is a wire-level refusal, not a Rust error");
    assert_eq!(out.len(), 1);
    assert_ne!(
        out[0].0.payload.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "ok:true must never be returned when persistence failed"
    );
    assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("persist_failed"));

    // The in-memory update still landed (S6's guarded in-place update
    // runs before the save attempt) -- only the DURABILITY claim is
    // refused.
    let resolved = workspaces.resolve(Some(&id)).expect("workspace still registered");
    assert_eq!(resolved.agent_handle(), "agentjoin-persist-fail-testhost");

    // No workspace.changed on a failed persist -- the FE would
    // re-list to a handle that isn't actually durable yet.
    assert!(
        ws_events_rx.try_recv().is_err(),
        "must not publish workspace.changed on a failed persist"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn agent_join_after_the_workspace_is_destroyed_reports_unknown_never_resurrects() {
    // (S6, Codex finding B5): the destroy race at the
    // handler level -- a join against a workspace_id the registry no
    // longer has must refuse cleanly, never bring the row back.
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let id = workspaces.insert(mk_ws("agentjoin-destroyed")).workspace_id.clone();
    workspaces.remove_by_id(&id);
    assert!(workspaces.resolve(Some(&id)).is_none(), "test setup: the row must actually be gone");

    let (ws_events_tx, mut ws_events_rx) = tokio::sync::broadcast::channel(4);
    let payload = serde_json::json!({"workspace_id": id, "handle": "agentjoin-destroyed-testhost"});
    let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx)
        .await
        .expect("handler must not error");
    assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("unknown_workspace"));
    assert!(workspaces.resolve(Some(&id)).is_none(), "the destroyed row must stay gone -- no resurrection");
    assert!(ws_events_rx.try_recv().is_err(), "no workspace.changed for a refused join");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A capsule row declaring `handle`, with `drive` applied to its phase cell: the observations it has been
/// seen through.
fn row_seen(workspaces: &Workspaces, label: &str, handle: &str, drive: &[crate::rows::workspace::Observation]) -> String {
    let mut w = mk_ws(label);
    w.runtime = "capsule".to_string();
    w.agent_handle = std::sync::Mutex::new(handle.to_string());
    for obs in drive {
        assert!(w.apply_phase_observation(obs.clone()), "{label}: {obs:?}");
    }
    workspaces.insert(w).workspace_id.clone()
}

fn at(phase: crate::rows::workspace::Phase) -> crate::rows::workspace::Observation {
    use crate::rows::workspace::{Observation, SupervisorIdentity};
    Observation::Phase {
        phase,
        supervisor: SupervisorIdentity { pid: 1, created: 1 },
        voyage: Some(uuid::Uuid::from_u128(1)),
    }
}

/// A row that runs (Ready) and declares `handle`.
fn running_row(workspaces: &Workspaces, label: &str, handle: &str) -> String {
    row_seen(workspaces, label, handle, &[at(crate::rows::workspace::Phase::Ready)])
}

async fn outside_answer(workspaces: &Workspaces, handle: &str) -> serde_json::Value {
    let (ws_events_tx, _rx) = tokio::sync::broadcast::channel(4);
    let payload = serde_json::json!({"workspace_id": "", "handle": handle});
    handle_agent_join(1, payload, workspaces, &ws_events_tx).await.expect("handler must not error")[0].0.payload.clone()
}

// A session outside any row (an empty workspace_id) is refused a handle a running row holds, with a message
// naming the handle, and the row keeps it.
#[tokio::test]
async fn a_join_from_outside_any_row_is_refused_a_handle_a_running_row_holds() {
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let id = running_row(&workspaces, "outside-held", "outside-held-box");
    let (ws_events_tx, mut ws_events_rx) = tokio::sync::broadcast::channel(4);

    let payload = serde_json::json!({"workspace_id": "", "handle": "outside-held-box"});
    let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx).await.expect("handler must not error");
    let p = &out[0].0.payload;
    assert_eq!(p.get("code").and_then(|v| v.as_str()), Some("held_by_row"), "{p:?}");
    let msg = p.get("error").and_then(|v| v.as_str()).unwrap_or_default();
    assert!(msg.contains("@outside-held-box") && msg.contains("outside any row"), "{msg}");
    assert_eq!(workspaces.resolve(Some(&id)).unwrap().agent_handle(), "outside-held-box");
    assert!(ws_events_rx.try_recv().is_err(), "a refused join publishes nothing");

    let _ = std::fs::remove_dir_all(&dir);
}

// The daemon says a handle is free only for a row it has seen end (Terminal, EndedNoRespawn) or no row: a row
// not seen running (loaded at start, Stopped, Unreachable, Foreign, Ending) may still run a session (B1).
#[tokio::test]
async fn a_join_from_outside_any_row_is_refused_a_handle_of_every_row_not_seen_ended() {
    use crate::rows::workspace::{Observation, Phase};
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let not_ended: [(&str, Vec<Observation>); 7] = [
        ("unobserved", vec![]),
        ("stopped", vec![Observation::Stopped]),
        ("starting", vec![at(Phase::Starting)]),
        ("ready", vec![at(Phase::Ready)]),
        ("ending", vec![at(Phase::Ending)]),
        ("unreachable", vec![at(Phase::Ready), Observation::Failed, Observation::Failed]),
        ("foreign", vec![Observation::Foreign]),
    ];
    for (label, drive) in &not_ended {
        row_seen(&workspaces, label, &format!("outside-{label}-box"), drive);
    }
    let ended: [(&str, Vec<Observation>); 2] =
        [("terminal", vec![at(Phase::Terminal)]), ("endednorespawn", vec![at(Phase::EndedNoRespawn)])];
    for (label, drive) in &ended {
        row_seen(&workspaces, label, &format!("outside-{label}-box"), drive);
    }
    for (label, _) in &not_ended {
        let p = outside_answer(&workspaces, &format!("outside-{label}-box")).await;
        assert_eq!(p.get("code").and_then(|v| v.as_str()), Some("held_by_row"), "{label}: {p:?}");
        // A row held for any phase, a stopped or unreachable one included, is not called live.
        let msg = p.get("error").and_then(|v| v.as_str()).unwrap_or_default();
        assert!(msg.contains("is held by a row;") && !msg.contains("live"), "{label}: {msg}");
    }
    for label in ["terminal", "endednorespawn", "destroyed"] {
        let p = outside_answer(&workspaces, &format!("outside-{label}-box")).await;
        assert_eq!(p.get("ok").and_then(|v| v.as_bool()), Some(true), "{label}: {p:?}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// A row's own join is the old path: a newer declaration from a row still moves the handle (B5).
#[tokio::test]
async fn a_row_taking_a_handle_from_another_row_still_moves_it() {
    let (_g, dir) = env_guarded();
    let workspaces = Workspaces::new();
    let older = running_row(&workspaces, "outside-older", "outside-moved-box");
    let newer = workspaces.insert(mk_ws("outside-newer")).workspace_id.clone();
    let (ws_events_tx, _rx) = tokio::sync::broadcast::channel(4);

    let payload = serde_json::json!({"workspace_id": newer, "handle": "outside-moved-box"});
    let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx).await.expect("handler must not error");
    assert_eq!(out[0].0.payload.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(workspaces.resolve(Some(&newer)).unwrap().agent_handle(), "outside-moved-box");
    assert_eq!(workspaces.resolve(Some(&older)).unwrap().agent_handle(), "");

    let _ = std::fs::remove_dir_all(&dir);
}
