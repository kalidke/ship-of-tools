// End-to-end through the real async handler: `read: true` clears a
// `done` row via the SAME workspace binding `workspace.list` uses;
// `read: false` (an old frontend, or any programmatic switch) leaves
// the registry untouched. The ack echoes the canonical workspace_id
// regardless of what the clear did.
use super::*;

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

// A `runtime = "capsule"` row: no tmux pane, no stored `agent_name`
// (the owner's actual capsule sessions — the ones piling up blue).
// `agent_handle` seeds `Workspace.agent_handle` directly (ADR 0046
// decision 1's `agent.join` persistence target) — "" for a row that
// has never joined.
fn seed_capsule_workspace(label: &str, agent_handle: &str) -> (Workspaces, String) {
    let reg = Workspaces::new();
    let mut ws = Workspace::from_label(
        label,
        std::path::PathBuf::from("/p/x"),
        false,
        "none".into(),
        String::new(),
        String::new(),
    );
    ws.runtime = "capsule".to_string();
    ws.agent_handle = crate::rows::handle::Declared::new(agent_handle.to_string());
    let id = ws.workspace_id.clone();
    reg.insert(ws);
    (reg, id)
}

async fn activate(
    workspaces: &Workspaces,
    workspace_id: &str,
    read: bool,
) -> serde_json::Value {
    let payload = serde_json::json!({ "workspace_id": workspace_id, "read": read });
    let out = handle_workspace_activate(1, payload, workspaces)
        .await
        .expect("handler must not error");
    assert_eq!(
        out.len(),
        1,
        "workspace.activate always answers with exactly one frame"
    );
    out[0].0.payload.clone()
}

#[tokio::test]
async fn capsule_row_read_true_clears_via_declared_agent_handle() {
    // ADR 0046 decision 1: a capsule workspace's row is found ONLY
    // through its DECLARED `agent_handle` (`agent.join`'s persistence
    // target), never through a tmux match, a stored `agent_name`
    // (both empty/absent here), or a daemon-side self-file read-back
    // (deleted) — this is what the owner's actual local capsule
    // sessions look like, so this path clearing is the whole point of
    // the fix.
    let _guard = guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-workspace-activate-capsule-read-test-{}-{}",
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

    let (reg, id) = seed_capsule_workspace("activate-capsule-x", "host-4-activate-capsule-x");

    std::fs::write(
        &registry_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "agents": {
                "host-4-activate-capsule-x": {
                    "host": "host-4",
                    "state": "done",
                    "done": true,
                    "summary": "capsule probe summary",
                    "status_at": "2026-09-08T00:00:00Z",
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let ack = activate(&reg, &id, true).await;
    assert_eq!(
        ack.get("workspace_id").and_then(|v| v.as_str()),
        Some(id.as_str())
    );
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
    let row = &after["agents"]["host-4-activate-capsule-x"];
    assert_eq!(row["state"], "idle");
    assert!(row.get("done").is_none(), "the done fact must be removed");
    assert_eq!(row["summary"], "capsule probe summary");
    assert_eq!(row["status_at"], "2026-09-08T00:00:00Z");

    let _ = std::fs::remove_dir_all(&dir);
}

// ADR 0049: one row per handle. A row that lost its handle to a newer
// `agent.join` (declared handle empty) still has a self-file naming it;
// that fallback must bind nothing while another row declares the handle,
// in the list and in the read-clears-done alike.
#[tokio::test]
async fn a_row_whose_self_file_names_a_declared_handle_neither_shows_nor_clears_it() {
    let _guard = guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-workspace-activate-moved-handle-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("self")).unwrap();
    std::env::set_var("SOT_COMM_HOME", &dir);
    std::env::set_var("SOT_SELF_HOST", "host-4");
    let registry_path = dir.join("registry.json");

    let (reg, older_id) = seed_capsule_workspace("m5-older", "");
    let mut newer = Workspace::from_label(
        "m5-newer",
        std::path::PathBuf::from("/p/x"),
        false,
        "none".into(),
        String::new(),
        String::new(),
    );
    newer.runtime = "capsule".to_string();
    newer.agent_handle = crate::rows::handle::Declared::new("m5-shared".to_string());
    let newer_id = reg.insert(newer).workspace_id.clone();
    std::fs::write(
        dir.join("self").join(format!("host-4__{older_id}.txt")),
        "m5-shared\nrepo=m5\nroot=/p/x\n",
    )
    .unwrap();
    let write_registry = || {
        std::fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agents": {
                    "m5-shared": {
                        "host": "host-4",
                        "state": "done",
                        "done": true,
                        "summary": "the newer row's summary",
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
    };
    write_registry();

    let out = handle_workspace_list(1, serde_json::json!({}), &reg, &no_pluto())
        .await
        .expect("handler must not error");
    let payload = out[0].0.payload.clone();
    let entries = payload.get("workspaces").unwrap().as_array().unwrap();
    let field = |id: &str, f: &str| {
        entries
            .iter()
            .find(|e| e.get("workspace_id").and_then(|v| v.as_str()) == Some(id))
            .and_then(|e| e.get(f))
            .and_then(|v| v.as_str())
            .unwrap_or("<missing>")
            .to_string()
    };
    assert_eq!(field(&older_id, "agent_state"), "");
    assert_eq!(field(&older_id, "agent_summary"), "");
    assert_eq!(field(&newer_id, "agent_state"), "done");

    let still_done = || {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        v["agents"]["m5-shared"].get("done").is_some()
    };
    activate(&reg, &older_id, true).await;
    assert!(still_done(), "the older row's self-file must not clear a handle the newer row declares");
    activate(&reg, &newer_id, true).await;
    assert!(!still_done(), "the declaring row's own read clears it");

    let _ = std::fs::remove_dir_all(&dir);
}
