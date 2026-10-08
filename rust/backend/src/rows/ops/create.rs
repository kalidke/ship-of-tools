//! workspace.create and its two gates: one root per session, and no refresh of a row in use.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::Frame;
use crate::paths::valid_name;
use crate::server::reply::HandlerOutput;
use crate::session::Session;
use crate::rows::WorkspaceChanged;
use crate::rows::Workspaces;
use tokio::sync::broadcast;

/// Duplicate-root gate lookup (ADR 0036 Phase 1): the first registered
/// workspace whose `project_root` canonicalizes to `candidate_canon` while
/// carrying a slug OTHER than `incoming_slug` — excluding the inert default
/// anchor (`Workspaces::is_inert_default_anchor`, ADR 0042 amendment): it is
/// not a session and never runs an agent, so a real session at its root (a
/// local host's home dir) is not the two-agents-one-tree collision this gate
/// refuses. Same-slug matches are deliberately invisible here: a same-slug
/// create is decided by `same_slug_row_in_use`, which keeps the id-preserving
/// refresh only for a row not in use. A
/// registered root that no longer canonicalizes (deleted dir, dangling
/// symlink) is skipped, not fatal: judging that workspace is the Phase 2
/// reap's job, not the create path's.
fn find_other_workspace_with_root(
    candidate_canon: &std::path::Path,
    incoming_slug: &str,
    workspaces: &crate::rows::Workspaces,
) -> Option<std::sync::Arc<crate::rows::Workspace>> {
    workspaces.list().into_iter().find(|w| {
        w.slug != incoming_slug
            && !workspaces.is_inert_default_anchor(w)
            && w.project_root
                .canonicalize()
                .map(|c| c == candidate_canon)
                .unwrap_or(false)
    })
}

/// A same-slug `workspace.create` refreshes the existing row in place
/// (`Workspaces::insert` keeps its id, new metadata wins) and then starts it
/// in `StartMode::Start`, which is right only for a row no supervisor has
/// published to. Any observed phase means a supervisor holds the row (a second
/// leg exits contended, 70) and the refresh would rewrite the account, agent
/// and task of a run that keeps its old ones; a non-capsule row would get a
/// capsule started beside it. So the refresh is kept only for a capsule row
/// still in phase `stopped`; this returns any other same-slug row.
fn same_slug_row_in_use(
    incoming_slug: &str,
    workspaces: &Workspaces,
) -> Option<std::sync::Arc<crate::rows::Workspace>> {
    workspaces.list().into_iter().find(|w| {
        w.slug == incoming_slug
            && (w.runtime != "capsule" || w.phase() != crate::rows::workspace::Phase::Stopped)
    })
}

/// The root exists, is a directory, is no other row's, and its slug is not in use.
fn check_create_root(req_id: u64, req: &sot_protocol::WorkspaceCreateReq, project_root: &std::path::PathBuf,
    workspaces: &Workspaces) -> std::result::Result<(), HandlerOutput> {
    if !project_root.exists() {
        let payload = json!({
            "error": format!("project_root does not exist: {}", req.project_root),
            "code": "no_such_path",
        });
        return Err(vec![(
            Frame::res(req_id, op::WORKSPACE_CREATE, payload),
            None,
        )]);
    }
    if !project_root.is_dir() {
        let payload = json!({
            "error": format!("project_root is not a directory: {}", req.project_root),
            "code": "not_a_directory",
        });
        return Err(vec![(
            Frame::res(req_id, op::WORKSPACE_CREATE, payload),
            None,
        )]);
    }

    // Duplicate-root gate (ADR 0036 Phase 1): one project root, one workspace
    // identity. A second registration for an already-registered root would
    // persist a TOML the daemon then faithfully respawns on every boot, and
    // hands two agent sessions one shared working tree (the collision class
    // worktrees exist to prevent). Compared by canonical path on BOTH sides so
    // symlinked spellings of one directory still collide; refused only for a
    // DIFFERENT slug (a same-slug create is the in-use gate's question, just
    // below). The `existing` block lets the caller offer
    // "switch to that workspace" instead of dead-ending. Canonicalization
    // failure on the candidate skips the gate rather than failing the create —
    // prevention must not make creation less reliable than it is today.
    let incoming_slug = crate::paths::slug(&req.label);
    match project_root.canonicalize() {
        Ok(canon) => {
            if let Some(existing) =
                find_other_workspace_with_root(&canon, &incoming_slug, workspaces)
            {
                let payload = json!({
                    "error": format!(
                        "project_root is already registered as workspace '{}' (slug '{}')",
                        existing.label, existing.slug
                    ),
                    "code": "duplicate_root",
                    "existing": {
                        "workspace_id": existing.workspace_id,
                        "slug": existing.slug,
                        "label": existing.label,
                    },
                });
                return Err(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, project_root = %req.project_root,
                "duplicate-root gate skipped — candidate did not canonicalize");
        }
    }

    // In-use gate (see `same_slug_row_in_use`): refused before any state
    // changes. `workspace_id` stays nested under `existing` -- the frontend
    // reads a top-level `workspace_id` as success (rust/frontend/src/net/transport/ops/workspace.rs).
    if let Some(existing) = same_slug_row_in_use(&incoming_slug, workspaces) {
        let phase = existing.phase().as_wire_str();
        let payload = json!({
            "error": format!(
                "workspace '{}' (slug '{}') is in use ({} row, phase '{}'): attach to it, or destroy it before creating it again",
                existing.label, existing.slug, existing.runtime, phase
            ),
            "code": "label_in_use",
            "existing": {
                "workspace_id": existing.workspace_id,
                "slug": existing.slug,
                "label": existing.label,
                "phase": phase,
            },
        });
        return Err(vec![(
            Frame::res(req_id, op::WORKSPACE_CREATE, payload),
            None,
        )]);
    }
    Ok(())
}

/// The agent name, agent kind and autostart flag, and the runtime the row will use.
fn resolve_create_agent(req_id: u64, req: &sot_protocol::WorkspaceCreateReq)
    -> std::result::Result<(String, bool, String), HandlerOutput> {
    // Name validation (security review): `agent_name` is persisted. Empty is a
    // legitimate "no agent name" sentinel; anything non-empty must match the strict
    // allowlist or this is rejected outright rather than silently sanitized.
    if !req.agent_name.is_empty() && !valid_name(&req.agent_name) {
        let payload = json!({
            "error": format!(
                "invalid agent_name {:?} (want 1-64 chars of [A-Za-z0-9._-])",
                req.agent_name
            ),
            "code": "bad_agent_name",
        });
        return Err(vec![(
            Frame::res(req_id, op::WORKSPACE_CREATE, payload),
            None,
        )]);
    }

    // ADR 0031: resolve the agent kind. Explicit `agent` wins; absent derives
    // from the legacy `autostart_claude` flag.
    let agent_kind: String = if !req.agent.is_empty() {
        match req.agent.as_str() {
            "claude" | "codex" | "none" => req.agent.clone(),
            other => {
                let payload = json!({
                    "error": format!("unknown agent kind '{other}' (want claude | codex | none)"),
                    "code": "bad_agent",
                });
                return Err(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
    } else if req.autostart_claude {
        "claude".to_string()
    } else {
        "none".to_string()
    };
    let autostart = agent_kind != "none";

    // ADR 0042's rule, flipped here (L6 / this repo's B6 lane) now that
    // the bridge (ADR 0045) gives a capsule row a remote attach path:
    // `""` (absent on the wire) resolves to "capsule". `"capsule"` still
    // asks for one explicitly either way. ADR 0046 decision 5: nothing
    // NEW runs on tmux — an explicit `"tmux"` ask is refused (Windows
    // already refused it; this extends the SAME refusal everywhere).
    // Existing tmux rows keep running unaffected; the daemon just stops
    // CREATING new ones — they retire by attrition.
    let runtime: String = match req.runtime.as_str() {
        "" | "capsule" => "capsule".to_string(),
        other => {
            let payload = json!({
                "error": format!("unknown runtime {other:?} (want \"capsule\" or \"\")"),
                "code": "bad_runtime",
            });
            return Err(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    };
    Ok((agent_kind, autostart, runtime))
}

/// The launcher, the state root, the state root outside the project, and the account.
fn check_create_host(req_id: u64, req: &sot_protocol::WorkspaceCreateReq, agent_kind: &str, runtime: &str,
    project_root: &std::path::PathBuf)
    -> std::result::Result<(Vec<String>, Option<std::path::PathBuf>, String), HandlerOutput> {
    // ADR 0042 slice L1a, Codex review finding 9: validated BEFORE any
    // state mutation, whenever the resolved runtime is "capsule" (every
    // NEW workspace on Windows, or an explicitly requested one anywhere
    // the capsule runtime (`rows/run/`, `rows/spawn/`) compiles — ADR 0043 decision 22). `agent_argv` is
    // the same function the spawn itself uses, so this is the real
    // check, not a second guess at it — "codex" (no known launcher on
    // either platform) is refused here rather than silently launching a
    // bare shell nobody asked for. Codex's check is a plain file read (no
    // spawn), so this stays a direct call, no `spawn_blocking`.
    let capsule_argv: Vec<String> = if runtime == "capsule" {
        match crate::agents::argv::agent_argv(&agent_kind, Some(project_root.as_path())) {
            Ok(argv) => argv,
            Err(detail) => {
                let payload = json!({
                    "error": detail,
                    "code": "unsupported_agent_on_this_host",
                });
                return Err(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
    } else {
        Vec::new()
    };
    // ADR 0043 decision 23: refuse an unqualified state root at the SAME
    // "before any state mutation" moment `capsule_argv` above already
    // established — before `ws_seed`, before `workspaces.insert`, before
    // any toml. Ungated, like the capsule spawn further down (macOS
    // wiring lane): there is no host this daemon builds for that lacks a
    // capsule runtime, so there is no second, platform-shaped refusal for
    // this check to defer to.
    let capsule_state_root: Option<std::path::PathBuf> = if runtime == "capsule" {
        match crate::rows::spawn::state_root::qualified_state_root(None) {
            Ok(root) => Some(root),
            Err(detail) => {
                let payload = json!({
                    "error": detail,
                    "code": "state_root_unqualified",
                });
                return Err(vec![(
                    Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                    None,
                )]);
            }
        }
    } else {
        None
    };
    // A second refusal at the same before-any-mutation moment: a state
    // root resolving INSIDE this workspace's own project root would sit
    // under this daemon's project-root file watcher, whose open
    // directory handles block a Windows rename underneath them (field
    // defect: `sot-capsule supervise` exiting terminal 69 on
    // `MoveFileExW`). Same predicate `spawn_detached_supervisor` checks
    // again right before it spawns; this copy just gets a clean `code`
    // here instead of a rollback after a partial row insert.
    if let Some(root) = &capsule_state_root {
        if crate::rows::spawn::state_root::state_root_inside_project(root, &project_root) {
            let payload = json!({
                "error": format!(
                    "state root {root:?} lies inside the project root {project_root:?}: a \
                     capsule's state tree must never sit inside a directory this workspace watches"
                ),
                "code": "state_root_inside_project",
            });
            return Err(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    }
    // Accounts brief (v0.6.0): resolved once, HERE, and recorded on the
    // row — never re-derived later. `""`/absent is the default account,
    // a no-op. A non-default account is checked with the SAME pure
    // resolver the spawn path itself calls ([`crate::agents::accounts::account_env`]),
    // so a create-time refusal and a later spawn-time one (the folder
    // vanishing in between) can never disagree. Refuses loudly, before
    // any state mutation (the same moment `capsule_argv`/the state-root
    // checks above already established) — never a silent fallback to
    // the default folder: a bash (`agent == "none"`) row is refused the
    // same way a claude/codex row with a missing folder is, both
    // surfacing `account_env`'s own exact-command message.
    let account: String = req.account.clone().unwrap_or_default();
    if !account.is_empty() && account != "default" {
        let home = crate::agents::accounts::account_home();
        let check = home
            .ok_or_else(|| "no home directory to resolve an account against".to_string())
            .and_then(|home| crate::agents::accounts::account_env(&agent_kind, &account, &home));
        if let Err(detail) = check {
            let payload = json!({
                "error": detail,
                "code": "unknown_account",
            });
            return Err(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    }
    Ok((capsule_argv, capsule_state_root, account))
}

/// Starts the new row's capsule supervisor; a failure rolls the row back and refuses.
async fn start_created_capsule(req_id: u64, req: &sot_protocol::WorkspaceCreateReq, workspaces: &Workspaces,
    ws_handle: &std::sync::Arc<crate::rows::Workspace>, capsule_state_root: Option<std::path::PathBuf>,
    capsule_argv: &Vec<String>, project_root: &std::path::PathBuf) -> std::result::Result<(), HandlerOutput> {
    {
    // ADR 0042 slice L1a, Codex review finding 1: the capsule spawn —
    // and a SYNCHRONOUS failure here
    // FAILS the whole op: "a capsule workspace with no supervisor is
    // not a workspace." Rule C (shrink round): this daemon no longer
    // creates the state directory itself — `sot-capsule supervise`
    // creates its OWN, as its first act after it actually runs — so
    // a synchronous failure below leaves nothing on disk at all, not
    // even an empty directory. The DETACHED spawn-and-watch is what
    // survives this daemon's own exit, with its own exit handled
    // going forward (finding 6). On ANY failure to reach a running
    // supervisor, roll back the registry row and its persisted toml
    // and refuse the op with the real error text.
    // ADR 0043 decision 23: `capsule_state_root` was already resolved
    // and qualified ABOVE, before this row (or its toml) ever existed
    // — reuse it rather than re-resolving a second time. Always
    // `Some` here in practice (this arm only runs when
    // `ws_handle.runtime == "capsule"`, which is exactly when the
    // earlier check ran and would have already returned on failure);
    // the `None` arm stays as a defensive fallback, never actually hit.
    // ADR 0043 decision 29: a process spawn never runs on a Tokio
    // worker.
    //
    // ADR 0043 decision 33 (Codex review, 2026-09-11): this row's own
    // guard, taken HERE — and
    // held across the spawn attempt below, closing the exact race
    // `pty.open`'s own `ensure_started` could otherwise win against
    // this handler's still-in-flight spawn (the field latency map's
    // own ordering: `ensure_started` can reach this SAME
    // freshly-minted workspace_id within milliseconds of the row
    // becoming visible via `insert` above). Every other lifecycle
    // mutation of a capsule row takes the SAME guard (`ensure_started`,
    // `resume_if_absent`, the watchdog's own restart, `resume_all`) —
    // this is that discipline's create-time entry. `capsule_guard`
    // itself already refuses to mint a guard for an absent row; the
    // membership recheck right after (under the lock, not before it)
    // catches one that vanished WHILE this waited for it — deciding
    // under the guard rather than starting unconditionally, the same
    // discipline every other guarded mutation follows.
    let capsule_guard = workspaces.capsule_guard(&ws_handle.workspace_id);
    let _capsule_guard_held = match &capsule_guard {
        Some(g) => Some(g.lock().await),
        None => None,
    };
    let still_registered = capsule_guard.is_some()
        && workspaces.list().iter().any(|ws| ws.workspace_id == ws_handle.workspace_id);
    let spawn_result: std::result::Result<(), String> = if !still_registered {
        Err("workspace was removed before its capsule supervisor could be started".to_string())
    } else {
        match capsule_state_root {
        None => Err(format!(
            "could not resolve this machine's state root ({} unset)",
            crate::rows::spawn::state_root::STATE_ROOT_HINT
        )),
        // `&req.agent_name` verbatim (Codex round finding 2: no
        // synthesized default — a synthesized `<slug>-<host>` handed
        // to SOT_COMM_NAME would become an explicit pin that
        // overwrites any existing registry row of that name,
        // violating PROTOCOL.md's "never reuse a handle"; an empty
        // `agent_name` is a real, supported case now — comm-join.sh's
        // own #148 auto-disambiguating derivation picks the handle,
        // via the SOT_COMM_SELF_FILE this spawn pins).
        Some(state_root) => {
            let workspace_id = ws_handle.workspace_id.clone();
            let capsule_argv = capsule_argv.clone();
            let project_root = project_root.clone();
            let agent_name = req.agent_name.clone();
            let slug = ws_handle.slug.clone();
            let workspaces_for_spawn = workspaces.clone();
            // BLOCKING (process spawn, superseded by ADR 0045: no
            // pre-spawn probe runs here anymore): the row guard is
            // held by the CALLING async fn's own frame for this whole
            // `.await`, not by this closure — a panic in here is
            // caught by `spawn_blocking` itself and never unwinds
            // past that guard, so there is nothing to release on the
            // error path below beyond reporting it.
            tokio::task::spawn_blocking(move || {
                crate::rows::run::start::start_supervisor(
                    &state_root,
                    &workspace_id,
                    sot_log::supervisor::StartMode::Start,
                    &capsule_argv,
                    &project_root,
                    &agent_name,
                    &slug,
                    workspaces_for_spawn,
                )
            })
            .await
            .unwrap_or_else(|join_err| Err(format!("capsule spawn task panicked: {join_err}")))
            .map(|_phase| ())
        }
        }
    };
    match spawn_result {
        Ok(()) => {
            tracing::info!(workspace_id = %ws_handle.workspace_id, "workspace.create: capsule supervisor spawned");
            // Starts this row's lifecycle observer for its ongoing periodic poll.
            crate::rows::run::observer::ensure_running(&workspaces, &ws_handle);
        }
        Err(detail) => {
            tracing::warn!(workspace_id = %ws_handle.workspace_id, error = %detail, "workspace.create: capsule spawn failed; rolling back");
            let _ = workspaces.remove_by_id(&ws_handle.workspace_id);
            for toml_path in [
                crate::rows::store::toml_path_for(&ws_handle.slug),
                crate::rows::store::legacy_toml_path_for(&ws_handle.slug),
            ] {
                match std::fs::remove_file(&toml_path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => tracing::warn!(error = %e, path = ?toml_path, "workspace.create rollback: toml remove failed"),
                }
            }
            let payload = json!({
                "error": format!("capsule workspace could not be started: {detail}"),
                "code": "capsule_spawn_failed",
            });
            return Err(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    }
    }
    Ok(())
}

pub async fn handle_workspace_create(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    workspaces: &Workspaces,
    ws_events: &broadcast::Sender<WorkspaceChanged>,
) -> Result<HandlerOutput> {
    use sot_protocol::{WorkspaceCreateReq, WorkspaceCreateRes};
    // ADR 0023 §3 daemon-boot trigger — read off the raw payload (it is not a
    // `WorkspaceCreateReq` struct field: adding one would force the frozen FE's
    // struct literal to set it). serde ignores it on the typed deserialize below.
    let boot = payload_json
        .get("boot")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let req: WorkspaceCreateReq =
        serde_json::from_value(payload_json).context("workspace.create payload")?;
    tracing::info!(label = %req.label, project_root = %req.project_root, boot, "workspace.create");

    let project_root = std::path::PathBuf::from(&req.project_root);
    if let Err(out) = check_create_root(req_id, &req, &project_root, workspaces) {
        return Ok(out);
    }

    let (agent_kind, autostart, runtime) = match resolve_create_agent(req_id, &req) {
        Ok(agent) => agent,
        Err(out) => return Ok(out),
    };
    let (capsule_argv, capsule_state_root, account) =
        match check_create_host(req_id, &req, &agent_kind, &runtime, &project_root) {
            Ok(host) => host,
            Err(out) => return Ok(out),
        };
    let mut ws_seed = crate::rows::Workspace::from_label(
        &req.label,
        project_root.clone(),
        autostart,
        agent_kind.clone(),
        req.agent_name.clone(),
        req.task.clone(),
    );
    ws_seed.runtime = runtime;
    ws_seed.account = std::sync::Mutex::new(account);
    // The run gate, before the row exists: a refused create leaves nothing
    // to roll back. Held through the start below, so a shutdown that
    // closes the gate meanwhile waits for this create to finish.
    let start_permit = match workspaces.begin_start(&ws_seed.workspace_id) {
        Ok(permit) => permit,
        Err(refusal) => {
            let payload = json!({
                "error": format!("capsule workspace could not be started: {refusal}"),
                "code": "capsule_spawn_failed",
            });
            return Ok(vec![(
                Frame::res(req_id, op::WORKSPACE_CREATE, payload),
                None,
            )]);
        }
    };
    let ws_handle = workspaces.insert(ws_seed);
    if let Err(e) = crate::rows::store::save(&ws_handle) {
        tracing::warn!(error = %e, "workspace toml persist failed; workspace is in-memory only");
    }

    // ADR 0043 decision 22: branch on the resolved runtime VALUE, not a
    // platform cfg. Since ADR 0046 decision 5 that value is always
    // "capsule" for a NEW row, and since the macOS wiring lane the
    // capsule runtime carries no platform gate at all: one spawn path,
    // every host this daemon builds for. What differs per platform lives
    // in the capsule runtime's own leaf `cfg(unix)`/`cfg(windows)` arms,
    // so a host with neither fails to COMPILE rather than quietly
    // creating a row it can never supervise.
    if let Err(out) = start_created_capsule(req_id, &req, workspaces, &ws_handle, capsule_state_root,
        &capsule_argv, &project_root).await
    {
        return Ok(out);
    }
    drop(start_permit);

    let res = WorkspaceCreateRes {
        workspace_id: ws_handle.workspace_id.clone(),
        slug: ws_handle.slug.clone(),
        label: ws_handle.label.clone(),
        project_root: ws_handle.project_root.to_string_lossy().into_owned(),
        session_name: ws_handle.session_name.clone(),
    };
    let rev = session
        .bump(
            "workspace.created",
            json!({ "workspace_id": ws_handle.workspace_id, "slug": ws_handle.slug }),
        )
        .await;
    // Live-push to every connected frontend so the Sessions strip refreshes
    // without a manual workspace.list poll. Send error means no subscribers;
    // harmless.
    let _ = ws_events.send(WorkspaceChanged {
        action: "created".into(),
        slug: ws_handle.slug.clone(),
        workspace_id: ws_handle.workspace_id.clone(),
    });
    Ok(vec![(
        Frame::res(req_id, op::WORKSPACE_CREATE, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

#[cfg(test)]
#[path = "create_tests.rs"]
mod tests;
