//! workspace.activate, .create, .list, .destroy, accounts.list, fe.presence, fe.sessions, pty.open, agent.send: the requests (send_<op>: write the frame, then record its PendingKind).
//! Their replies (on_<op>: the reply frame becomes an IncomingEvt).

use super::*;

pub(crate) async fn send_workspace_activate<W: AsyncWrite + Unpin>(
    mut tx: W,
    id: u64,
    workspace_id: Option<String>,
    read: bool,
) -> Result<()> {
    tracing::debug!(?workspace_id, read, id, "→ workspace.activate");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::WORKSPACE_ACTIVATE,
            serde_json::to_value(WorkspaceActivateReq {
                workspace_id,
                read,
            })?,
        ),
        None,
    )
    .await?;
    // No PendingKind: the FE doesn't act on the echoed
    // canonical id today; an unmatched response id is
    // silently ignored (same idiom as ToggleHidden above).
    Ok(())
}

pub(crate) async fn send_fe_presence<W: AsyncWrite + Unpin>(
    mut tx: W,
    id: u64,
) -> Result<()> {
    tracing::debug!(id, "→ fe.presence");
    codec::write_frame(
        &mut tx,
        &Frame::req(id, op::FE_PRESENCE, serde_json::to_value(FePresenceReq {})?),
        None,
    )
    .await?;
    // No PendingKind: fire-and-forget, same idiom as
    // ToggleHidden/WorkspaceActivate above.
    Ok(())
}

pub(crate) async fn send_fe_sessions<W: AsyncWrite + Unpin>(
    mut tx: W,
    id: u64,
    sessions: Vec<sot_protocol::DeclaredSession>,
) -> Result<()> {
    tracing::debug!(id, count = sessions.len(), "→ fe.sessions");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::FE_SESSIONS,
            serde_json::to_value(sot_protocol::FeSessionsReq { sessions })?,
        ),
        None,
    )
    .await?;
    // No PendingKind: fire-and-forget, same idiom as
    // FePresence above.
    Ok(())
}

pub(crate) async fn send_pty_open<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    cols: u16,
    rows: u16,
    target: Option<String>,
    user_switch: bool,
) -> Result<()> {
    tracing::debug!(cols, rows, ?target, user_switch, id, "→ pty.open");
    // L1b fix 1: cloned BEFORE the move into
    // `PtyOpenReq` below — the pending entry must
    // remember exactly what this request targeted so
    // the reply (in particular an `attach_direct`
    // refusal) is never applied to a different row.
    let pending_target = target.clone();
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::PTY_OPEN,
            serde_json::to_value(PtyOpenReq { cols, rows, target, user_switch })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::PtyOpen { target: pending_target });
    Ok(())
}

pub(crate) async fn send_workspace_create<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    label: String,
    project_root: String,
    autostart_claude: bool,
    agent: String,
    account: Option<String>,
) -> Result<()> {
    tracing::info!(%label, %project_root, autostart_claude, %agent, account = account.as_deref().unwrap_or(""), id, "→ workspace.create");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::WORKSPACE_CREATE,
            serde_json::to_value(sot_protocol::WorkspaceCreateReq {
                label,
                project_root,
                // Enter → ccb (comm-aware launcher, its first
                // turn is its own /sot-session-start); Shift+
                // Enter → false = bare session, no LLM agent.
                // agent/task stay empty either way — those are
                // spawned-agent fields, not used by interactive
                // creates.
                autostart_claude,
                agent,
                agent_name: String::new(),
                task: String::new(),
                // ADR 0043 decision 22: "" asks for this
                // host's own platform default — the FE
                // sends nothing new here, unchanged
                // behavior on every host.
                runtime: String::new(),
                account,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::WorkspaceCreate);
    Ok(())
}

pub(crate) async fn send_workspace_list<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
) -> Result<()> {
    tracing::debug!(id, "→ workspace.list");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::WORKSPACE_LIST,
            serde_json::to_value(WorkspaceListReq::default())?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::WorkspaceList);
    Ok(())
}

pub(crate) async fn send_accounts_list<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
) -> Result<()> {
    tracing::debug!(id, "→ accounts.list");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::ACCOUNTS_LIST,
            serde_json::to_value(sot_protocol::AccountsListReq::default())?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::AccountsList);
    Ok(())
}

pub(crate) async fn send_workspace_destroy<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    workspace_id: String,
) -> Result<()> {
    tracing::info!(%workspace_id, id, "→ workspace.destroy");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::WORKSPACE_DESTROY,
            serde_json::to_value(sot_protocol::WorkspaceDestroyReq {
                workspace_id,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::WorkspaceDestroy);
    Ok(())
}

pub(crate) async fn send_agent_send<W: AsyncWrite + Unpin>(
    mut tx: W,
    id: u64,
    from: String,
    to: String,
    text: String,
) -> Result<()> {
    tracing::debug!(%from, %to, id, "→ agent.send");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::AGENT_SEND,
            // No `id` (ADR 0048): this send is
            // fire-and-forget — nothing here reads a
            // receipt, so minting one would only ask a
            // filer to answer into a void.
            serde_json::to_value(AgentSendReq { from, to, text, id: None })?,
        ),
        None,
    )
    .await?;
    // Fire-and-forget: the ack carries `{ok, receivers}` and we
    // track neither, so no pending entry (an unmatched response id
    // is silently ignored).
    Ok(())
}

pub(crate) fn on_pty_open(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
    target: Option<String>,
) {
    // ADR 0042 slice L1b: a capsule row's `pty.open` is
    // refused with `{error, code: "attach_direct",
    // state_dir}` — this build's daemon has no tmux runtime,
    // so that refusal is the ONLY reply `pty.open` ever
    // gets; there is no size-confirmation success case left
    // to parse. `target` is THIS request's own target (fix
    // 1) — the chrome corrects/attaches that row, not
    // whatever is currently selected.
    if is_attach_direct(&frame.payload) {
        emit(IncomingEvt::PtyAttachDirect { target });
    } else {
        let error = pty_open_failure_reason(&frame.payload);
        tracing::warn!(?target, %error, payload = ?frame.payload, "pty.open res was not attach_direct");
        emit(IncomingEvt::PtyOpenFailed { target, error });
    }
}

pub(crate) fn on_workspace_create(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
) {
    // Backend returns either WorkspaceCreateRes on success
    // or `{error, code}` on failure (no_such_path etc).
    // Distinguish by presence of `workspace_id`.
    let payload = frame.payload;
    let result = if payload.get("workspace_id").is_some() {
        match serde_json::from_value::<sot_protocol::WorkspaceCreateRes>(payload) {
            Ok(r) => Ok(WorkspaceCreatedInfo {
                workspace_id: r.workspace_id,
                slug: r.slug,
                label: r.label,
                project_root: r.project_root,
                session_name: r.session_name,
            }),
            Err(e) => Err(format!("workspace.create res parse: {e}")),
        }
    } else {
        let msg = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        Err(msg)
    };
    emit(IncomingEvt::WorkspaceCreated { result });
}

pub(crate) fn on_workspace_list(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
) {
    match serde_json::from_value::<WorkspaceListRes>(frame.payload) {
        Ok(res) => {
            let workspaces: Vec<WorkspaceInfo> = res
                .workspaces
                .into_iter()
                .map(|w| WorkspaceInfo {
                    workspace_id: w.workspace_id,
                    slug: w.slug,
                    label: w.label,
                    project_root: w.project_root,
                    session_name: w.session_name,
                    kernel_running: w.kernel_running,
                    is_default: w.is_default,
                    agent: w.agent,
                    autostart_claude: w.autostart_claude,
                    agent_name: w.agent_name,
                    agent_handle: w.agent_handle,
                    task: w.task,
                    agent_state: w.agent_state,
                    agent_summary: w.agent_summary,
                    agent_status_at: w.agent_status_at,
                    repl_state: w.repl_state,
                    runtime: w.runtime,
                    phase: w.phase,
                    account: w.account,
                })
                .collect();
            emit(IncomingEvt::Workspaces { workspaces });
        }
        Err(e) => {
            tracing::warn!(error = %e, "workspace.list res parse failed");
        }
    }
}

// Per-session accounts (owner-simplified brief, 2026-09-15): a
// daemon that has no `accounts.list` handler (old build) or
// whose reply otherwise fails to parse as `AccountsListRes` is
// treated as an empty list — default-only, no error surfaced.
// The chrome hides the account choice entirely on an empty list.
pub(crate) fn on_accounts_list(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
) {
    let accounts = serde_json::from_value::<sot_protocol::AccountsListRes>(frame.payload)
        .map(|r| r.accounts)
        .unwrap_or_default()
        .into_iter()
        .map(|a| AccountInfo {
            name: a.name,
            kinds: a.kinds,
            logged_in: a.logged_in.into_iter().collect(),
        })
        .collect();
    emit(IncomingEvt::AccountsList { accounts });
}

pub(crate) fn on_workspace_destroy(
    frame: Frame,
    emit: &impl Fn(IncomingEvt),
) {
    // Same shape as WorkspaceCreate: success carries the
    // canonical fields (workspace_id etc.), failure carries
    // `{error, code}`. Distinguish by presence of
    // `workspace_id` since the protocol re-uses the op
    // response frame for both.
    let payload = frame.payload;
    let result = if payload.get("workspace_id").is_some() {
        match serde_json::from_value::<sot_protocol::WorkspaceDestroyRes>(payload) {
            Ok(r) => Ok(WorkspaceDestroyedInfo {
                workspace_id: r.workspace_id,
                slug: r.slug,
                label: r.label,
                tmux_killed: r.tmux_killed,
                toml_removed: r.toml_removed,
                kept: r.kept,
            }),
            Err(e) => Err(format!("workspace.destroy res parse: {e}")),
        }
    } else {
        let msg = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error")
            .to_string();
        Err(msg)
    };
    emit(IncomingEvt::WorkspaceDestroyed { result });
}

/// ADR 0042 slice L1b, revised by ADR 0045 decision 1: is `payload` a
/// `pty.open` refusal carrying `code: "attach_direct"` (the daemon's
/// answer for a capsule-runtime workspace, `rust/backend/src/server.rs`'s
/// `PTY_OPEN` arm)? The daemon still emits a `state_dir` alongside it
/// (until the next `PROTOCOL_VERSION` bump) but the frontend no longer
/// reads it — every capsule row is attached through its own daemon's
/// `lane.connect` bridge, keyed by `target` alone.
fn is_attach_direct(payload: &Value) -> bool {
    payload.get("code").and_then(|v| v.as_str()) == Some("attach_direct")
}

/// The reason text `PtyOpenFailed` carries for a `pty.open` reply that
/// isn't `attach_direct` — the reply's own `code` field, or a generic
/// fallback when the payload carries none (a malformed frame, or a
/// success shape this build no longer expects).
fn pty_open_failure_reason(payload: &Value) -> String {
    payload
        .get("code")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unsupported daemon reply".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ADR 0042 slice L1b: the attach_direct switch. ---

    #[test]
    fn is_attach_direct_declines_every_other_response_shape() {
        let attach = serde_json::json!({
            "error": "this workspace's agent pane is a capsule; attach directly instead of pty.open",
            "code": "attach_direct",
            "state_dir": "/state/workspaces/ws-1",
        });
        assert!(is_attach_direct(&attach));
        // Still recognized when the daemon couldn't resolve a state root —
        // the code alone gates this now, not the (ignored) path.
        let attach_no_dir = serde_json::json!({
            "error": "this workspace's agent pane is a capsule; attach directly instead of pty.open",
            "code": "attach_direct",
            "state_dir": serde_json::Value::Null,
        });
        assert!(is_attach_direct(&attach_no_dir));
        // Any other response shape — this build's daemon never sends
        // one for `pty.open`, but the check must still decline it.
        let ok = serde_json::json!({"cols": 80, "rows": 24});
        assert!(!is_attach_direct(&ok));
        // A DIFFERENT error code must not be mistaken for attach_direct —
        // only the exact literal switches the pane to the attach path.
        let other_error = serde_json::json!({"error": "boom", "code": "bad_target"});
        assert!(!is_attach_direct(&other_error));
    }

    #[test]
    fn pty_open_failure_reason_prefers_code_falls_back_when_absent() {
        let coded = serde_json::json!({"error": "boom", "code": "bad_target"});
        assert_eq!(pty_open_failure_reason(&coded), "bad_target");
        let uncoded = serde_json::json!({"cols": 80, "rows": 24});
        assert_eq!(pty_open_failure_reason(&uncoded), "unsupported daemon reply");
    }
}
