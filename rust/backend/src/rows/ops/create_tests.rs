//! Tests of create.rs: the duplicate-root and same-slug gates, the refusal test pinning every refusal and the order of the gates, and the root a create stores.

use super::*;

mod duplicate_root_tests {
    use super::find_other_workspace_with_root;
    use crate::rows::{Workspace, Workspaces};
    use std::path::Path;

    /// A fresh folder per test, named by tempfile's random suffix. The caller
    /// binds the guard, so the folder is removed when the test ends and also
    /// when an assertion panics.
    fn scratch_dir(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("sot-duproot-{tag}-"))
            .tempdir()
            .unwrap()
    }

    fn ws(label: &str, root: &Path) -> Workspace {
        Workspace::from_label(label, root.to_path_buf(), false, "none".into(), String::new(), String::new())
    }

    fn reg(rows: Vec<Workspace>) -> Workspaces {
        let r = Workspaces::new();
        for w in rows {
            r.insert(w);
        }
        r
    }

    #[test]
    fn same_root_different_slug_is_found() {
        let scratch = scratch_dir("hit");
        let root = scratch.path();
        let existing = reg(vec![ws("sot", root)]);
        let canon = root.canonicalize().unwrap();
        let hit = find_other_workspace_with_root(&canon, "ship-of-tools", &existing)
            .expect("a second identity for one root must be caught");
        assert_eq!(hit.slug, "sot");
    }

    #[test]
    fn same_slug_is_invisible_so_refresh_stays_allowed() {
        // A same-slug create is Workspaces::insert's id-preserving metadata
        // refresh; the gate must not turn that idempotent path into an error.
        let scratch = scratch_dir("refresh");
        let root = scratch.path();
        let existing = reg(vec![ws("sot", root)]);
        let canon = root.canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "sot", &existing).is_none());
    }

    #[test]
    fn different_roots_pass() {
        let a = scratch_dir("a");
        let b = scratch_dir("b");
        let existing = reg(vec![ws("sot", a.path())]);
        let canon = b.path().canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "other", &existing).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_spelling_of_a_registered_root_still_collides() {
        // The incident shape with a twist: the duplicate is registered via a
        // symlink to the same directory. Canonical comparison must see through
        // it — path-string comparison would not.
        let scratch = scratch_dir("real");
        let root = scratch.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let link = scratch.path().join("link");
        std::os::unix::fs::symlink(&root, &link).expect("create symlink");
        let existing = reg(vec![ws("sot", &link)]);
        let canon = root.canonicalize().unwrap();
        let hit = find_other_workspace_with_root(&canon, "ship-of-tools", &existing)
            .expect("symlinked duplicate must be caught");
        assert_eq!(hit.slug, "sot");
    }

    /// ADR 0042 amendment: the inert default anchor (the default row with no
    /// agent, root = the home dir, any runtime) is not a session, so a session
    /// created at that root passes the gate — while a default row that
    /// carries an agent is still refused.
    #[test]
    fn inert_default_anchor_does_not_block_a_session_at_its_root() {
        let scratch = scratch_dir("anchor");
        let root = scratch.path();
        let canon = root.canonicalize().unwrap();
        let existing = Workspaces::new();
        let mut anchor = ws("local", root);
        anchor.runtime = "capsule".to_string();
        let anchor = existing.insert(anchor);
        existing.set_default(&anchor.workspace_id);
        assert!(
            find_other_workspace_with_root(&canon, "home-session", &existing).is_none(),
            "the inert anchor must not claim its root against a real session"
        );
        // Control: the default row WITH an agent is a real session and is
        // still caught (same-slug insert keeps the id, so it stays default).
        let mut sot = ws("local", root);
        sot.agent = std::sync::Mutex::new("claude".to_string());
        existing.insert(sot);
        assert!(find_other_workspace_with_root(&canon, "home-session", &existing).is_some());
    }

    #[test]
    fn registered_root_that_no_longer_resolves_is_skipped_not_fatal() {
        // A workspace whose root was deleted is Phase 2's (reap) problem; the
        // gate must neither match it nor error on it.
        let scratch = scratch_dir("gone");
        let gone = scratch.path().join("gone");
        let live = scratch_dir("live");
        let existing = reg(vec![ws("dead", &gone)]);
        let canon = live.path().canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "other", &existing).is_none());
    }
}

mod label_in_use_tests {
    use super::same_slug_row_in_use;
    use crate::rows::workspace::{Observation, Phase, SupervisorIdentity};
    use crate::rows::{Workspace, Workspaces};

    fn row(label: &str, runtime: &str, observed: Option<Phase>) -> Workspace {
        let mut w = Workspace::from_label(
            label,
            std::env::temp_dir(),
            false,
            "none".into(),
            String::new(),
            String::new(),
        );
        w.runtime = runtime.to_string();
        if let Some(phase) = observed {
            assert!(w.apply_phase_observation(Observation::Phase {
                phase,
                supervisor: SupervisorIdentity { pid: 1, created: 1 },
                voyage: Some(uuid::Uuid::from_u128(1)),
            }));
            assert_eq!(w.phase(), phase);
        }
        w
    }

    fn reg(w: Workspace) -> Workspaces {
        let r = Workspaces::new();
        r.insert(w);
        r
    }

    #[test]
    fn stopped_capsule_row_is_not_in_use_so_the_refresh_stays_allowed() {
        let r = reg(row("sot", "capsule", None));
        assert!(same_slug_row_in_use("sot", &r).is_none());
    }

    #[test]
    fn every_observed_phase_is_in_use() {
        for phase in [
            Phase::Starting,
            Phase::Ready,
            Phase::Ending,
            Phase::EndedNoRespawn,
            Phase::Terminal,
        ] {
            let r = reg(row("sot", "capsule", Some(phase)));
            let hit = same_slug_row_in_use("sot", &r)
                .unwrap_or_else(|| panic!("phase {phase:?} must be in use"));
            assert_eq!(hit.slug, "sot");
        }
    }

    #[test]
    fn a_non_capsule_row_is_in_use_even_when_stopped() {
        let r = reg(row("sot", "tmux", None));
        assert!(same_slug_row_in_use("sot", &r).is_some());
    }

    #[test]
    fn a_different_slug_is_not_this_gates_question() {
        let r = reg(row("sot", "capsule", Some(Phase::Ready)));
        assert!(same_slug_row_in_use("other", &r).is_none());
    }
}

mod refusal_tests {
    use super::handle_workspace_create;
    use crate::session::Session;
    use crate::rows::{Workspace, Workspaces};
    use serde_json::{json, Value};
    use std::path::Path;
    use tokio::sync::broadcast;

    /// A fresh folder per case, named by tempfile's random suffix. The caller
    /// binds the guard, so the folder is removed when the case ends and also
    /// when an assertion panics.
    fn scratch_dir(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("sot-createref-{tag}-"))
            .tempdir()
            .unwrap()
    }

    fn seeded(label: &str, root: &Path, runtime: &str) -> Workspace {
        let mut w = Workspace::from_label(label, root.to_path_buf(), false, "none".into(), String::new(), String::new());
        w.runtime = runtime.to_string();
        w
    }

    /// Pins the three folders the config and state roots resolve under (the two XDG ones, and the Windows local-app-data
    /// one) to the test's own folder, under the crate-wide env lock; the guards restore them before the lock drops.
    pub(super) struct EnvPinned {
        _guards: [crate::paths::EnvGuard; 3],
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvPinned {
        pub(super) fn new(dir: &Path) -> Self {
            let serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let guards = ["XDG_CONFIG_HOME", "XDG_STATE_HOME", "LOCALAPPDATA"].map(crate::paths::EnvGuard::capture);
            std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
            std::env::set_var("XDG_STATE_HOME", dir.join("state"));
            std::env::set_var("LOCALAPPDATA", dir.join("local"));
            let (state, config) = (crate::paths::state_dir(), crate::rows::store::app_config_dir());
            assert!(state.starts_with(dir) && config.starts_with(dir), "the pin missed a root: {state:?}, {config:?}");
            EnvPinned { _guards: guards, _serial: serial }
        }
    }

    /// Calls the handler once and returns the single reply frame's payload; the registry's ids must not change.
    /// Every payload carries an account that cannot resolve, so a regressed gate stops at `unknown_account`
    /// (the account check comes after every gate these cases pin) before any row, file or capsule exists.
    async fn refused(workspaces: &Workspaces, mut payload: Value) -> Value {
        payload["account"] = json!("no-such-account");
        let ids = |w: &Workspaces| {
            let mut v: Vec<String> = w.list().iter().map(|r| r.workspace_id.clone()).collect();
            v.sort();
            v
        };
        let before = ids(workspaces);
        let (ws_events, _rx) = broadcast::channel(16);
        let out = handle_workspace_create(1, payload, &Session::new(), workspaces, &ws_events)
            .await
            .expect("a refusal is a reply, not an error");
        assert_eq!(out.len(), 1);
        let (frame, blob) = &out[0];
        assert!(blob.is_none());
        assert_eq!(frame.op, sot_protocol::op::WORKSPACE_CREATE);
        assert_eq!(ids(workspaces), before);
        frame.payload.clone()
    }

    #[tokio::test]
    async fn workspace_create_refusals_are_unchanged() {
        let env_dir = scratch_dir("env");
        let _env = EnvPinned::new(env_dir.path());
        // 1. missing root
        let gone = env_dir.path().join("gone");
        let got = refused(&Workspaces::new(), json!({"label": "c1", "project_root": gone.to_string_lossy()})).await;
        assert_eq!(got, json!({
            "error": format!("project_root does not exist: {}", gone.display()),
            "code": "no_such_path",
        }));

        // 2. a file as root
        let file_dir = scratch_dir("file");
        let file = file_dir.path().join("plain.txt");
        std::fs::write(&file, b"x").unwrap();
        let got = refused(&Workspaces::new(), json!({"label": "c2", "project_root": file.to_string_lossy()})).await;
        assert_eq!(got, json!({
            "error": format!("project_root is not a directory: {}", file.display()),
            "code": "not_a_directory",
        }));

        // 3. a root already held by another row
        let dup_dir = scratch_dir("dup");
        let root = dup_dir.path();
        let reg = Workspaces::new();
        let other = reg.insert(seeded("other", root, "capsule"));
        let got = refused(&reg, json!({"label": "c3", "project_root": root.to_string_lossy()})).await;
        assert_eq!(got, json!({
            "error": "project_root is already registered as workspace 'other' (slug 'other')",
            "code": "duplicate_root",
            "existing": {"workspace_id": other.workspace_id, "slug": "other", "label": "other"},
        }));

        // 4. the same label again while its row is in use
        let reg = Workspaces::new();
        let busy_seed = scratch_dir("busy-seed");
        let busy = reg.insert(seeded("busy", busy_seed.path(), "tmux"));
        let busy_new = scratch_dir("busy-new");
        let got = refused(&reg, json!({"label": "busy", "project_root": busy_new.path().to_string_lossy()})).await;
        assert_eq!(got, json!({
            "error": "workspace 'busy' (slug 'busy') is in use (tmux row, phase 'stopped'): attach to it, or destroy it before creating it again",
            "code": "label_in_use",
            "existing": {"workspace_id": busy.workspace_id, "slug": "busy", "label": "busy", "phase": "stopped"},
        }));

        // 5. 6. 7. a bad agent name, agent kind and runtime
        let agent_dir = scratch_dir("agent");
        let root = agent_dir.path().to_string_lossy().into_owned();
        let got = refused(&Workspaces::new(), json!({"label": "c5", "project_root": root, "agent_name": "a b"})).await;
        assert_eq!(got, json!({
            "error": "invalid agent_name \"a b\" (want 1-64 chars of [A-Za-z0-9._-])",
            "code": "bad_agent_name",
        }));
        let got = refused(&Workspaces::new(), json!({"label": "c6", "project_root": root, "agent": "robot"})).await;
        assert_eq!(got, json!({
            "error": "unknown agent kind 'robot' (want claude | codex | none)",
            "code": "bad_agent",
        }));
        let got = refused(&Workspaces::new(), json!({"label": "c7", "project_root": root, "runtime": "tmux"})).await;
        assert_eq!(got, json!({
            "error": "unknown runtime \"tmux\" (want \"capsule\" or \"\")",
            "code": "bad_runtime",
        }));

        // Precedence: a payload tripping several gates gets the first gate's refusal.
        // 1. a missing root wins over a bad agent name, kind and runtime
        let got = refused(&Workspaces::new(), json!({
            "label": "p1", "project_root": gone.to_string_lossy(),
            "agent_name": "a b", "agent": "robot", "runtime": "tmux",
        })).await;
        assert_eq!(got["code"], "no_such_path");
        // 2. a held root wins over a label in use
        let held_dir = scratch_dir("p2-held");
        let held = held_dir.path();
        let reg = Workspaces::new();
        let other = reg.insert(seeded("other", held, "capsule"));
        let p2_busy = scratch_dir("p2-busy");
        reg.insert(seeded("busy", p2_busy.path(), "tmux"));
        let got = refused(&reg, json!({"label": "busy", "project_root": held.to_string_lossy()})).await;
        assert_eq!(got, json!({
            "error": "project_root is already registered as workspace 'other' (slug 'other')",
            "code": "duplicate_root",
            "existing": {"workspace_id": other.workspace_id, "slug": "other", "label": "other"},
        }));
        // 3. a bad agent name wins over a bad kind and runtime
        let got = refused(&Workspaces::new(), json!({
            "label": "p3", "project_root": root, "agent_name": "a b", "agent": "robot", "runtime": "tmux",
        })).await;
        assert_eq!(got["code"], "bad_agent_name");
        // 4. a bad agent kind wins over a bad runtime
        let got = refused(&Workspaces::new(), json!({
            "label": "p4", "project_root": root, "agent": "robot", "runtime": "tmux",
        })).await;
        assert_eq!(got["code"], "bad_agent");
    }
}

mod root_tests {
    use super::requested_root;
    use serde_json::json;
    use std::path::PathBuf;

    fn req(root: &str) -> sot_protocol::WorkspaceCreateReq {
        serde_json::from_value(json!({"label": "r", "project_root": root})).expect("a create request")
    }

    #[test]
    fn a_plain_root_is_created_as_sent() {
        assert_eq!(requested_root(&req("/home/k/proj")), PathBuf::from("/home/k/proj"));
    }

    /// A verbatim root is created plain, the form the store's load gives it, so the row's working directory and its
    /// `SOT_WORKSPACE_ROOT` are never `\\?\…`.
    #[cfg(windows)]
    #[test]
    fn a_verbatim_root_is_created_plain() {
        assert_eq!(requested_root(&req(r"\\?\C:\Users\k\proj")), PathBuf::from(r"C:\Users\k\proj"));
        assert_eq!(requested_root(&req(r"\\?\UNC\srv\share\x")), PathBuf::from(r"\\srv\share\x"));
    }
}

mod rollback_tests {
    use super::handle_workspace_create;
    use crate::rows::{Workspace, Workspaces};
    use crate::session::Session;
    use serde_json::json;
    use tokio::sync::broadcast;

    /// A same-slug create of a row the daemon reads as stopped whose spawn fails (no sot-capsule beside the test
    /// binary) leaves the prior registration and its toml as they were, byte for byte.
    #[tokio::test]
    async fn failed_same_slug_create_restores_the_prior_row_and_toml() {
        let env_dir = tempfile::Builder::new().prefix("sot-createrb-").tempdir().unwrap();
        let _env = super::refusal_tests::EnvPinned::new(env_dir.path());
        let old_root = env_dir.path().join("old");
        let new_root = env_dir.path().join("new");
        std::fs::create_dir_all(&old_root).unwrap();
        std::fs::create_dir_all(&new_root).unwrap();
        let reg = Workspaces::new();
        let mut seed = Workspace::from_label("rb", old_root.clone(), false, "none".into(), String::new(), "old task".into());
        seed.runtime = "capsule".into();
        let prior = reg.insert(seed);
        let toml = crate::rows::store::save(&prior).expect("prior toml");
        let before = std::fs::read(&toml).unwrap();

        let (ws_events, _rx) = broadcast::channel(16);
        let payload = json!({"label": "rb", "project_root": new_root.to_string_lossy(), "agent": "none", "task": "new task"});
        let out = handle_workspace_create(1, payload, &Session::new(), &reg, &ws_events).await.unwrap();
        assert_eq!(out[0].0.payload["code"], "capsule_spawn_failed", "{:?}", out[0].0.payload);

        let rows = reg.list();
        assert_eq!(rows.len(), 1, "the prior row stays");
        assert_eq!(rows[0].workspace_id, prior.workspace_id);
        assert_eq!(rows[0].project_root, old_root);
        assert_eq!(rows[0].task, "old task");
        assert_eq!(std::fs::read(&toml).unwrap(), before, "the toml is the prior bytes");
    }

    /// A create that added the row removes it and its toml when the spawn fails.
    #[tokio::test]
    async fn failed_new_create_leaves_nothing() {
        let env_dir = tempfile::Builder::new().prefix("sot-createrb-").tempdir().unwrap();
        let _env = super::refusal_tests::EnvPinned::new(env_dir.path());
        let root = env_dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let reg = Workspaces::new();
        let (ws_events, _rx) = broadcast::channel(16);
        let payload = json!({"label": "fresh", "project_root": root.to_string_lossy(), "agent": "none"});
        let out = handle_workspace_create(1, payload, &Session::new(), &reg, &ws_events).await.unwrap();
        assert_eq!(out[0].0.payload["code"], "capsule_spawn_failed", "{:?}", out[0].0.payload);
        assert!(reg.list().is_empty());
        assert!(!crate::rows::store::toml_path_for("fresh").exists());
    }

    /// Seeds the capsule row "rb" (task "old task", root `old`) with its toml; returns the row and the toml's bytes.
    fn seed_prior(reg: &Workspaces, env_dir: &std::path::Path)
        -> (std::sync::Arc<Workspace>, Vec<u8>, std::path::PathBuf) {
        let old_root = env_dir.join("old");
        std::fs::create_dir_all(&old_root).unwrap();
        let mut seed = Workspace::from_label("rb", old_root, false, "none".into(), String::new(), "old task".into());
        seed.runtime = "capsule".into();
        let prior = reg.insert(seed);
        let toml = crate::rows::store::save(&prior).expect("prior toml");
        let before = std::fs::read(&toml).unwrap();
        (prior, before, toml)
    }

    /// Spawns a create of "rb" with `task` and its own root; the join handle yields the reply's code.
    fn spawn_create(reg: &Workspaces, env_dir: &std::path::Path, task: &str) -> tokio::task::JoinHandle<String> {
        let root = env_dir.join(task);
        std::fs::create_dir_all(&root).unwrap();
        let reg = reg.clone();
        let task = task.to_string();
        tokio::spawn(async move {
            let (ws_events, _rx) = broadcast::channel(16);
            let payload = json!({"label": "rb", "project_root": root.to_string_lossy(), "agent": "none", "task": task});
            let out = handle_workspace_create(1, payload, &Session::new(), &reg, &ws_events).await.unwrap();
            out[0].0.payload["code"].as_str().unwrap_or("").to_string()
        })
    }

    async fn until_task(reg: &Workspaces, task: &str) {
        for _ in 0..500 {
            if reg.list().iter().any(|w| w.task == task) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("no row with task {task}");
    }

    /// A destroy that wins the row guard while a create waits for it stays destroyed: the create's rollback
    /// restores nothing, in the registry or on disk.
    #[tokio::test]
    async fn a_destroy_during_a_failing_create_stays_destroyed() {
        let env_dir = tempfile::Builder::new().prefix("sot-createrb-").tempdir().unwrap();
        let _env = super::refusal_tests::EnvPinned::new(env_dir.path());
        let reg = Workspaces::new();
        let (prior, _before, toml) = seed_prior(&reg, env_dir.path());
        let guard = reg.capsule_guard(&prior.workspace_id).unwrap();
        let held = guard.lock().await;
        let create = spawn_create(&reg, env_dir.path(), "new task");
        until_task(&reg, "new task").await;
        assert!(crate::rows::run::end::remove_row_files("rb"));
        reg.remove_by_id(&prior.workspace_id);
        drop(held);
        create.await.unwrap();
        let rows: Vec<String> = reg.list().iter().map(|w| w.task.clone()).collect();
        assert!(rows.is_empty(), "rows after: {rows:?}");
        assert!(!toml.exists(), "the destroyed row's toml stays gone");
    }

    /// A second create of a slug being created is refused as in use, so the first one's rollback restores the
    /// prior row and toml, never another create's metadata.
    #[tokio::test]
    async fn two_same_slug_creates_leave_the_prior_row_when_both_fail() {
        let env_dir = tempfile::Builder::new().prefix("sot-createrb-").tempdir().unwrap();
        let _env = super::refusal_tests::EnvPinned::new(env_dir.path());
        let reg = Workspaces::new();
        let (prior, before, toml) = seed_prior(&reg, env_dir.path());
        let guard = reg.capsule_guard(&prior.workspace_id).unwrap();
        let held = guard.lock().await;
        let a = spawn_create(&reg, env_dir.path(), "A");
        until_task(&reg, "A").await;
        let b = spawn_create(&reg, env_dir.path(), "B");
        let refused = tokio::time::timeout(std::time::Duration::from_secs(5), b).await;
        assert_eq!(refused.expect("B waited on the row guard").unwrap(), "label_in_use",
            "a create of a slug being created is refused");
        drop(held);
        assert_eq!(a.await.unwrap(), "capsule_spawn_failed");
        let rows: Vec<String> = reg.list().iter().map(|w| w.task.clone()).collect();
        assert_eq!(rows, vec!["old task".to_string()]);
        assert_eq!(std::fs::read(&toml).unwrap(), before, "the toml is the prior bytes");
    }

    /// A toml the create cannot read (anything but absent) refuses it before the insert.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_toml_refuses_the_create_before_the_insert() {
        use std::os::unix::fs::PermissionsExt;
        let env_dir = tempfile::Builder::new().prefix("sot-createrb-").tempdir().unwrap();
        let _env = super::refusal_tests::EnvPinned::new(env_dir.path());
        let reg = Workspaces::new();
        let (_prior, _before, toml) = seed_prior(&reg, env_dir.path());
        std::fs::set_permissions(&toml, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&toml).is_ok() {
            return; // running as a user the mode does not bind
        }
        let code = spawn_create(&reg, env_dir.path(), "new task").await.unwrap();
        assert!(toml.exists(), "the toml the create could not read stays");
        std::fs::set_permissions(&toml, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(code, "capsule_spawn_failed");
        let rows: Vec<String> = reg.list().iter().map(|w| w.task.clone()).collect();
        assert_eq!(rows, vec!["old task".to_string()], "no insert happened");
    }
}

mod startup_cleanup_tests {
    use super::handle_workspace_create;
    use crate::lifecycle::lease::{self, HeldRecord};
    use crate::rows::{Workspace, Workspaces};
    use crate::session::Session;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::broadcast;

    /// The startup Cleanup lists the rows registered before any listener binds and ends each by its id, once one of
    /// `LANE_CONCURRENCY` permits is free; a same-label create keeps that id. Here as many rows as there are permits,
    /// each with a file where its state folder goes, have their ends refused until the test removes the files, so the
    /// Cleanup reaches the row "dup" only after a window's create of "dup". Neither a create the Cleanup refuses nor
    /// one it lets through may end with the Cleanup removing the row the create made.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_same_label_create_during_the_startup_cleanup_is_never_the_cleanups_to_end() {
        let env_dir = tempfile::Builder::new().prefix("sot-createcl-").tempdir().unwrap();
        let _env = super::refusal_tests::EnvPinned::new(env_dir.path());
        // The Cleanup's end prunes the comm registry.
        let _comm = crate::paths::EnvGuard::capture("SOT_COMM_HOME");
        std::env::set_var("SOT_COMM_HOME", env_dir.path().join("comm"));
        let state_root = sot_platform::host::state_dir::sot_state_dir().expect("the pinned state root");
        std::fs::create_dir_all(&state_root).unwrap();
        let reg = Workspaces::new();
        let seed = |label: &str, task: &str| {
            let root = env_dir.path().join(label);
            std::fs::create_dir_all(&root).unwrap();
            let mut w = Workspace::from_label(label, root, false, "none".into(), String::new(), task.into());
            w.runtime = "capsule".into();
            w
        };
        let row = |label: &str| {
            let w = reg.insert(seed(label, "listed"));
            crate::rows::store::save(&w).expect("toml");
            w
        };
        let blockers: Vec<std::path::PathBuf> = (1..=crate::rows::run::resume::LANE_CONCURRENCY)
            .map(|i| {
                let w = row(&format!("a{i}"));
                let dir = crate::rows::spawn::state_root::state_dir_for(&state_root.canonicalize().unwrap(), &w.workspace_id);
                std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
                std::fs::write(&dir, b"").unwrap();
                dir
            })
            .collect();
        let listed = row("dup");
        let record = state_root.join(sot_protocol::ops::lease::HELD_RECORD_FILE);
        let boot = sot_platform::identity::challenge::boot_identity().expect("this host's boot");
        let closing = HeldRecord { v: 1, boot, holders: vec![], handover_until_ms: None, closing: true, not_ended: 0, forget: vec![] };
        lease::write_or_delete(&record, &closing).unwrap();

        let (ws_events, _rx) = broadcast::channel(64);
        let _leases = crate::lifecycle::startup::begin(Some(state_root.clone()), &reg, &ws_events);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            reg.resolve(Some(&listed.workspace_id)).is_some_and(|w| Arc::ptr_eq(&w, &listed)),
            "test setup: the Cleanup reached dup before the blockers let it go"
        );

        // A window's create of "dup"; its spawn fails here (no sot-capsule beside the test binary).
        let payload = json!({"label": "dup", "project_root": env_dir.path().join("dup").to_string_lossy(), "agent": "none"});
        let reply = handle_workspace_create(1, payload, &Session::new(), &reg, &ws_events).await.unwrap();
        // A create of "dup" whose spawn succeeds, as `handle_workspace_create` makes it: the slug's claim, the insert
        // under the kept id, the toml. The spawn and the observer it starts do not change what the Cleanup removes.
        let created = reg.claim_create("dup").map(|_claim| {
            let w = reg.insert(seed("dup", "created"));
            crate::rows::store::save(&w).expect("toml");
            w
        });
        for blocker in &blockers {
            std::fs::remove_file(blocker).unwrap();
        }
        // The Cleanup ends by writing its record, and with nothing left not ended that deletes it.
        for _ in 0..300 {
            if !record.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!record.exists(), "the Cleanup did not finish in 30 s");

        if let Some(created) = &created {
            let kept = reg.resolve(Some(&created.workspace_id)).is_some_and(|w| Arc::ptr_eq(&w, created));
            assert!(
                kept && crate::rows::store::toml_path_for("dup").exists(),
                "the startup Cleanup ended row {} after a same-label create registered it (the create kept the listed \
                 row's id): registered {kept}, toml {}",
                created.workspace_id,
                crate::rows::store::toml_path_for("dup").exists()
            );
        }
        assert_eq!(reply[0].0.payload["code"], "label_in_use", "{:?}", reply[0].0.payload);
        assert!(reg.resolve(Some("dup")).is_none(), "the Cleanup kept the row it listed");
        assert!(reg.claim_create("dup").is_some(), "the label is still held after the Cleanup");
    }
}
