//! Tests of registry.rs.

use super::*;

#[cfg(test)]
mod with_comm_registry_lock_panic_tests {
    // Hardening: `f` runs inside the caller's `spawn_blocking`,
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
    fn by_name_leaves_an_entry_that_another_row_now_owns() {
        // ADR 0049: the handle moved to a newer row, whose `workspace_id` the
        // entry carries; destroying the older row, whose stored name is
        // still that handle, must not take the newer row's entry.
        let _guard = guarded();
        let dir = std::env::temp_dir().join(format!(
            "sot-comm-registry-by-name-moved-test-{}-{}",
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
                    "m5-shared": {"host": "host-4", "workspace_id": "newer-id"},
                    "m5-bare": {"host": "host-4"},
                }
            }))
            .unwrap(),
        )
        .unwrap();

        assert!(remove_comm_agents_for_workspace("m5-shared", "older-id", "host-4").is_empty());
        // An entry with no `workspace_id` is still pruned by name.
        assert_eq!(remove_comm_agents_for_workspace("m5-bare", "older-id", "host-4"), vec!["m5-bare".to_string()]);

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        let agents = after.get("agents").unwrap().as_object().unwrap();
        assert!(agents.contains_key("m5-shared"), "the newer row's entry must survive");
        assert_eq!(agents.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn by_workspace_id_prunes_a_manually_joined_handle() {
        // A handle the row's session derived at its join never sets
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
        // As a work-state write's contended-lock test does: the
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
