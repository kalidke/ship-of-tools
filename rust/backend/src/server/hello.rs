//! The hello: the admission every connection passes first (`admit_hello`), the hello reply with its revision replay
//! and the roster entry (`register_hello`).

use super::*;
use crate::server::reply::HandlerOutput;
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

/// A hello refused: the `{error, code, ...}` payload its reply carries. The connection is closed after it.
#[derive(Debug)]
pub(super) struct HelloRefusal(pub(super) serde_json::Value);

impl HelloRefusal {
    fn new(code: &str, message: String) -> Self {
        Self(serde_json::json!({ "error": message, "code": code }))
    }
}

/// A frame as a connection's first frame: it must be a `hello` request that parses, else `unauthenticated`.
pub(super) fn parse_first_frame(frame: &Frame) -> Result<HelloReq, HelloRefusal> {
    if frame.kind != Kind::Req || frame.op != op::HELLO {
        return Err(HelloRefusal::new(
            "unauthenticated",
            format!("send a hello first: {:?} is not served to a connection that has not said hello", frame.op),
        ));
    }
    serde_json::from_value(frame.payload.clone())
        .map_err(|e| HelloRefusal::new("unauthenticated", format!("the hello payload does not parse: {e}")))
}

/// The one admission every connection passes after its process was admitted at accept: its hello, in this order. The
/// protocol gate (ADR 0030 §2: a structured `{error, code}` envelope that does NOT deserialize as `HelloRes`, so the
/// frontend surfaces a clear "update needed" screen), the declared host and OS account both present, and the host's
/// account record (ADR 0049 `## User isolation`: a host that has said hello as two accounts is refused until the
/// daemon restarts, and the refusal names the host and the remedy, never an account). A refused hello is one reply,
/// then the connection's end.
pub(super) fn admit_hello(req: &HelloReq, clients: &Clients) -> Result<(), HelloRefusal> {
    if protocol_gate(req.protocol) == ProtocolGate::Reject {
        let frontend_version = if req.app_version.is_empty() { "<pre-versioning>" } else { req.app_version.as_str() };
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
        return Err(HelloRefusal(serde_json::json!({
            "error": message,
            "code": "protocol_mismatch",
            "backend_protocol": sot_protocol::PROTOCOL_VERSION,
            "frontend_protocol": req.protocol,
            "backend_version": sot_protocol::app_version(),
            "frontend_version": req.app_version,
        })));
    }
    let present = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_string);
    let (Some(host), Some(os_user)) = (present(&req.host), present(&req.os_user)) else {
        return Err(HelloRefusal::new(
            "identity_missing",
            "a hello must name its host and the OS account it runs as (`host` and `os_user`)".to_string(),
        ));
    };
    clients.admit_account(&host, &os_user).map_err(|conflict| {
        tracing::warn!(host = %conflict.host, client_id = %req.client_id, "hello refused: the host has said hello as two OS accounts");
        HelloRefusal::new(
            "os_user_conflict",
            format!(
                "host {} has said hello to this daemon as more than one OS account; each OS account enrols its \
                 own hub account, then restart this daemon",
                conflict.host
            ),
        )
    })
}

/// The reply to an admitted hello: the session, its revision and any replay the client missed.
pub async fn handle_hello(
    req_id: u64,
    req: HelloReq,
    session: &Session,
    files_mode: &FilesMode,
    label: Option<&str>,
    clients: &crate::clients::Clients,
) -> Result<HandlerOutput> {
    let (session_id, revision) = session.snapshot().await;

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
    let host = Some(crate::rows::store::declared_host());
    let project_root = Some(files_mode.root_path().display().to_string());

    let res = HelloRes {
        session_id,
        revision,
        snapshot_pending,
        host,
        project_root,
        label: label.map(str::to_string),
        // Includes the connection this hello answers when it is a control session — it registers in
        // `register_hello` before this handler runs (ADR 0010/0013); a handoff connection never does.
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

/// Enters a control session in the client roster, holding its declared host, role and name for the connection's
/// life. Called after the connection's bus subscriptions and before the hello's reply, so `clients_connected` counts
/// it and no event between the two is lost. Only a long-lived role (`fe`, `bridge`) is eligible for the read
/// deadline, which the connection's first `ping` arms (`serve_control`); a handoff connection is never listed.
pub(super) fn register_hello(req: &HelloReq, clients: &Clients) -> crate::clients::ClientGuard {
    clients.register(
        req.client_id.clone(),
        req.app_version.clone(),
        req.protocol,
        req.role.clone(),
        req.host.clone(),
        req.instance.clone(),
        req.name.clone(),
    )
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
        // Concretely, protocol 3 is accepted today.
        assert_eq!(protocol_gate(3), ProtocolGate::Accept);
    }

    #[test]
    fn rejects_mismatched_protocol() {
        // The previous protocol (a frontend box that has not converged),
        // a pre-versioning peer (0: the v1 grace ended with the bump to
        // 2) and a newer one are all rejected — the FE renders the
        // "update needed" screen naming both sides.
        assert_eq!(protocol_gate(sot_protocol::PROTOCOL_VERSION - 1), ProtocolGate::Reject);
        // Every pre-0.6.6 client (frontend, hub link, comm scripts) speaks 2: it declares no OS account.
        assert_eq!(protocol_gate(2), ProtocolGate::Reject);
        assert_eq!(protocol_gate(0), ProtocolGate::Reject);
        assert_eq!(protocol_gate(99), ProtocolGate::Reject);
    }
}
