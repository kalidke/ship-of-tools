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

/// Rows' activity rule at the list: a row's `active_at` is the newest of its creation and its shown work-state stamp.
/// A stamp that is not well formed, and the stamp of another host's same-named entry, do not count; a row with no
/// session, which has no stamp, is placed by its creation.
#[tokio::test]
async fn the_list_says_each_rows_last_activity() {
    let _guard = guarded();
    let dir = std::env::temp_dir().join(format!(
        "sot-active-at-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SOT_COMM_HOME", &dir);
    std::env::set_var("SOT_SELF_HOST", "host-4");
    let entry = |host: &str, at: &str| json!({"host": host, "state": "working", "status_at": at});
    std::fs::write(
        dir.join("registry.json"),
        serde_json::to_vec_pretty(&json!({
            "agents": {
                "stamped-later": entry("host-4", "2026-01-02T00:00:00Z"),
                "stamped-earlier": entry("host-4", "2025-12-31T23:59:59Z"),
                "stamped-badly": entry("host-4", "2026-01-02 00:00:00"),
                "stamped-elsewhere": entry("hostB", "2026-01-02T00:00:00Z"),
            }
        }))
        .unwrap(),
    )
    .unwrap();

    // Every row was created at 2026-01-01T00:00:00Z, but for the one no time can be told for.
    let created = 1_767_225_600;
    let workspaces = Workspaces::new();
    for (label, agent_name, created) in [
        ("later", "stamped-later", created),
        ("earlier", "stamped-earlier", created),
        ("badly", "stamped-badly", created),
        ("elsewhere", "stamped-elsewhere", created),
        ("terminal", "", created),
        ("before-the-epoch", "", -5),
    ] {
        let mut ws = Workspace::from_label(
            label,
            std::path::PathBuf::from(format!("/p/{label}")),
            false,
            "none".into(),
            agent_name.into(),
            String::new(),
        );
        ws.created = created;
        workspaces.insert(ws);
    }

    let out = handle_workspace_list(1, json!({}), &workspaces, &no_pluto())
        .await
        .expect("handler must not error");
    let payload = out[0].0.payload.clone();
    let entries = payload.get("workspaces").unwrap().as_array().unwrap();
    let said = |slug: &str, field: &str| {
        entries
            .iter()
            .find(|e| e.get("slug").and_then(|v| v.as_str()) == Some(slug))
            .and_then(|e| e.get(field))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    for (slug, active_at, status_at) in [
        ("later", "2026-01-02T00:00:00Z", "2026-01-02T00:00:00Z"),
        ("earlier", "2026-01-01T00:00:00Z", "2025-12-31T23:59:59Z"),
        ("badly", "2026-01-01T00:00:00Z", "2026-01-02 00:00:00"),
        ("elsewhere", "2026-01-01T00:00:00Z", ""),
        ("terminal", "2026-01-01T00:00:00Z", ""),
        ("before-the-epoch", "1970-01-01T00:00:00Z", ""),
    ] {
        assert_eq!(said(slug, "active_at").as_deref(), Some(active_at), "{slug}: its last activity");
        // The work-state stamp keeps its own meaning beside it.
        assert_eq!(said(slug, "agent_status_at").as_deref(), Some(status_at), "{slug}: its work-state stamp");
    }

    let _ = std::fs::remove_dir_all(&dir);
}
