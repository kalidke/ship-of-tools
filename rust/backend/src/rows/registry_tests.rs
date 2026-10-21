use super::*;

#[test]
fn registry_insert_and_resolve_by_id_and_slug() {
    let reg = Workspaces::new();
    let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
    let id = ws.workspace_id.clone();
    reg.insert(ws);
    assert!(reg.resolve(Some(&id)).is_some());
    assert_eq!(reg.resolve(Some("alpha")).unwrap().slug, "alpha");
}

#[test]
fn registry_default_resolves_when_id_missing() {
    let reg = Workspaces::new();
    let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
    let id = ws.workspace_id.clone();
    reg.insert(ws);
    reg.set_default(&id);
    assert_eq!(reg.resolve(None).unwrap().slug, "alpha");
}

/// ADR 0046 decision 1: `agent.join`'s persistence target. Mirrors
/// `reset_agent_to_none_makes_a_carried_over_default_row_inert_again`'s
/// shape — mutate through the registry, then prove BOTH the returned
/// handle and a fresh `resolve()` see it, and that unrelated metadata
/// (here `runtime`) survives the `insert()` round trip unchanged.
#[test]
fn set_agent_handle_persists_in_the_registry_and_preserves_other_fields() {
    let reg = Workspaces::new();
    let mut row = Workspace::from_label(
        "capsuleprobe",
        PathBuf::from("/home/u/capsuleprobe"),
        false,
        "claude".into(),
        String::new(),
        String::new(),
    );
    row.runtime = "capsule".to_string();
    let row = reg.insert(row);
    let original_id = row.workspace_id.clone();
    assert_eq!(row.agent_handle(), "", "a fresh row has never joined");

    let joined = reg
        .set_agent_handle(&row.workspace_id, "capsuleprobe-testhost")
        .expect("the row is registered")
        .0;
    assert_eq!(joined.workspace_id, original_id, "set_agent_handle must preserve the id");
    assert_eq!(joined.agent_handle(), "capsuleprobe-testhost");
    assert_eq!(joined.runtime, "capsule", "unrelated metadata must survive the update");

    // The registry's own row (not just the returned handle) reflects
    // the join -- resolve() must see it too.
    let resolved = reg.resolve(Some(&original_id)).unwrap();
    assert_eq!(resolved.agent_handle(), "capsuleprobe-testhost");
}

#[test]
fn set_agent_handle_on_an_unknown_workspace_is_none() {
    let reg = Workspaces::new();
    assert!(reg.set_agent_handle("nope", "someone").is_none());
}

#[test]
fn set_agent_handle_mutates_the_same_arc_never_a_replacement() {
    // (S6, Codex finding B5): the row's resource caches
    // live on the SAME struct instance as agent_handle -- a guarded
    // in-place update must never discard them by reconstructing the
    // Workspace. Constructing the kernel handle BEFORE the join, then
    // checking it is still built afterward, proves no replacement
    // happened (a `meta_only`-rebuilt row would start with a cold,
    // unbuilt kernel cache).
    let reg = Workspaces::new();
    let row = Workspace::from_label(
        "capsuleprobe2",
        PathBuf::from("/home/u/capsuleprobe2"),
        false,
        "claude".into(),
        String::new(),
        String::new(),
    );
    let row = reg.insert(row);
    let _ = row.kernel();
    assert!(row.kernel_built(), "test setup: the kernel cache must be built before the join");

    let joined = reg.set_agent_handle(&row.workspace_id, "capsuleprobe2-testhost").unwrap().0;
    assert!(
        joined.kernel_built(),
        "a guarded in-place update must never discard a live resource cache"
    );
    assert!(
        Arc::ptr_eq(&row, &joined),
        "set_agent_handle must mutate the SAME Arc, never hand back a replacement"
    );
}

#[test]
fn set_agent_handle_after_the_row_is_destroyed_never_resurrects_it() {
    // (S6, Codex finding B5): the old replace-based
    // implementation read the row, then reinserted a fresh copy --
    // a destroy landing between those two steps got resurrected by
    // the reinsert. The guarded in-place version never touches
    // by_id/by_slug at all, so this is structurally impossible: a
    // destroyed row simply has no entry for a later join to find.
    let reg = Workspaces::new();
    let row = Workspace::from_label(
        "capsuleprobe3",
        PathBuf::from("/home/u/capsuleprobe3"),
        false,
        "claude".into(),
        String::new(),
        String::new(),
    );
    let row = reg.insert(row);
    let id = row.workspace_id.clone();

    reg.remove_by_id(&id);
    assert!(reg.resolve(Some(&id)).is_none(), "test setup: the row must actually be gone");

    assert!(
        reg.set_agent_handle(&id, "capsuleprobe3-testhost").is_none(),
        "a join against a destroyed workspace_id must report unknown_workspace, never resurrect the row"
    );
    assert!(
        reg.resolve(Some(&id)).is_none(),
        "the destroyed row must stay gone -- no resurrection"
    );
}

#[test]
fn registry_resolve_unknown_is_none() {
    let reg = Workspaces::new();
    assert!(reg.resolve(Some("nope")).is_none());
    assert!(reg.resolve(None).is_none());
}

#[test]
fn registry_default_id_round_trip() {
    let reg = Workspaces::new();
    assert!(reg.default_id().is_none());
    let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
    let id = ws.workspace_id.clone();
    reg.insert(ws);
    reg.set_default(&id);
    assert_eq!(reg.default_id().as_deref(), Some(id.as_str()));
}

#[test]
fn registry_reinsert_preserves_workspace_id() {
    let reg = Workspaces::new();
    let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
    let original_id = ws.workspace_id.clone();
    reg.insert(ws);
    // Same slug, different label — id should stay the same.
    let again = Workspace::from_label("alpha", PathBuf::from("/p/alpha-renamed"), false, "none".into(), String::new(), String::new());
    reg.insert(again);
    let resolved = reg.resolve(Some("alpha")).unwrap();
    assert_eq!(resolved.workspace_id, original_id);
    assert_eq!(resolved.project_root, PathBuf::from("/p/alpha-renamed"));
}

// The default row's boot re-seed (`seed_default_row`) carries the row's
// declared handle into a fresh `Workspace` and re-inserts it; the
// same-slug arm used to blank it through `meta_only`.
#[test]
fn a_same_slug_reinsert_takes_the_new_rows_declared_handle() {
    let reg = Workspaces::new();
    let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
    let original_id = ws.workspace_id.clone();
    reg.insert(ws);
    reg.set_agent_handle(&original_id, "m5-carried").expect("registered");

    let mut carried = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
    carried.agent_handle = Mutex::new("m5-carried".to_string());
    reg.insert(carried);
    let resolved = reg.resolve(Some("alpha")).unwrap();
    assert_eq!(resolved.workspace_id, original_id);
    assert_eq!(resolved.agent_handle(), "m5-carried");

    let fresh = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
    reg.insert(fresh);
    assert_eq!(reg.resolve(Some("alpha")).unwrap().agent_handle(), "");
}

/// ADR 0042 slice L1a: `insert`'s own doc says the id-preserving
/// re-insert (same slug) takes "the REST of the metadata" from the
/// NEW `ws` — `runtime` must be no exception. Before `insert`'s own
/// fix, the reconstruction branch called `Workspace::meta_only`
/// (which always defaults `runtime` to "tmux" internally) without
/// threading the incoming `ws.runtime` through at all, so EVERY
/// same-slug reinsert silently reset it to "tmux" regardless of what
/// the caller passed — exactly the "id-preserving refresh" a second
/// `workspace.create` for an existing capsule workspace's slug is
/// (`rows/ops/create.rs`'s own duplicate-root gate comment names this case),
/// which still sets `runtime = "capsule"` on every call.
#[test]
fn registry_reinsert_takes_the_new_runtime_not_metas_default() {
    let reg = Workspaces::new();
    let ws = Workspace::from_label("alpha", PathBuf::from("/p/alpha"), false, "none".into(), String::new(), String::new());
    reg.insert(ws);
    let mut again = Workspace::from_label("alpha", PathBuf::from("/p/alpha-renamed"), false, "none".into(), String::new(), String::new());
    again.runtime = "capsule".to_string();
    reg.insert(again);
    let resolved = reg.resolve(Some("alpha")).unwrap();
    assert_eq!(resolved.runtime, "capsule");
}

#[tokio::test]
async fn remove_by_id_aborts_the_row_s_observer_task() {
    let reg = Workspaces::new();
    let ws = Workspace::from_label("obs", PathBuf::from("/p/obs"), false, "none".into(), String::new(), String::new());
    let id = ws.workspace_id.clone();
    reg.insert(ws);
    assert!(!reg.has_observer(&id), "no observer installed yet");

    let ticks = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let ticks_for_task = ticks.clone();
    let handle = tokio::spawn(async move {
        loop {
            ticks_for_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    });
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancelled_for_closure = cancelled.clone();
    let cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        cancelled_for_closure.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    reg.install_observer(&id, handle, cancel);
    assert!(reg.has_observer(&id));

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(ticks.load(std::sync::atomic::Ordering::SeqCst) > 0, "the task must actually be running");

    reg.remove_by_id(&id);
    assert!(!reg.has_observer(&id), "removal must forget the handle");
    assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst), "removal must call the paired cancel closure too");

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let after_removal = ticks.load(std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(
        ticks.load(std::sync::atomic::Ordering::SeqCst),
        after_removal,
        "the observer task must stop ticking once its row is removed"
    );
}

/// A same-slug `workspace.create` of a stopped capsule row replaces the row's `Arc` under its id
/// (`insert_replacing`), observes its settle into the new row, and calls `observer::ensure_running`, which finds
/// the id's observer already running. That observer's rounds must land on the row the registry holds, and the row
/// it replaced must be let go, with its watcher and its kernel and REPL children.
#[tokio::test]
async fn a_running_observer_feeds_the_row_a_same_slug_insert_registers() {
    use crate::rows::run::observer::{ensure_running, observe};
    use crate::rows::workspace::{Observation, Phase, SupervisorIdentity};
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = ["XDG_STATE_HOME", "LOCALAPPDATA"].map(crate::paths::EnvGuard::capture);
    // No voyage pointer under this state root, so every observer round reads `Stopped`.
    let state = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_STATE_HOME", state.path());
    std::env::set_var("LOCALAPPDATA", state.path());
    let reg = Workspaces::new();
    let register = || {
        let mut ws =
            Workspace::from_label("refresh", PathBuf::from("/p/refresh"), false, "none".into(), String::new(), String::new());
        ws.runtime = "capsule".to_string();
        reg.insert(ws)
    };
    let ready = |pid| Observation::Phase { phase: Phase::Ready, supervisor: SupervisorIdentity { pid, created: 100 }, voyage: None };
    // Three of the observer's rounds (`observer::POLL_INTERVAL`, 2 s).
    let stops = |ws: Arc<Workspace>| async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
        while ws.phase() != Phase::Stopped && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        ws.phase() == Phase::Stopped
    };

    let old = register();
    observe(&old, ready(1));
    ensure_running(&reg, &old);
    assert!(stops(old.clone()).await, "test setup: the observer polls the loaded row");

    let new = register();
    let registered = reg.resolve(Some(&old.workspace_id)).expect("the row");
    assert!(Arc::ptr_eq(&registered, &new) && !Arc::ptr_eq(&new, &old), "test setup: the insert replaced the row under its id");
    drop(registered);
    observe(&new, ready(2));
    ensure_running(&reg, &new);
    assert!(
        stops(new.clone()).await,
        "the refreshed row is never observed: it still reads {:?} after three rounds, the row it replaced {:?}",
        new.phase(),
        old.phase()
    );
    let replaced = Arc::downgrade(&old);
    drop(old);
    assert!(replaced.upgrade().is_none(), "the row the insert replaced is still held");
}
