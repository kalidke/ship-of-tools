//! Tests of the write that ends `registry.rs`'s two locked closures: the bytes it leaves and what a failed temp write
//! changes.

use super::*;

mod prune_write_tests {
    use super::remove_comm_agents_for_workspace_in;

    fn seed(env: &mut crate::comm::Env, tag: &str) -> (std::path::PathBuf, std::path::PathBuf, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!(
            "sot-registry-write-prune-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        env.set("SOT_COMM_HOME", &dir);
        let registry_path = dir.join("registry.json");
        let seeded = serde_json::to_vec_pretty(&serde_json::json!({
            "agents": {
                "gone": {"host": "hostA", "workspace_id": "ws-1"},
                "kept": {"host": "hostA", "workspace_id": "ws-2"},
            }
        }))
        .unwrap();
        std::fs::write(&registry_path, &seeded).unwrap();
        (dir, registry_path, seeded)
    }

    #[test]
    fn a_prune_writes_pretty_json_and_a_newline_and_no_temp() {
        let _turn = crate::comm::registry::lock::TURN_TESTS.read().unwrap_or_else(|e| e.into_inner());
        let mut env = crate::comm::Env::empty();
        let (dir, registry_path, _) = seed(&mut env, "bytes");

        let removed = remove_comm_agents_for_workspace_in(&env, "", "ws-1", "hostA");
        assert_eq!(removed, vec!["gone".to_string()]);

        let mut expected = serde_json::to_vec_pretty(&serde_json::json!({
            "agents": {"kept": {"host": "hostA", "workspace_id": "ws-2"}}
        }))
        .unwrap();
        expected.push(b'\n');
        assert_eq!(std::fs::read(&registry_path).unwrap(), expected);
        assert!(!dir.join("registry.json.tmp").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_temp_write_prunes_nothing() {
        let _turn = crate::comm::registry::lock::TURN_TESTS.read().unwrap_or_else(|e| e.into_inner());
        let mut env = crate::comm::Env::empty();
        let (dir, registry_path, seeded) = seed(&mut env, "tmpfail");
        // On Unix a dangling symlink makes only the create fail (ENOENT): a
        // write that went on to rename would leave the link at the registry.
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("missing/x"), dir.join("registry.json.tmp")).unwrap();
        #[cfg(not(unix))]
        std::fs::create_dir(dir.join("registry.json.tmp")).unwrap();

        let removed = remove_comm_agents_for_workspace_in(&env, "", "ws-1", "hostA");
        assert!(removed.is_empty());
        assert_eq!(std::fs::read(&registry_path).unwrap(), seeded);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

mod edit_write_tests {
    use super::edit_entry_in;

    /// The edit a person's read makes: `done` removed, the state it showed made idle.
    fn clear(entry: &mut serde_json::Map<String, serde_json::Value>) -> ((), bool) {
        entry.remove("done");
        entry.insert("state".into(), "idle".into());
        ((), true)
    }

    // An entry `h`, seeded `done` on host-4.
    fn seed(env: &mut crate::comm::Env, tag: &str) -> (std::path::PathBuf, std::path::PathBuf, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!(
            "sot-registry-write-clear-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        env.set("SOT_COMM_HOME", &dir);
        env.set("SOT_SELF_HOST", "host-4");
        let registry_path = dir.join("registry.json");
        let seeded = serde_json::to_vec_pretty(&serde_json::json!({
            "agents": {"h": {"host": "host-4", "state": "done", "done": true, "summary": "s"}}
        }))
        .unwrap();
        std::fs::write(&registry_path, &seeded).unwrap();
        (dir, registry_path, seeded)
    }

    #[test]
    fn an_edit_writes_pretty_json_and_a_newline_and_no_temp() {
        let _turn = crate::comm::registry::lock::TURN_TESTS.read().unwrap_or_else(|e| e.into_inner());
        let mut env = crate::comm::Env::empty();
        let (dir, registry_path, _) = seed(&mut env, "bytes");

        assert_eq!(edit_entry_in(&env, "h", std::time::Duration::from_secs(1), clear), Ok(Some(())));

        let mut expected = serde_json::to_vec_pretty(&serde_json::json!({
            "agents": {"h": {"host": "host-4", "state": "idle", "summary": "s"}}
        }))
        .unwrap();
        expected.push(b'\n');
        assert_eq!(std::fs::read(&registry_path).unwrap(), expected);
        assert!(!dir.join("registry.json.tmp").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_temp_write_writes_nothing() {
        let _turn = crate::comm::registry::lock::TURN_TESTS.read().unwrap_or_else(|e| e.into_inner());
        let mut env = crate::comm::Env::empty();
        let (dir, registry_path, seeded) = seed(&mut env, "tmpfail");
        // On Unix a dangling symlink makes only the create fail (ENOENT): a
        // write that went on to rename would leave the link at the registry.
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("missing/x"), dir.join("registry.json.tmp")).unwrap();
        #[cfg(not(unix))]
        std::fs::create_dir(dir.join("registry.json.tmp")).unwrap();

        assert_eq!(edit_entry_in(&env, "h", std::time::Duration::from_secs(1), clear), Err(super::EditError::Unwritten));
        assert_eq!(std::fs::read(&registry_path).unwrap(), seeded);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

mod stamp_write_tests {
    use super::stamp_last_seen_in;
    use std::collections::BTreeSet;

    fn seed(env: &mut crate::comm::Env, tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "sot-registry-write-stamp-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        env.set("SOT_COMM_HOME", &dir);
        let registry_path = dir.join("registry.json");
        let seeded = serde_json::to_vec_pretty(&serde_json::json!({
            "agents": {
                "a": {"host": "h", "last_seen": "2026-01-01T00:00:00Z", "state": "idle", "summary": "s"},
                "b": {"host": "h", "state": "working"},
                "c": {"host": "h", "last_seen": "2026-01-01T00:00:00Z"},
            }
        }))
        .unwrap();
        std::fs::write(&registry_path, seeded).unwrap();
        (dir, registry_path)
    }

    fn set(handles: &[&str]) -> BTreeSet<String> {
        handles.iter().map(|h| h.to_string()).collect()
    }

    const STAMP: &str = "2026-10-05T00:00:00Z";

    #[test]
    fn it_stamps_listed_entries_only_and_creates_none() {
        let _turn = crate::comm::registry::lock::TURN_TESTS.read().unwrap_or_else(|e| e.into_inner());
        let mut env = crate::comm::Env::empty();
        let (dir, registry_path) = seed(&mut env, "listed");
        assert!(stamp_last_seen_in(&env, &set(&["a", "b", "nobody"]), STAMP));
        let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        assert_eq!(root["agents"]["a"], serde_json::json!({"host": "h", "last_seen": STAMP, "state": "idle", "summary": "s"}));
        assert_eq!(root["agents"]["b"], serde_json::json!({"host": "h", "state": "working", "last_seen": STAMP}));
        assert_eq!(root["agents"]["c"]["last_seen"], "2026-01-01T00:00:00Z");
        assert!(root["agents"].get("nobody").is_none(), "an entry was created");
        assert!(!dir.join("registry.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_nothing_to_change_it_writes_nothing() {
        let _turn = crate::comm::registry::lock::TURN_TESTS.read().unwrap_or_else(|e| e.into_inner());
        let mut env = crate::comm::Env::empty();
        let (dir, registry_path) = seed(&mut env, "nothing");
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&registry_path).unwrap());
        let before = std::fs::read(&registry_path).unwrap();
        assert!(stamp_last_seen_in(&env, &BTreeSet::new(), STAMP));
        assert!(stamp_last_seen_in(&env, &set(&["nobody"]), STAMP));
        assert_eq!(std::fs::read(&registry_path).unwrap(), before);
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&registry_path).unwrap()), inode, "the registry was rewritten");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_held_lock_returns_false_and_leaves_the_bytes_alone() {
        // Held for the whole bound, so no other test takes the lock meanwhile.
        let _turn = crate::comm::registry::lock::TURN_TESTS.write().unwrap_or_else(|e| e.into_inner());
        let mut env = crate::comm::Env::empty();
        let (dir, registry_path) = seed(&mut env, "locked");
        let before = std::fs::read(&registry_path).unwrap();
        // A directory at the lock's path can never be taken.
        std::fs::create_dir(dir.join(".registry.lock")).unwrap();
        assert!(!stamp_last_seen_in(&env, &set(&["a"]), STAMP));
        assert_eq!(std::fs::read(&registry_path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
