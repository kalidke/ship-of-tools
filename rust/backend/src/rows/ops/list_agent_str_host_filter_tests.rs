// LU5d2: `handle_workspace_list`'s `agent_str` closure read
// `.agents[handle].<field>` by handle name alone. The handle it's
// called with here is bound via the stored `agent_name` fallback (no
// live tmux row for the session) — a caller-supplied name, not proof
// of ownership — so a same-named row stamped by ANOTHER host on the
// shared registry must read as empty, never leak its
// summary/status_at into this host's `workspace.list`.
use super::*;

struct EnvGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
    sot_comm_home: Option<std::ffi::OsString>,
    sot_self_host: Option<std::ffi::OsString>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, val) in [
            ("SOT_COMM_HOME", &self.sot_comm_home),
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
        sot_self_host: std::env::var_os("SOT_SELF_HOST"),
        _serial: serial,
    }
}

#[tokio::test]
async fn agent_str_never_reads_another_hosts_same_named_row() {
    let _guard = guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-agent-str-host-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SOT_COMM_HOME", &dir);
    std::env::set_var("SOT_SELF_HOST", "host-4");
    std::fs::write(
        dir.join("registry.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "agents": {
                "same-name": {
                    "host": "hostB",
                    "state": "working",
                    "summary": "leaked",
                    "status_at": "2026-01-01T00:00:00Z"
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let workspaces = Workspaces::new();
    let ws = Workspace::from_label(
        "myws",
        std::path::PathBuf::from("/p/myws"),
        false,
        "none".into(),
        "same-name".into(),
        String::new(),
    );
    workspaces.insert(ws);

    let out = handle_workspace_list(1, json!({}), &workspaces, &no_pluto())
        .await
        .expect("handler must not error");
    let payload = out[0].0.payload.clone();
    assert_eq!(payload.get("pluto_server_up"), Some(&json!(false)), "the list says whether the Pluto server is up: not here");
    let entries = payload.get("workspaces").unwrap().as_array().unwrap();
    let entry = entries
        .iter()
        .find(|e| e.get("slug").and_then(|v| v.as_str()) == Some("myws"))
        .expect("the workspace we inserted must be in the list");
    assert_eq!(
        entry.get("agent_summary").and_then(|v| v.as_str()),
        Some("")
    );
    assert_eq!(
        entry.get("agent_status_at").and_then(|v| v.as_str()),
        Some("")
    );

    let _ = std::fs::remove_dir_all(&dir);
}
