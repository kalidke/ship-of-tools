//! workspace.activate, .create, .list, .destroy, accounts.list, fe.presence, fe.sessions, pty.open, agent.send: the requests (send_<op>: write the frame, then record its PendingKind).

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
