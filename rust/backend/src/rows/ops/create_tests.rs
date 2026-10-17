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
    struct EnvPinned {
        _guards: [crate::paths::EnvGuard; 3],
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvPinned {
        fn new(dir: &Path) -> Self {
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
