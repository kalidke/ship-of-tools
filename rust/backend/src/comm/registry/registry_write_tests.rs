//! Tests of the write that ends `registry.rs`'s two locked closures: the bytes it leaves and what a failed temp write
//! changes.

use super::*;

mod prune_write_tests {
    use super::remove_comm_agents_for_workspace;

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

    fn seed(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!(
            "sot-registry-write-prune-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
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
        let _guard = guarded();
        let (dir, registry_path, _) = seed("bytes");

        let removed = remove_comm_agents_for_workspace("", "ws-1", "hostA");
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
        let _guard = guarded();
        let (dir, registry_path, seeded) = seed("tmpfail");
        // On Unix a dangling symlink makes only the create fail (ENOENT): a
        // write that went on to rename would leave the link at the registry.
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("missing/x"), dir.join("registry.json.tmp")).unwrap();
        #[cfg(not(unix))]
        std::fs::create_dir(dir.join("registry.json.tmp")).unwrap();

        let removed = remove_comm_agents_for_workspace("", "ws-1", "hostA");
        assert!(removed.is_empty());
        assert_eq!(std::fs::read(&registry_path).unwrap(), seeded);

        let _ = std::fs::remove_file(dir.join("registry.json.tmp"));
        let _ = std::fs::remove_dir(dir.join("registry.json.tmp"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

mod clear_write_tests {
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

    // A capsule row bound by its declared handle, seeded `done` on host-4.
    fn seed(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, Vec<u8>, Workspace) {
        let dir = std::env::temp_dir().join(format!(
            "sot-registry-write-clear-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SOT_COMM_HOME", &dir);
        std::env::set_var("SOT_SELF_HOST", "host-4");
        let registry_path = dir.join("registry.json");
        let seeded = serde_json::to_vec_pretty(&serde_json::json!({
            "agents": {"h": {"host": "host-4", "state": "done", "done": true, "summary": "s"}}
        }))
        .unwrap();
        std::fs::write(&registry_path, &seeded).unwrap();
        let mut ws = Workspace::from_label(
            "writeprobe",
            std::path::PathBuf::from("/p"),
            false,
            "none".into(),
            String::new(),
            String::new(),
        );
        ws.runtime = "capsule".to_string();
        ws.agent_handle = std::sync::Mutex::new("h".to_string());
        (dir, registry_path, seeded, ws)
    }

    #[test]
    fn a_clear_writes_pretty_json_and_a_newline_and_no_temp() {
        let _guard = guarded();
        let (dir, registry_path, _, ws) = seed("bytes");

        clear_comm_unread(&ws, "host-4");

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
    fn a_failed_temp_write_clears_nothing() {
        let _guard = guarded();
        let (dir, registry_path, seeded, ws) = seed("tmpfail");
        // On Unix a dangling symlink makes only the create fail (ENOENT): a
        // write that went on to rename would leave the link at the registry.
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("missing/x"), dir.join("registry.json.tmp")).unwrap();
        #[cfg(not(unix))]
        std::fs::create_dir(dir.join("registry.json.tmp")).unwrap();

        clear_comm_unread(&ws, "host-4");
        assert_eq!(std::fs::read(&registry_path).unwrap(), seeded);

        let _ = std::fs::remove_file(dir.join("registry.json.tmp"));
        let _ = std::fs::remove_dir(dir.join("registry.json.tmp"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
