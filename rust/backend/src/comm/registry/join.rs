//! agent.join: a session declares its comm handle on its row, under the row's guard.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::AgentJoinReq;
use sot_protocol::AgentJoinRes;
use sot_protocol::Frame;
use crate::rows::WorkspaceChanged;
use crate::rows::Workspaces;
use tokio::sync::broadcast;
use crate::paths::valid_name;
use crate::server::reply::HandlerOutput;

/// `agent.join` (ADR 0046 decision 1): a session inside `req.workspace_id`
/// declares its sot-comm handle to this daemon — read by every later
/// `workspace.list`/`clear_comm_unread` call ahead of the self-file
/// read-back fallback (`capsule_comm_handle`, S5: stays until family H).
/// `handle` is validated against the same charset `workspace.create`'s own
/// `valid_name` already enforces (comm-lib.sh's derived handles are built
/// to satisfy exactly this). `set_agent_handle` (S6) is a guarded
/// in-place update of the existing row — never a replacement, never a
/// resurrection of a destroyed one. `ok: true` only after
/// `Workspace.agent_handle` is durably persisted (S7): a save failure is
/// reported as `persist_failed`, not swallowed as a warning, since a
/// declared handle with nothing on disk would not survive a daemon
/// restart. Publishes `workspace.changed` so the FE re-lists on success.
/// Refuses `bad_handle` for an invalid handle, `unknown_workspace` for a
/// workspace this daemon doesn't have (destroyed or never existed).
pub async fn handle_agent_join(
    req_id: u64,
    payload_json: serde_json::Value,
    workspaces: &Workspaces,
    ws_events_tx: &broadcast::Sender<WorkspaceChanged>,
) -> Result<HandlerOutput> {
    let req: AgentJoinReq = serde_json::from_value(payload_json).context("agent.join payload")?;
    if !valid_name(&req.handle) {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::AGENT_JOIN,
                json!({
                    "error": format!("invalid handle: {:?}", req.handle),
                    "code": "bad_handle",
                }),
            ),
            None,
        )]);
    }
    // Manager review round 2 (Codex finding B1): take the SAME per-row
    // lifecycle guard `destroy_capsule_workspace` takes (ADR 0043
    // decision 33) — held across the mutate+persist below, exactly the
    // way destroy holds it across its own toml-delete + registry-removal
    // sequence, so the two can never interleave. Without this, a
    // concurrent destroy could delete the toml and remove the registry
    // entry AFTER `set_agent_handle`'s own read but BEFORE this save,
    // and the save would recreate the toml for a workspace_id the
    // registry no longer has — a resurrection destroy's caller believes
    // it prevented. `capsule_guard` mints/returns `None` under its own
    // write lock if the row isn't currently registered, so a row already
    // gone by the time we ask for the guard refuses immediately, same as
    // before.
    let Some(guard) = workspaces.capsule_guard(&req.workspace_id) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::AGENT_JOIN,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    let _held = guard.lock().await;
    // Re-check under the guard: `capsule_guard`'s own registration check
    // ran before we actually acquired the lock above — a destroy that
    // was already mid-flight (holding this same guard) could have
    // finished removing the row in the interim. `set_agent_handle` does
    // its own fresh lookup, so this one call is both the re-check and
    // the mutation.
    let Some(ws) = workspaces.set_agent_handle(&req.workspace_id, &req.handle) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::AGENT_JOIN,
                json!({
                    "error": format!("unknown workspace: {:?}", req.workspace_id),
                    "code": "unknown_workspace",
                }),
            ),
            None,
        )]);
    };
    // Manager review (S7, Codex finding B9): `ok` depends on the save
    // actually succeeding — a persisted daemon restart with no toml
    // record would otherwise lose the only authoritative handle (the
    // self-file read-back this replaces is a fallback, not a second
    // source of truth to fall back on for a row that DID declare).
    // `set_agent_handle` has already applied the in-memory update by this
    // point (guarded in-place, S6) — a save failure is reported, not
    // rolled back, so the caller can retry the SAME join rather than
    // re-deriving a value that already matches memory. Still under
    // `_held`: the save that recreates the toml must finish before a
    // waiting destroy can start deleting it.
    if let Err(e) = crate::rows::store::save(&ws) {
        tracing::warn!(error = %e, workspace_id = %req.workspace_id, "agent.join: toml persist failed");
        return Ok(vec![(
            Frame::res(
                req_id,
                op::AGENT_JOIN,
                json!({
                    "error": format!("{e:#}"),
                    "code": "persist_failed",
                }),
            ),
            None,
        )]);
    }
    tracing::info!(workspace_id = %req.workspace_id, handle = %req.handle, "agent.join");
    let _ = ws_events_tx.send(WorkspaceChanged {
        action: "agent_joined".into(),
        slug: ws.slug.clone(),
        workspace_id: ws.workspace_id.clone(),
    });
    Ok(vec![(
        Frame::res(
            req_id,
            op::AGENT_JOIN,
            serde_json::to_value(AgentJoinRes { ok: true })?,
        ),
        None,
    )])
}

#[cfg(test)]
mod agent_join_tests {
    use super::*;
    use crate::comm::registry::registry::comm_handle_for_workspace;
    use crate::rows::Workspace;

    // `handle_agent_join` persists through `crate::rows::store::save` —
    // isolate every var `app_config_dir` reads (manager review, S18:
    // Windows persistence ignores XDG_CONFIG_HOME entirely and uses
    // LOCALAPPDATA/USERPROFILE instead — guarding only the Unix var let
    // this test module write into a real app-config root on Windows).
    // Mirrors `rows/store/`'s own round-trip test's exact setup.
    struct EnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        xdg_config_home: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
        localappdata: Option<std::ffi::OsString>,
        userprofile: Option<std::ffi::OsString>,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("XDG_CONFIG_HOME", &self.xdg_config_home),
                ("HOME", &self.home),
                ("LOCALAPPDATA", &self.localappdata),
                ("USERPROFILE", &self.userprofile),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
    fn env_guarded() -> (EnvGuard, std::path::PathBuf) {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "sot-agent-join-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let g = EnvGuard {
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            home: std::env::var_os("HOME"),
            localappdata: std::env::var_os("LOCALAPPDATA"),
            userprofile: std::env::var_os("USERPROFILE"),
            _serial: serial,
        };
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::set_var("LOCALAPPDATA", &dir);
        std::env::remove_var("USERPROFILE");
        (g, dir)
    }

    fn mk_ws(label: &str) -> Workspace {
        Workspace::from_label(
            label,
            std::path::PathBuf::from("/p"),
            false,
            "none".into(),
            String::new(),
            String::new(),
        )
    }

    #[tokio::test]
    async fn agent_join_persists_and_list_merges_by_the_declared_handle() {
        let (_g, dir) = env_guarded();
        let workspaces = Workspaces::new();
        let id = workspaces.insert(mk_ws("agentjoin")).workspace_id.clone();
        let (ws_events_tx, mut ws_events_rx) = tokio::sync::broadcast::channel(4);

        let payload = serde_json::json!({"workspace_id": id, "handle": "agentjoin-testhost"});
        let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx)
            .await
            .expect("handler must not error");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.payload.get("ok").and_then(|v| v.as_bool()), Some(true));

        // Persisted in the in-memory registry, readable back via resolve().
        let resolved = workspaces.resolve(Some(&id)).expect("workspace still registered");
        assert_eq!(resolved.agent_handle(), "agentjoin-testhost");

        // `workspace.list`'s own row-binding rule merges by the declared
        // handle — this is what makes the join visible there.
        let merged = comm_handle_for_workspace(&resolved, &workspaces.list());
        assert_eq!(merged, "agentjoin-testhost");

        // workspace.changed published so the FE re-lists.
        let evt = ws_events_rx.try_recv().expect("workspace.changed must be published");
        assert_eq!(evt.workspace_id, id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Manager review round 3 (Codex): the round-2 version of this test was
    // ineffective -- it removed the row from the registry (and the toml)
    // BEFORE spawning `handle_agent_join`, so the handler's very first
    // `capsule_guard` lookup returned `None` immediately and the join
    // never actually contended for the held mutex at all; the test passed
    // even with the round-2 B1 fix fully reverted. Rebuilt here to match
    // the real race: the row stays registered and the toml stays on disk
    // while we hold the guard and spawn the REAL handler, we prove that
    // spawned join is genuinely PENDING (a bounded timeout that must
    // elapse -- deterministic, not a sleep-and-hope, because a task
    // blocked on a held `tokio::sync::Mutex` cannot complete regardless of
    // how long we wait), and only THEN do we perform destroy's own two
    // actions (toml removal, `remove_by_id`) while STILL holding the
    // guard, exactly the way `handle_workspace_destroy` holds
    // `destroy_guard` across that same sequence for both its capsule arm
    // and (round 3 finding B) its tmux arm. Releasing the guard only after
    // that is what proves the join, once unblocked, refuses cleanly
    // instead of resurrecting what destroy just removed. Run for both a
    // capsule row and a tmux row: `capsule_guard` mints/returns a guard
    // for any registered row regardless of runtime (see its own doc
    // comment), and round 3's fix made the tmux destroy arm take it too,
    // so the same interleaving must be closed for both.
    async fn agent_join_blocks_on_held_guard_then_destroy_wins(runtime: &str, slug: &str) {
        let (_g, dir) = env_guarded();
        let workspaces = Workspaces::new();
        let mut ws = mk_ws(slug);
        ws.runtime = runtime.to_string();
        let id = workspaces.insert(ws).workspace_id.clone();
        crate::rows::store::save(&workspaces.resolve(Some(&id)).unwrap()).expect("seed save");
        let toml_path = crate::rows::store::toml_path_for(slug);
        assert!(toml_path.exists(), "test setup: the seed toml must exist");

        // 1 + 2: the row stays registered; take and hold its guard exactly
        // the way `handle_workspace_destroy` does (both arms, since round
        // 3) across its own toml-delete + remove_by_id sequence.
        let guard = workspaces.capsule_guard(&id).expect("row is registered");
        let held = guard.lock().await;

        // 3: spawn the REAL handler while the row is STILL registered and
        // the guard is STILL held -- it must block trying to acquire the
        // same guard.
        let workspaces_for_join = workspaces.clone();
        let (ws_events_tx, mut ws_events_rx) = tokio::sync::broadcast::channel(4);
        let id_for_join = id.clone();
        let handle_for_join = format!("{slug}-testhost");
        let mut join_task = tokio::spawn(async move {
            let payload = serde_json::json!({"workspace_id": id_for_join, "handle": handle_for_join});
            handle_agent_join(1, payload, &workspaces_for_join, &ws_events_tx).await
        });

        // Prove it is genuinely PENDING: this bounded wait must elapse,
        // not race-and-hope -- a task truly blocked on the held guard
        // cannot complete no matter how long we wait, so a timeout here
        // is a deterministic proof, not a flaky one.
        let still_pending =
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut join_task).await;
        assert!(
            still_pending.is_err(),
            "join must still be blocked on the held guard, not completed"
        );

        // 4: perform destroy's own two actions while STILL holding the
        // guard -- the exact interleaving the guard exists to serialize
        // against.
        std::fs::remove_file(&toml_path).expect("remove seed toml");
        workspaces.remove_by_id(&id);
        assert!(!toml_path.exists(), "test setup: toml must be gone before the guard is released");
        assert!(workspaces.resolve(Some(&id)).is_none(), "test setup: the row must be gone from the registry");

        // 5: release the guard -- only now can the join proceed.
        drop(held);

        let out = join_task.await.expect("join task must not panic").expect("handler must not error");
        assert_eq!(
            out[0].0.payload.get("code").and_then(|v| v.as_str()),
            Some("unknown_workspace"),
            "a join that loses the race to a destroy must refuse, never resurrect: {:?}",
            out[0].0.payload
        );
        assert!(
            !toml_path.exists(),
            "the destroyed row's toml must never be recreated by a losing join"
        );
        assert!(
            workspaces.resolve(Some(&id)).is_none(),
            "the destroyed row must stay gone from the registry"
        );
        assert!(ws_events_rx.try_recv().is_err(), "no workspace.changed for a refused join");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn agent_join_interleaved_with_destroy_never_recreates_the_toml_capsule() {
        agent_join_blocks_on_held_guard_then_destroy_wins("capsule", "agentjoin-race-capsule").await;
    }

    #[tokio::test]
    async fn agent_join_refuses_unknown_workspace() {
        let (_g, dir) = env_guarded();
        let workspaces = Workspaces::new();
        let (ws_events_tx, _rx) = tokio::sync::broadcast::channel(4);

        let payload = serde_json::json!({"workspace_id": "ws-does-not-exist", "handle": "somehandle"});
        let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx)
            .await
            .expect("handler must not error");
        assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("unknown_workspace"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn agent_join_refuses_bad_handle_and_persists_nothing() {
        let (_g, dir) = env_guarded();
        let workspaces = Workspaces::new();
        let id = workspaces.insert(mk_ws("badhandle")).workspace_id.clone();
        let (ws_events_tx, _rx) = tokio::sync::broadcast::channel(4);

        let payload = serde_json::json!({"workspace_id": id, "handle": "not a valid handle!"});
        let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx)
            .await
            .expect("handler must not error");
        assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("bad_handle"));

        let resolved = workspaces.resolve(Some(&id)).expect("workspace still registered");
        assert_eq!(resolved.agent_handle(), "", "a refused join must persist nothing");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn agent_join_reports_persist_failed_and_never_returns_ok_on_a_save_failure() {
        // Manager review (S7, Codex finding B9): ok:true must depend on
        // the save actually succeeding. Occupy the "sot" config
        // subdirectory with a plain FILE instead of a directory --
        // `crate::rows::store::save`'s `create_dir_all` necessarily fails
        // under it, regardless of the host-derived `workspaces-<host>`
        // segment.
        let (_g, dir) = env_guarded();
        std::fs::write(dir.join("sot"), b"not a directory").unwrap();

        let workspaces = Workspaces::new();
        let id = workspaces.insert(mk_ws("agentjoin-persist-fail")).workspace_id.clone();
        let (ws_events_tx, mut ws_events_rx) = tokio::sync::broadcast::channel(4);

        let payload =
            serde_json::json!({"workspace_id": id, "handle": "agentjoin-persist-fail-testhost"});
        let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx)
            .await
            .expect("a save failure is a wire-level refusal, not a Rust error");
        assert_eq!(out.len(), 1);
        assert_ne!(
            out[0].0.payload.get("ok").and_then(|v| v.as_bool()),
            Some(true),
            "ok:true must never be returned when persistence failed"
        );
        assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("persist_failed"));

        // The in-memory update still landed (S6's guarded in-place update
        // runs before the save attempt) -- only the DURABILITY claim is
        // refused.
        let resolved = workspaces.resolve(Some(&id)).expect("workspace still registered");
        assert_eq!(resolved.agent_handle(), "agentjoin-persist-fail-testhost");

        // No workspace.changed on a failed persist -- the FE would
        // re-list to a handle that isn't actually durable yet.
        assert!(
            ws_events_rx.try_recv().is_err(),
            "must not publish workspace.changed on a failed persist"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn agent_join_after_the_workspace_is_destroyed_reports_unknown_never_resurrects() {
        // Manager review (S6, Codex finding B5): the destroy race at the
        // handler level -- a join against a workspace_id the registry no
        // longer has must refuse cleanly, never bring the row back.
        let (_g, dir) = env_guarded();
        let workspaces = Workspaces::new();
        let id = workspaces.insert(mk_ws("agentjoin-destroyed")).workspace_id.clone();
        workspaces.remove_by_id(&id);
        assert!(workspaces.resolve(Some(&id)).is_none(), "test setup: the row must actually be gone");

        let (ws_events_tx, mut ws_events_rx) = tokio::sync::broadcast::channel(4);
        let payload = serde_json::json!({"workspace_id": id, "handle": "agentjoin-destroyed-testhost"});
        let out = handle_agent_join(1, payload, &workspaces, &ws_events_tx)
            .await
            .expect("handler must not error");
        assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("unknown_workspace"));
        assert!(workspaces.resolve(Some(&id)).is_none(), "the destroyed row must stay gone -- no resurrection");
        assert!(ws_events_rx.try_recv().is_err(), "no workspace.changed for a refused join");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
