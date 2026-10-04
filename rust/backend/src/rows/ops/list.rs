//! workspace.list (the rows the window shows, with their comm state) and workspace.activate (viewing clears done).

use anyhow::Context;
use anyhow::Result;
#[cfg(test)]
use serde_json::json;
use sot_protocol::op;
use sot_protocol::Frame;
use crate::server::reply::HandlerOutput;
use crate::comm::registry::registry::{clear_comm_unread, comm_handle_for_workspace, host_matches, read_comm_agents};
use crate::rows::Workspaces;
#[cfg(test)]
use crate::rows::Workspace;

pub async fn handle_workspace_list(
    req_id: u64,
    _payload_json: serde_json::Value,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceListEntry, WorkspaceListRes};
    let default_id = workspaces.default_id();
    // Read the sot-comm registry once per list call (fresh — picks up the
    // owning agents' latest `comm-status.sh` writes). `None` when the file is
    // absent/malformed; every lookup below then falls back to empty strings.
    // On a blocking thread: the read's retry sleeps after a failed read.
    let comm_agents = tokio::task::spawn_blocking(read_comm_agents).await.ok().flatten();
    let host = crate::rows::store::declared_host();
    // Pull `.agents[agent_name].<field>` as an owned String, "" if anything is
    // missing or not a string. LU5d2: `agent_name` here is a handle the caller
    // (below) already bound to THIS workspace — by the
    // stored `agent_name` fallback — never proof it's this host's row, so
    // filter the entry through `host_matches` too: a same-named handle
    // stamped by another host on the shared registry must read as empty, not
    // leak its summary/status_at/state into this host's list.
    let agent_str = |agent_name: &str, field: &str| -> String {
        if agent_name.is_empty() {
            return String::new();
        }
        comm_agents
            .as_ref()
            .and_then(|a| a.get(agent_name))
            .filter(|entry| host_matches(entry, &host))
            .and_then(|entry| entry.get(field))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let ws_list = workspaces.list();
    // Pure memory: kept current by the row's lifecycle observer, no lane query.
    let mut entries: Vec<WorkspaceListEntry> = ws_list
        .into_iter()
        .map(|ws| {
            // Which registry row is this workspace's — one rule, shared with
            // `clear_comm_unread` (`comm_handle_for_workspace`).
            let handle = comm_handle_for_workspace(&ws);
            // The registry `state` IS the badge — no pane scrape, no merge, no
            // precedence to arbitrate. A prior version of this comment described
            // a registry-vs-pane precedence merge that no longer exists here; it
            // was the only thing in the tree suggesting a pane-based idle
            // detector could overrule a stamped fact, which it never can (only
            // an act that could BE the answer clears a question — ADR 0044
            // amendment, field report 2026-09-27).
            let agent_state = agent_str(&handle, "state");
            // `phase` is read straight off the row's own cell — every row
            // is a capsule on this build (`ws.runtime` is always
            // `"capsule"`), so there is no other case left to branch on.
            // A row with no state dir at all reads NEVER_STARTED_PHASE
            // ("stopped") exactly like a row that simply hasn't been
            // attached yet — deliberately NOT distinguished here (Fable
            // review, `tests/capsule_workspaces/`'s own "Rule H": a pre-seeded,
            // never-`workspace.create`d row is indistinguishable from a
            // truly orphaned one by any fact this list can cheaply check,
            // and a wire-visible claim otherwise would be dishonest for
            // exactly that row shape). `rows::run::resume::
            // log_orphaned_state_dirs` still names such rows once at
            // boot, as an operator diagnostic only; `workspace.destroy`'s
            // own real proof (a live lane connect) is what actually
            // decides whether one is removable.
            let state_dir = sot_log::host::state_dir::sot_state_dir().map(|root| {
                crate::rows::spawn::state_root::state_dir_for(&root, &ws.workspace_id)
                    .to_string_lossy()
                    .into_owned()
            });
            let phase = Some(ws.phase().as_wire_str().to_string());
            WorkspaceListEntry {
                workspace_id: ws.workspace_id.clone(),
                slug: ws.slug.clone(),
                label: ws.label.clone(),
                project_root: ws.project_root.to_string_lossy().into_owned(),
                session_name: ws.session_name.clone(),
                kernel_running: ws.kernel_built(),
                is_default: default_id.as_deref() == Some(ws.workspace_id.as_str()),
                autostart_claude: ws.autostart_claude,
                agent: ws.agent(),
                agent_name: if handle.is_empty() {
                    ws.agent_name()
                } else {
                    handle.clone()
                },
                agent_handle: ws.agent_handle(),
                task: ws.task.clone(),
                agent_state,
                agent_summary: agent_str(&handle, "summary"),
                agent_status_at: agent_str(&handle, "status_at"),
                repl_state: ws.repl_state().to_string(),
                runtime: ws.runtime.clone(),
                state_dir,
                phase,
                activation_error: ws.activation_error(),
                account: ws.account(),
            }
        })
        .collect();
    // Pin the default workspace (the daemon's home anchor) FIRST: the FE never
    // lists it, but its position is the strip's own active-index fallback.
    // Stable sort: every other workspace keeps its alphabetical-by-slug order.
    entries.sort_by(|a, b| b.is_default.cmp(&a.is_default));
    tracing::debug!(count = entries.len(), "workspace.list");
    let res = WorkspaceListRes {
        workspaces: entries,
    };
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_LIST, serde_json::to_value(res)?),
        None,
    )])
}

/// `workspace.activate` — builds the ack. The connection-local state this
/// updates (`active_workspace`, `server/conn.rs`'s `handle_connection`) is
/// mutated by the CALLER, not here — this function only resolves
/// `req.workspace_id` (again; the caller does the same resolve to learn
/// what to store, mirroring how the `HELLO` arm parses its payload
/// inline before calling `handle_hello`) and echoes back the canonical id,
/// or `None` when it didn't resolve. See `op::WORKSPACE_ACTIVATE` (ops/mod.rs)
/// for the full design.
pub async fn handle_workspace_activate(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceActivateReq, WorkspaceActivateRes};
    let req: WorkspaceActivateReq =
        serde_json::from_value(payload_json).context("workspace.activate payload")?;
    let resolved_ws = workspaces.resolve(req.workspace_id.as_deref());
    let resolved = resolved_ws.as_ref().map(|ws| ws.workspace_id.clone());
    // `read: true` = a PERSON switched the view here (Sessions-Enter,
    // Shift+Left/Right cycling) — clear this row's blue (ADR 0044). The ack
    // below is sent unconditionally, whatever this does or doesn't clear.
    if req.read {
        if let Some(ws) = resolved_ws.clone() {
            let host = crate::rows::store::declared_host();
            let _ = tokio::task::spawn_blocking(move || clear_comm_unread(&ws, &host)).await;
        }
    }
    tracing::info!(
        requested = req.workspace_id.as_deref().unwrap_or("<default>"),
        resolved = resolved.as_deref().unwrap_or("<unresolved>"),
        read = req.read,
        "workspace.activate"
    );
    let res = WorkspaceActivateRes {
        workspace_id: resolved,
    };
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_ACTIVATE, serde_json::to_value(res)?),
        None,
    )])
}

#[cfg(test)]
mod workspace_activate_read_tests {
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
        ws.agent_handle = std::sync::Mutex::new(agent_handle.to_string());
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
}

#[cfg(test)]
mod agent_str_host_filter_tests {
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

        let out = handle_workspace_list(1, json!({}), &workspaces)
            .await
            .expect("handler must not error");
        let payload = out[0].0.payload.clone();
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
}
