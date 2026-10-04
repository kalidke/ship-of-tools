//! The hello handshake: the protocol gate, the hello reply with its revision replay and the roster entry.

use super::*;
use crate::handlers::HandlerOutput;
use sot_protocol::{HelloReq, HelloRes};

/// Outcome of the FE↔BE protocol handshake gate (ADR 0030 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtocolGate {
    /// Client protocol equals ours — proceed cleanly.
    Accept,
    /// Protocols differ — reject the hello with a structured mismatch error.
    Reject,
}

/// Gate the handshake on protocol integer equality (ADR 0030 §2). The
/// protocol-1 grace for a pre-versioning peer (`protocol == 0`) ended with
/// the bump to 2: a peer that cannot name its protocol is rejected like
/// any other mismatch, with both versions named.
fn protocol_gate(client_protocol: u32) -> ProtocolGate {
    if client_protocol == sot_protocol::PROTOCOL_VERSION {
        ProtocolGate::Accept
    } else {
        ProtocolGate::Reject
    }
}

pub async fn handle_hello(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    files_mode: &FilesMode,
    label: Option<&str>,
    clients: &crate::clients::Clients,
) -> Result<HandlerOutput> {
    let req: HelloReq = serde_json::from_value(payload_json).context("hello payload")?;
    let (session_id, revision) = session.snapshot().await;

    // Protocol version gate (ADR 0030 §2): a structured `{error, code}` envelope that does NOT deserialize
    // as `HelloRes`, so the frontend surfaces a clear "update needed" screen
    // instead of failing on a later op with a cryptic frame-parse error.
    match protocol_gate(req.protocol) {
        ProtocolGate::Accept => {}
        ProtocolGate::Reject => {
            let frontend_version = if req.app_version.is_empty() {
                "<pre-versioning>".to_string()
            } else {
                req.app_version.clone()
            };
            let message = format!(
                "protocol mismatch: backend {} (protocol {}) vs frontend {} (protocol {}) \
                 — update the older side",
                sot_protocol::app_version(),
                sot_protocol::PROTOCOL_VERSION,
                frontend_version,
                req.protocol,
            );
            tracing::warn!(
                client_id = %req.client_id,
                client_protocol = req.protocol,
                backend_protocol = sot_protocol::PROTOCOL_VERSION,
                "hello rejected: {message}"
            );
            let payload = serde_json::json!({
                "error": message,
                "code": "protocol_mismatch",
                "backend_protocol": sot_protocol::PROTOCOL_VERSION,
                "frontend_protocol": req.protocol,
                "backend_version": sot_protocol::app_version(),
                "frontend_version": req.app_version,
            });
            return Ok(vec![(
                Frame::res(req_id, op::HELLO, payload).with_rev(revision),
                None,
            )]);
        }
    }

    // Replay policy:
    //   - First-time client (no session_id): nothing to replay.
    //   - Session matches: replay every ring entry newer than last_seen_revision.
    //   - Session mismatches (e.g. backend restarted): snapshot_pending; client
    //     needs to refetch state from scratch.
    // If `last_seen_revision` is older than the ring's low watermark,
    // session.replay_after returns None, and we mark snapshot_pending too.
    let replay = match req.session_id.as_deref() {
        None => Some(Vec::new()),
        Some(sid) if sid == session_id => session.replay_after(req.last_seen_revision).await,
        Some(_) => None,
    };
    let snapshot_pending = replay.is_none();
    let replay_entries = replay.unwrap_or_default();

    tracing::info!(
        client_id = %req.client_id,
        client_session = ?req.session_id,
        client_rev = req.last_seen_revision,
        session_id = %session_id,
        revision,
        replay_count = replay_entries.len(),
        snapshot_pending,
        "hello"
    );

    // Surface backend identity to the chrome so users can tell where
    // they're connected — the one declared host (ADR 0046 decision 1),
    // resolved once at boot, never recomputed per hello. `root_path` is
    // the configured --project-root (absolute, canonicalised on startup).
    let host = Some(crate::workspaces::declared_host());
    let project_root = Some(files_mode.root_path().display().to_string());

    let res = HelloRes {
        session_id,
        revision,
        snapshot_pending,
        host,
        project_root,
        label: label.map(str::to_string),
        // Includes the connection this hello answers — it registers in
        // `handle_connection` before this handler runs (ADR 0010/0013).
        clients_connected: clients.count(),
        // ADR 0030 §2: report our wire-contract protocol + product version so
        // the frontend can warn on a legacy backend (protocol 0) and surface
        // both sides' versions if a later skew check needs them.
        protocol: sot_protocol::PROTOCOL_VERSION,
        app_version: sot_protocol::app_version(),
        // ADR 0035: this daemon accepts `proxy.connect` — the FE arms its
        // lazy loopback proxy listeners so backend pages reach a remote FE
        // through the one control tunnel, no per-port ssh forward.
        proxy: true,
    };

    let mut out: HandlerOutput = Vec::with_capacity(1 + replay_entries.len());
    out.push((
        Frame::res(req_id, op::HELLO, serde_json::to_value(res)?).with_rev(revision),
        None,
    ));
    for entry in replay_entries {
        out.push((
            Frame::evt(&entry.op, entry.payload).with_rev(entry.revision),
            None,
        ));
    }
    Ok(out)
}

/// Enters a connection in the client roster at its first hello and records its declared host and name.
pub(super) fn admit_hello(
    frame: &Frame, clients: &Clients, client_guard: &mut Option<crate::clients::ClientGuard>,
    is_long_lived_role: &mut bool, hello_host: &mut Option<String>, hello_name: &mut Option<String>,
) {
    // Register this connection in the client roster the first
    // time we learn its client_id (a reconnect re-sends hello
    // on the same connection — keep the original guard). Done
    // before `handle_hello` so `clients_connected` counts self.
    if client_guard.is_none() {
        if let Ok(req) =
            serde_json::from_value::<sot_protocol::HelloReq>(frame.payload.clone())
        {
            // Topology plan §F step 2: mark this connection
            // ELIGIBLE for the read-deadline reaper -- exactly
            // the two long-lived roles, `fe` and `bridge`
            // (`cli`/`agent` are one-shot and stay ungated).
            // This does NOT arm the deadline itself (manager
            // compatibility fix, post-review) — only this
            // connection's FIRST `ping` does that, so a peer too old to send one keeps
            // today's behaviour exactly, never reaped by this
            // path.
            // A peer on another protocol is about to be
            // refused by `handle_hello`'s gate: never enter
            // the roster (it would be counted as a directed
            // command's audience and listed by `version.query`
            // while its hello stands refused). It gets the
            // structured mismatch reply and nothing else.
            if req.protocol == sot_protocol::PROTOCOL_VERSION {
                *is_long_lived_role = matches!(req.role.as_str(), "fe" | "bridge");
                *hello_host = req.host.clone();
                *hello_name = req.name.clone();
                *client_guard = Some(clients.register(
                    req.client_id,
                    req.app_version,
                    req.protocol,
                    req.role,
                    req.host,
                    req.instance,
                    req.name,
                ));
            }
        }
    }
}

#[cfg(test)]
mod protocol_gate_tests {
    use super::{protocol_gate, ProtocolGate};

    #[test]
    fn accepts_matching_protocol() {
        // The backend's own PROTOCOL_VERSION always matches itself.
        assert_eq!(
            protocol_gate(sot_protocol::PROTOCOL_VERSION),
            ProtocolGate::Accept
        );
        // Concretely, protocol 2 is accepted today.
        assert_eq!(protocol_gate(2), ProtocolGate::Accept);
    }

    #[test]
    fn rejects_mismatched_protocol() {
        // The previous protocol (a frontend box that has not converged),
        // a pre-versioning peer (0: the v1 grace ended with the bump to
        // 2) and a newer one are all rejected — the FE renders the
        // "update needed" screen naming both sides.
        assert_eq!(protocol_gate(sot_protocol::PROTOCOL_VERSION - 1), ProtocolGate::Reject);
        assert_eq!(protocol_gate(0), ProtocolGate::Reject);
        assert_eq!(protocol_gate(99), ProtocolGate::Reject);
    }
}
