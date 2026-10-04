//! Tests of registry.rs.

use super::*;

#[cfg(test)]
mod capsule_comm_handle_tests {
    // Manager review (S5, Codex finding B8): `capsule_comm_handle` reads
    // back the handle `comm-join.sh`'s own derivation wrote into the
    // self-file the daemon pinned via `SOT_COMM_SELF_FILE` — restored as
    // the FALLBACK `comm_handle_for_workspace` reaches for once a row's
    // declared `agent_handle` (ADR 0046 decision 1's `agent.join`) is
    // empty, so an older `comm-join.sh` (never sends `agent.join`) or a
    // not-yet-declared session still resolves correctly (a capsule has no
    // tmux pane, so `resolve_handle`'s tmux-session match never finds it
    // either way).
    use super::capsule_comm_handle;

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
        userprofile: Option<std::ffi::OsString>,
        sot_self_host: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("SOT_COMM_HOME", &self.sot_comm_home),
                ("HOME", &self.home),
                ("USERPROFILE", &self.userprofile),
                ("SOT_SELF_HOST", &self.sot_self_host),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            home: std::env::var_os("HOME"),
            userprofile: std::env::var_os("USERPROFILE"),
            sot_self_host: std::env::var_os("SOT_SELF_HOST"),
            _serial: serial,
        }
    }

    #[test]
    fn reads_first_line_of_the_pinned_self_file() {
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-capsule-comm-handle-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let self_dir = dir.join("self");
        std::fs::create_dir_all(&self_dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        std::env::set_var("SOT_SELF_HOST", "testhost");
        std::fs::write(
            self_dir.join("testhost__ws-myrepo-1a2b.txt"),
            "myrepo-testhost\nrepo=myrepo\nroot=/home/me/myrepo\n",
        )
        .unwrap();

        assert_eq!(capsule_comm_handle("ws-myrepo-1a2b"), "myrepo-testhost");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_when_self_file_does_not_exist() {
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-capsule-comm-handle-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("SOT_COMM_HOME", &dir);
        std::env::set_var("SOT_SELF_HOST", "testhost");

        assert_eq!(capsule_comm_handle("ws-never-joined-9f9f"), "");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod with_comm_registry_lock_panic_tests {
    // Coordinator hardening: `f` runs inside the caller's `spawn_blocking`,
    // which contains a panic (the awaiting task just sees a `JoinError`) —
    // but the OLD code released `.registry.lock` only on the normal return
    // path, so a panic mid-critical-section left it behind forever. Since
    // the fail-closed fix means nothing force-breaks it any more, every
    // subsequent writer — this daemon's own callers AND every
    // `comm-status.sh` hook on the shared home — would then wedge closed
    // permanently. The held lock's `Drop` must release on unwind
    // too.
    use super::with_comm_registry_lock;

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.sot_comm_home {
                Some(v) => std::env::set_var("SOT_COMM_HOME", v),
                None => std::env::remove_var("SOT_COMM_HOME"),
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            _serial: serial,
        }
    }

    #[test]
    fn a_panicking_closure_still_releases_the_lock() {
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-comm-registry-lock-panic-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        let lock_dir = dir.join(".registry.lock");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_comm_registry_lock(std::time::Duration::from_secs(1), |_reg, _tmp| {
                panic!("boom — simulate a write that panics mid-critical-section");
            })
        }));

        assert!(result.is_err(), "the panic must propagate to the caller");
        assert!(
            !lock_dir.exists(),
            "the lock dir must be released even when `f` panics"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod remove_comm_agents_for_workspace_host_tests {
    // Same shared-registry scenario, exercised through the real prune path:
    // a destroy on host A must remove only host A's row on a session —
    // never host B's same-session row, and never a host-less row (LU5d2:
    // absent `host` is unknown ownership, not a free pass).
    use super::{remove_comm_agents_for_workspace, remove_comm_agents_for_workspace_bounded};

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.sot_comm_home {
                Some(v) => std::env::set_var("SOT_COMM_HOME", v),
                None => std::env::remove_var("SOT_COMM_HOME"),
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            _serial: serial,
        }
    }

    #[test]
    fn by_name_requires_host() {
        // The `by_name` fallback (for a not-yet-joined `spawning` row whose
        // `tmux` is still "") matches on the caller-supplied `agent_name`
        // alone before LU5d2 — a row on another host that happens to share
        // that handle string must survive.
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-comm-registry-by-name-host-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        let registry_path = dir.join("registry.json");
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "same-name": {"host": "hostB"},
                }
            }))
            .unwrap(),
        )
        .unwrap();

        // No live tmux row for this session, so only the `by_name` term is in
        // play; the stored handle matches, but the row's host does not.
        let removed = remove_comm_agents_for_workspace("same-name", "", "host-4");
        assert!(removed.is_empty());

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        assert!(after
            .get("agents")
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("same-name"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn by_workspace_id_prunes_a_manually_joined_handle() {
        // A handle joined by `comm-join.sh --name other` never sets
        // `ws.agent_name`, so `by_name` alone never matches its row — but
        // its row carries the workspace's own `workspace_id`, so the
        // second match term must prune it when that workspace is destroyed.
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-comm-registry-by-workspace-id-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        let registry_path = dir.join("registry.json");
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "manually-joined": {"host": "hostA", "workspace_id": "ws-1"},
                    "other-workspace": {"host": "hostA", "workspace_id": "ws-2"},
                }
            }))
            .unwrap(),
        )
        .unwrap();

        // The driving agent's own handle differs from the manually-joined
        // one, so only the `by_workspace` term can catch it.
        let removed = remove_comm_agents_for_workspace("driver", "ws-1", "hostA");
        assert_eq!(removed, vec!["manually-joined".to_string()]);

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        let agents = after.get("agents").unwrap().as_object().unwrap();
        assert!(
            !agents.contains_key("manually-joined"),
            "the manually-joined handle should have been pruned by workspace_id"
        );
        assert!(
            agents.contains_key("other-workspace"),
            "a row for a different workspace_id must survive"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contended_lock_gives_up_bounded_and_leaves_registry_unchanged() {
        // Mirrors `clear_comm_unread_tests`'s own contended-lock test: the
        // prune used to force-break a stale-looking lock after its bound
        // (200×50ms); since PR #148 F2 fail-closed is the rule the SHELL
        // side already lives by, and this proves the Rust prune now follows
        // it too — via a short bound (`remove_comm_agents_for_workspace_bounded`)
        // so the test doesn't have to wait out the real
        // `COMM_PRUNE_LOCK_BOUND`.
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-comm-registry-prune-lock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        let registry_path = dir.join("registry.json");
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "host-4-be-x": {"host": "host-4"},
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let before = std::fs::read(&registry_path).unwrap();
        // Pre-create the lock dir so the mkdir-spinlock can never acquire it.
        let lock_dir = dir.join(".registry.lock");
        std::fs::create_dir(&lock_dir).unwrap();

        let bound = std::time::Duration::from_millis(150);
        let start = std::time::Instant::now();
        let removed = remove_comm_agents_for_workspace_bounded("", "", "host-4", bound);
        let elapsed = start.elapsed();

        assert!(removed.is_empty(), "a contended lock must prune nothing");
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "bounded spin must not stall the caller: took {elapsed:?}"
        );
        let after = std::fs::read(&registry_path).unwrap();
        assert_eq!(before, after, "a contended lock must fail closed with no write");
        assert!(
            lock_dir.is_dir(),
            "fail-closed means the pre-existing lock dir is left exactly as found — never force-broken"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod clear_comm_unread_tests {
    // ADR 0044 "Viewing clears blue": `workspace.activate { read: true }`
    // flips a `done` row to `idle` and touches NOTHING else. Same
    // guarded()/SOT_COMM_HOME pattern as
    // `remove_comm_agents_for_workspace_host_tests` above —
    // `clear_comm_unread` shares `with_comm_registry_lock` (the lock
    // protocol) and `comm_handle_for_workspace` (the row-binding rule,
    // tmux and capsule alike) with that function and `handle_workspace_list`
    // respectively, so these tests also stand in for both: neither has any
    // other caller-facing behaviour beyond what binds/writes a row here.
    use super::{clear_comm_unread, Workspace};

    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        sot_comm_home: Option<std::ffi::OsString>,
        sot_self_host: Option<std::ffi::OsString>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.sot_comm_home {
                Some(v) => std::env::set_var("SOT_COMM_HOME", v),
                None => std::env::remove_var("SOT_COMM_HOME"),
            }
            match &self.sot_self_host {
                Some(v) => std::env::set_var("SOT_SELF_HOST", v),
                None => std::env::remove_var("SOT_SELF_HOST"),
            }
        }
    }

    fn guarded() -> EnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        EnvGuard {
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            sot_self_host: std::env::var_os("SOT_SELF_HOST"),
            _serial: serial,
        }
    }

    fn temp_home(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "sot-clear-comm-unread-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn write_registry(dir: &std::path::Path, agents: serde_json::Value) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", dir);
        let registry_path = dir.join("registry.json");
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({ "agents": agents })).unwrap(),
        )
        .unwrap();
        registry_path
    }

    // A `"tmux"`-runtime workspace with `session_name = "sot-be-<label>"`
    // (`Workspace::from_label`'s own convention) and the given stored
    // `agent_name`. Registry rows bind to it by `workspace_id`/`host`;
    // the registry's old `tmux` pane field is gone (topology plan §D).
    fn mk_ws(label: &str, agent_name: &str) -> Workspace {
        let mut ws = Workspace::from_label(
            label,
            std::path::PathBuf::from("/p"),
            false,
            "none".into(),
            agent_name.to_string(),
            String::new(),
        );
        // These rows are tmux rows on every platform: a label-built
        // workspace defaults to "capsule" on Windows, which would route the
        // clear through the capsule branch instead of the seeded tmux row.
        ws.runtime = "tmux".to_string();
        ws
    }

    #[test]
    fn same_session_row_on_another_host_survives_untouched() {
        let _guard = guarded();
        let dir = temp_home("otherhost");
        let registry_path = write_registry(
            &dir,
            serde_json::json!({
                "host-2-be-x": {
                    "host": "hostB",
                    "state": "done",
                    "done": true,
                    "summary": "not yours",
                    "status_at": "2026-09-08T00:00:00Z",
                },
            }),
        );
        let before = std::fs::read(&registry_path).unwrap();

        clear_comm_unread(&mk_ws("x", ""), "host-4");

        let after = std::fs::read(&registry_path).unwrap();
        assert_eq!(before, after, "a foreign host's row must never be touched");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_done_states_are_never_touched() {
        let _guard = guarded();
        for state in ["blocked", "waiting", "working", "idle"] {
            let dir = temp_home(&format!("state-{state}"));
            let registry_path = write_registry(
                &dir,
                serde_json::json!({
                    "host-4-be-x": {
                        "host": "host-4",
                        "state": state,
                        "summary": "unchanged",
                        "status_at": "2026-09-08T00:00:00Z",
                    },
                }),
            );
            let before = std::fs::read(&registry_path).unwrap();

            clear_comm_unread(&mk_ws("x", ""), "host-4");

            let after = std::fs::read(&registry_path).unwrap();
            assert_eq!(before, after, "state {state} must never be rewritten");

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn missing_registry_is_a_silent_no_op() {
        let _guard = guarded();
        let dir = temp_home("missing");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        // No registry.json written at all.

        clear_comm_unread(&mk_ws("x", "agent"), "host-4");
        assert!(!dir.join("registry.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_registry_is_a_silent_no_op() {
        let _guard = guarded();
        let dir = temp_home("malformed");
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        let registry_path = dir.join("registry.json");
        std::fs::write(&registry_path, b"not json{{{").unwrap();
        let before = std::fs::read(&registry_path).unwrap();

        clear_comm_unread(&mk_ws("x", "agent"), "host-4");

        let after = std::fs::read(&registry_path).unwrap();
        assert_eq!(before, after);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contended_lock_gives_up_bounded_and_leaves_registry_unchanged() {
        let _guard = guarded();
        let dir = temp_home("locked");
        let registry_path = write_registry(
            &dir,
            serde_json::json!({
                "host-4-be-x": {
                    "host": "host-4",
                    "state": "done",
                    "done": true,
                    "summary": "probe summary",
                    "status_at": "2026-09-08T00:00:00Z",
                },
            }),
        );
        let before = std::fs::read(&registry_path).unwrap();
        // Pre-create the lock dir so the mkdir-spinlock inside
        // `clear_comm_unread` can never acquire it.
        std::fs::create_dir(dir.join(".registry.lock")).unwrap();

        // No wall-clock assertion: the spin is bounded by a fixed iteration
        // count, and a loaded CI runner (the macOS leg took 2.6 s for the
        // ~1 s spin) turns any elapsed-time gate into a flake. The property
        // under test is fail-closed: the registry is untouched.
        clear_comm_unread(&mk_ws("x", ""), "host-4");
        let after = std::fs::read(&registry_path).unwrap();
        assert_eq!(before, after, "a contended lock must fail closed with no write");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capsule_row_resolved_via_declared_agent_handle_clears_to_idle() {
        // ADR 0046 decision 1: a capsule workspace has no tmux pane and
        // (unless explicitly requested) no stored `agent_name` either —
        // the ONLY way to its registry row is the handle the session
        // DECLARED via `agent.join` (`Workspace.agent_handle`), never a
        // daemon-side self-file read-back any more. `comm_handle_for_workspace`
        // must try that field FIRST for every runtime, same as
        // `handle_workspace_list` does, or these rows — the ones that
        // actually pile up blue for a capsule-only user — would never
        // clear.
        let _guard = guarded();
        let dir = temp_home("capsule");
        let registry_path = write_registry(
            &dir,
            serde_json::json!({
                "capsule-handle-x": {
                    "host": "host-4",
                    "state": "done",
                    "done": true,
                    "summary": "probe summary",
                    "status_at": "2026-09-08T00:00:00Z",
                },
            }),
        );
        std::env::set_var("SOT_SELF_HOST", "host-4");

        let mut ws = mk_ws("capsuleprobe", "");
        ws.runtime = "capsule".to_string();
        ws.agent_handle = std::sync::Mutex::new("capsule-handle-x".to_string());

        clear_comm_unread(&ws, "host-4");

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        let row = &after["agents"]["capsule-handle-x"];
        assert_eq!(row["state"], "idle");
        assert!(row.get("done").is_none(), "the done fact must be removed");
        assert_eq!(row["summary"], "probe summary");
        assert_eq!(row["status_at"], "2026-09-08T00:00:00Z");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn done_fact_removed_but_state_untouched_when_a_floor_or_wait_outranks_it() {
        // ADR 0044 amendment: `done` can sit under a running `floor` or a
        // `waiting` (the model stamped `done` mid-turn, or a wait was
        // declared after). Viewing removes the stale `done` fact so it
        // doesn't reappear once the floor/wait lifts, but must NOT touch the
        // higher-priority display the reduction already picked.
        let _guard = guarded();
        let dir = temp_home("done-under-waiting");
        let registry_path = write_registry(
            &dir,
            serde_json::json!({
                "host-4-be-x": {
                    "host": "host-4",
                    "state": "waiting",
                    "waiting": "job",
                    "done": true,
                    "summary": "job",
                    "status_at": "2026-09-08T00:00:00Z",
                },
            }),
        );

        // A non-empty `agent_name` so `comm_handle_for_workspace` actually
        // resolves to this row (an empty one, as most sibling tests here
        // use, means "nothing to bind to" and the call returns before ever
        // reaching the row — fine for an untouched-either-way assertion,
        // not for this one, which needs the write to actually run).
        clear_comm_unread(&mk_ws("x", "host-4-be-x"), "host-4");

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        let row = &after["agents"]["host-4-be-x"];
        assert_eq!(row["state"], "waiting", "state must stay waiting, not flip to idle");
        assert!(row.get("done").is_none(), "the stale done fact must still be removed");
        assert_eq!(row["waiting"], "job", "waiting must survive untouched");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_done_fact_at_all_is_a_no_op() {
        let _guard = guarded();
        let dir = temp_home("no-done-fact");
        let registry_path = write_registry(
            &dir,
            serde_json::json!({
                "host-4-be-x": {
                    "host": "host-4",
                    "state": "idle",
                    "summary": "unchanged",
                    "status_at": "2026-09-08T00:00:00Z",
                },
            }),
        );
        let before = std::fs::read(&registry_path).unwrap();

        clear_comm_unread(&mk_ws("x", "host-4-be-x"), "host-4");

        let after = std::fs::read(&registry_path).unwrap();
        assert_eq!(before, after, "a row with no done fact must never be rewritten");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capsule_row_with_empty_agent_name_and_no_declared_handle_is_a_no_op() {
        // The fallback half of `comm_handle_for_workspace`'s capsule arm:
        // no declared `agent_handle` AND an empty stored `agent_name`
        // resolves to an empty handle, same as the tmux path's "nothing
        // to bind to".
        let _guard = guarded();
        let dir = temp_home("capsule-unbound");
        let registry_path = write_registry(
            &dir,
            serde_json::json!({
                "someone-else": {
                    "host": "host-4",
                    "state": "done",
                    "summary": "not yours",
                    "status_at": "2026-09-08T00:00:00Z",
                },
            }),
        );
        std::env::set_var("SOT_SELF_HOST", "host-4");
        let before = std::fs::read(&registry_path).unwrap();

        let mut ws = mk_ws("capsuleprobe2", "");
        ws.runtime = "capsule".to_string();
        // No agent_handle declared at all.

        clear_comm_unread(&ws, "host-4");

        let after = std::fs::read(&registry_path).unwrap();
        assert_eq!(before, after, "an unbound capsule row must never fall through to an unrelated handle");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod unix_now_secs_tests {
    use super::unix_now_secs;

    fn wall_secs() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    }

    #[test]
    fn unix_now_secs_is_the_wall_clock_in_whole_seconds() {
        let before = wall_secs();
        let got = unix_now_secs();
        let after = wall_secs();
        assert!(before <= got && got <= after, "{before} <= {got} <= {after}");
    }
}
