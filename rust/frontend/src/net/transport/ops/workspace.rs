//! workspace.activate, .create, .list, .destroy, accounts.list, fe.presence, fe.sessions, pty.open, agent.send: the requests (send_<op>: write the frame, then record its PendingKind).
//! Their replies (on_<op>: the reply frame becomes an IncomingEvt).

use super::*;

/// One row of the `workspace.list` response. Mirrors
/// `sot_protocol::WorkspaceListEntry` so the chrome can store it
/// without a protocol dependency on every consumer.
#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub project_root: String,
    pub session_name: String,
    pub kernel_running: bool,
    pub is_default: bool,
    /// Which agent this workspace auto-starts: "claude" | "codex" | "none".
    /// A host's DEFAULT row with `agent == "none"` is the inert anchor (ADR
    /// 0042 amendments 2026-09-04 / 2026-09-06), on every runtime —
    /// `session_host_children` filters it out of the Sessions tree entirely
    /// rather than rendering it as a session.
    pub agent: String,
    /// Contract (b): the FE launches claude (ccb) on first attach to a
    /// workspace with `autostart_claude == true`. `agent_name` is the comm
    /// handle the bootstrap joins as (informational on the FE side).
    pub autostart_claude: bool,
    pub agent_name: String,
    /// The sot-comm handle the session inside this workspace actually
    /// declared via `agent.join` — the **joined** handle, mirroring the
    /// wire's `WorkspaceListEntry.agent_handle`. Distinct from
    /// `agent_name` above, which is only what the workspace was CREATED
    /// to expect: the two can differ, and only this one is what a sender
    /// actually addresses. `WorkspaceInfo` doesn't derive
    /// Serialize/Deserialize (it's constructed by hand from the wire
    /// type at the one parse site below), so there's no `#[serde(default)]`
    /// to mirror here — empty string = never joined, same as the wire
    /// field's own empty-string-means-absent convention.
    pub agent_handle: String,
    /// Persisted spawn brief from the wire (mirrors the daemon's
    /// `WorkspaceListEntry.task`). The FE no longer delivers briefs (maintainer
    /// directive, 2026-06-16 — comm-spawn owns task delivery via a durable
    /// post-spawn comm message), so this is retained for protocol parity but
    /// intentionally unread on the FE side.
    #[allow(dead_code)]
    pub task: String,
    /// State-nav (ADR 0023 seam): the agent's work state read from the
    /// sot-comm registry by the daemon and copied onto the entry (the
    /// FE can't read the registry — separate machine/HOME). One of
    /// "working" | "idle" | "waiting" | "blocked" | "done"; "" when absent.
    pub agent_state: String,
    /// One-line glance of what the agent is doing / just did. "" when absent.
    pub agent_summary: String,
    /// ISO8601 (RFC3339) timestamp of the last state write — drives the
    /// staleness aging of a "working" that's gone quiet. "" when absent.
    pub agent_status_at: String,
    /// Lifecycle of the workspace's persistent REPL child: "not_started" |
    /// "starting" (spawned, precompiling — NOT dead) | "ready" | "dead";
    /// "" from a daemon that predates the field. Mirrors the `lifecycle`
    /// repl.frame evt for FEs that (re)connect mid-boot.
    pub repl_state: String,
    /// ADR 0042 slice L1a/L1b: `"tmux"` | `"capsule"` — which runtime
    /// hosts this workspace's agent pane. `""` from a daemon that
    /// predates L1a. ADR 0042 shrink round (rule A): no longer consulted
    /// by the attach path at all — `attach_session_to_bl` always sends
    /// `pty.open` and lets the daemon's own `attach_direct` reply decide
    /// (the only kind this build's daemon sends), so an old daemon (or
    /// one reporting `""`) changes nothing there either.
    ///
    /// Deserialized on every platform (the wire contract doesn't fork by
    /// FE OS). Historically read by gpu.rs only from `#[cfg(windows)]`
    /// call sites (L1b fix 5: capsule has nothing to attach to off
    /// Windows) — 2026-09-04 amendment adds one platform-agnostic
    /// reader, `session_host_children`'s inert-anchor filter, which must
    /// tell a Windows capsule default row (`agent == "none"` there means
    /// "never seeded an agent") apart from a shared backend's own TMUX
    /// default row (`agent == "none"` there is normal — the SoT LLM
    /// lives in the drawer). `""` from a daemon that predates L1a reads
    /// as neither and is simply never filtered.
    pub runtime: String,
    /// The supervisor-lane phase (ADR 0041 Lifecycle, snake_case),
    /// `"stopped"` (its state directory was never created — no supervisor
    /// has ever run for it), or `"unreachable"` (the lane could not be
    /// queried at all) — `Some` only for `runtime == "capsule"` rows.
    /// Folded into the Sessions row's glance line (`capsule_phase_tag`).
    pub phase: Option<String>,
    /// Per-session accounts (owner-simplified brief, 2026-09-15): the
    /// login directory this row's agent runs under. `""` = the agent's
    /// default directory — the common case, and the only value from a
    /// daemon that predates the field.
    pub account: String,
}

impl WorkspaceInfo {
    /// The daemon's default row in its inert-anchor state: home-rooted, no
    /// agent, not a session -- on EVERY host (owner ruling 2026-09-06: a row
    /// that looks like a session but cannot be closed, and invites an LLM
    /// pane it must not have, confuses; the SoT LLM lives in the drawer, and
    /// Ship of Tools development runs in its own workspace row like any
    /// other project). Shared by the Sessions tree (`session_host_children`)
    /// and the bottom strip's cache build (`fresh_workspace_caches`) so the
    /// two never drift on what "inert" means.
    pub(crate) fn is_inert_anchor(&self) -> bool {
        self.is_default && self.agent == "none"
    }
}

/// One row of the `accounts.list` response (owner-simplified brief,
/// 2026-09-15). Mirrors `sot_protocol::AccountEntry`. `"default"` sorts
/// first.
#[derive(Debug, Clone)]
pub struct AccountInfo {
    pub name: String,
    pub kinds: Vec<String>,
    pub logged_in: std::collections::HashMap<String, bool>,
}

impl AccountInfo {
    /// True if none of this account's declared kinds have a login here —
    /// the new-session prompt dims the row and appends "(not logged in)",
    /// still selectable: the daemon refuses with the exact fix command.
    pub fn any_logged_in(&self) -> bool {
        self.kinds.iter().any(|k| self.logged_in.get(k).copied().unwrap_or(false))
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceCreatedInfo {
    #[allow(dead_code)] // exposed by the protocol; frontend currently
    // keys off slug for active_workspace_id, but the
    // canonical id is what disk/IO consumers want
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub project_root: String,
    pub session_name: String,
}

/// `workspace.destroy` reply payload. `tmux_killed` and `toml_removed`
/// reflect what the daemon actually did — the chrome surfaces both in
/// the status line so the user can spot a half-success.
#[derive(Debug, Clone)]
pub struct WorkspaceDestroyedInfo {
    #[allow(dead_code)] // mirrors WorkspaceCreatedInfo; future routing
    // may need the canonical id even though slug is
    // what handlers key off today.
    pub workspace_id: String,
    pub slug: String,
    pub label: String,
    pub tmux_killed: bool,
    pub toml_removed: bool,
    /// `Some(detail)` when the backend kept the row instead of removing
    /// it (the default workspace's capsule run was ended in place — see
    /// `sot_protocol::ops::WorkspaceDestroyRes::kept`); `None` for an
    /// ordinary destroy where the row is actually gone.
    pub kept: Option<String>,
}

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
