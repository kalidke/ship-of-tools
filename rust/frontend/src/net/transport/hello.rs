//! The hello: its reply bound, its refusal and the protocol-mismatch message.

use super::*;

/// How long the hello reply may take. An ssh child stalled before auth
/// leaves the pipe open and silent, so without a bound the one control
/// transport waits forever; on expiry the read errors like an EOF and the
/// reconnect loop backs off normally, dropping the child.
pub(super) const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Read the hello reply frame, bounded by `timeout` (a parameter so a test
/// can pass a short one).
pub(super) async fn read_hello_reply<R: tokio::io::AsyncBufRead + Unpin>(
    rx: &mut R,
    timeout: std::time::Duration,
) -> Result<(Frame, Option<Vec<u8>>)> {
    tokio::time::timeout(timeout, codec::read_frame(rx))
        .await
        .map_err(|_| anyhow::anyhow!("hello reply timed out after {timeout:?}"))?
}

/// The daemon answered the hello with a refusal (protocol skew, no account named, a second account):
/// a reply, so the link itself is fine and the gate stays up.
#[derive(Debug)]
pub(super) struct HelloRefused(pub(super) String);

impl std::fmt::Display for HelloRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HelloRefused {}

/// The blocking "update needed" body for a `protocol_mismatch` hello
/// refusal (ADR 0030 §2), naming BOTH sides from the daemon's structured
/// payload and this build's own constants — so an old daemon and a new
/// frontend fail as loudly as the reverse skew, which the daemon's own
/// hello gate names (`sot-backend`'s `handle_hello`).
pub(crate) fn protocol_mismatch_message(payload: &serde_json::Value, err_msg: &str) -> String {
    let get_str = |k: &str| payload.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let get_u32 = |k: &str| payload.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let backend_version = {
        let v = get_str("backend_version");
        if v.is_empty() {
            "<unknown>".to_string()
        } else {
            v.to_string()
        }
    };
    let theirs = get_u32("backend_protocol");
    let ours = u64::from(sot_protocol::PROTOCOL_VERSION);
    let behind = if theirs > ours { "frontend" } else { "daemon" };
    format!(
        "{behind} out of date — daemon {backend_version} speaks protocol {theirs}, this frontend {} speaks {ours}\n\n\
         backend:  {}  (protocol {})\n\
         frontend: {}  (protocol {})\n\n\
         dev: git pull + rebuild + relaunch · see docs/adr/0030\n\n\
         ({err_msg})",
        sot_protocol::app_version(),
        backend_version,
        theirs,
        sot_protocol::app_version(),
        sot_protocol::PROTOCOL_VERSION,
    )
}

/// Write the hello request: this window's identity and its reconnect memory.
pub(super) async fn send_hello<W: AsyncWrite + Unpin>(
    mut tx: W,
    hello_id: u64,
    session: &SessionState,
    token: Option<&str>,
) -> Result<()> {
    // ADR 0030 §2: `this_process` advertises our wire-contract protocol + product version so the backend can
    // gate on protocol equality and name both sides in a mismatch error, and the OS account this window runs as
    // (ADR 0049 `## User isolation`); an unreadable account sends no hello.
    //
    // This FE's own declared identity (ADR 0046 decision 1): `name` is its address, `fe@<host>` — the value a
    // `--fe <host>` target matches against, so the daemon can name "the frontend a person is at"
    // (`fe.presence`) without a second derivation.
    let identity = crate::net::identity::frontend_identity();
    let hello = HelloReq {
        session_id: session.memory.session_id.clone(),
        last_seen_revision: session.memory.last_seen_revision,
        token: token.map(|s| s.to_string()),
        instance: Some(identity.instance.clone()),
        name: Some(identity.name.clone()),
        ..HelloReq::this_process(
            session.memory.client_id.clone(),
            crate::net::identity::FrontendIdentity::ROLE,
            Some(identity.host.clone()),
        )?
    };
    codec::write_frame(
        &mut tx,
        &Frame::req(hello_id, op::HELLO, serde_json::to_value(&hello)?),
        None,
    )
    .await?;
    Ok(())
}

/// Read the hello reply and refuse an error envelope; any reply raises the link gate.
pub(super) async fn read_hello<R: tokio::io::AsyncBufRead + Unpin, Wn: Redraw>(
    mut rx: R,
    hello_id: u64,
    gate: Option<&sot_protocol::topology::ssh_bridge::LinkGate>,
    session: &mut SessionState,
    emit: &impl Fn(IncomingEvt),
    window: &Wn,
) -> Result<Frame> {
    let (frame, _) = read_hello_reply(&mut rx, HELLO_TIMEOUT).await?;
    if frame.id != hello_id {
        anyhow::bail!("hello reply id mismatch: got {}, want {hello_id}", frame.id);
    }
    // Any reply, a refusal included, proves the link.
    if let Some(gate) = gate {
        gate.set_up(true);
    }
    if let Some(r) = frame.rev {
        session.memory.last_seen_revision = session.memory.last_seen_revision.max(r);
    }
    // Inspect the frame for an error envelope first — the backend rejects
    // a hello it does not admit with `{error, code}` (ADR 0049 `## User
    // isolation`: another protocol, no host or OS account named, a second
    // account on the host), which does not deserialize as HelloRes. Every
    // refusal is the person's to read: push it as a HelloRefused evt so the
    // chrome shows a persistent blocking screen, the daemon's own message
    // for an account refusal and, for a version skew (ADR 0030 §2, not a
    // transient drop), a readable multi-line body built from the backend's
    // structured fields (falling back to its already-formatted `error`
    // string) with the dev fix hint. We still bail afterward so the
    // reconnect loop keeps the socket warm — a re-hello re-affirms the same
    // overlay, idempotently, until the cause is resolved.
    if let Some(err_msg) = frame.payload.get("error").and_then(|v| v.as_str()) {
        let code = frame
            .payload
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        tracing::error!(code, "hello rejected: {err_msg}");
        let message = if code == "protocol_mismatch" {
            protocol_mismatch_message(&frame.payload, err_msg)
        } else {
            err_msg.to_string()
        };
        emit(IncomingEvt::HelloRefused { message });
        window.request_redraw();
        return Err(HelloRefused(format!("hello rejected: {err_msg} (code={code})")).into());
    }
    Ok(frame)
}

/// Record the accepted hello: session id and the Connected event.
pub(super) fn accept_hello<Wn: Redraw>(
    frame: Frame,
    host: &HostKey,
    session: &mut SessionState,
    resolved: ResolvedDial,
    emit: &impl Fn(IncomingEvt),
    window: &Wn,
) -> Result<()> {
    let hello_res: HelloRes = serde_json::from_value(frame.payload).context("hello res")?;
    // ADR 0030 §2: a successful hello from a pre-versioning backend comes back
    // with protocol == 0. We still run (it spoke a compatible wire), but warn
    // loudly so the skew is visible in logs — the backend should be updated.
    if hello_res.protocol == 0 {
        tracing::warn!(
            backend_version = %hello_res.app_version,
            "connected to a pre-versioning backend (protocol 0) — update the backend (ADR 0030)"
        );
    }
    if session.memory.session_id.as_deref() != Some(hello_res.session_id.as_str()) {
        // First run, or backend restarted with a new session — record the
        // assigned id so the next reconnect is on the live session.
        session.memory.session_id = Some(hello_res.session_id.clone());
    }
    crate::net::state::save(host, &session.memory).ok();
    // ADR 0046 decision 1: the daemon's declared host travels on this
    // event for display only (see `State::record_declared_host` and
    // `crate::ui::nav::hosts_tree::host_label`) — the DIAL label (`host`) stays this
    // connection's tag for its whole lifetime.
    emit(IncomingEvt::Connected {
        session_id: hello_res.session_id.clone(),
        revision: hello_res.revision,
        host: hello_res.host.clone(),
        project_root: hello_res.project_root.clone(),
        proxy: hello_res.proxy,
        resolved,
        backend_version: hello_res.app_version.clone(),
    });
    window.request_redraw();
    // Manager review (round 2, finding 14): the declaration, not only the
    // dial label — this is the exact instant the declared host becomes
    // known, so it belongs in this line's own fields, not only the tree/
    // status-line projection (`host_label`) that reads it back later.
    tracing::info!(
        dial = %host,
        declared = ?hello_res.host,
        session_id = %hello_res.session_id,
        revision = hello_res.revision,
        snapshot_pending = hello_res.snapshot_pending,
        "connected"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hello_read_times_out_against_a_silent_peer() {
        // The far end is held open and never written: without a bound the
        // read waits forever.
        let (_far, near) = tokio::io::duplex(64);
        let mut rx = tokio::io::BufReader::new(near);
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_hello_reply(&mut rx, std::time::Duration::from_millis(100)),
        )
        .await
        .expect("hang guard: the bounded read must return");
        let err = r.expect_err("a silent peer must time out");
        assert!(err.to_string().contains("timed out"), "{err}");
    }

    #[test]
    fn protocol_mismatch_message_names_both_sides_of_the_skew() {
        // A NEW frontend against an OLD daemon: the daemon's gate answers
        // with its own version; this build adds its own. Both must appear.
        let payload = serde_json::json!({
            "code": "protocol_mismatch",
            "backend_protocol": sot_protocol::PROTOCOL_VERSION - 1,
            "backend_version": "0.5.9",
        });
        let msg = super::protocol_mismatch_message(&payload, "protocol mismatch");
        assert!(msg.contains(&format!("backend:  0.5.9  (protocol {})", sot_protocol::PROTOCOL_VERSION - 1)), "{msg}");
        assert!(
            msg.contains(&format!("frontend: {}  (protocol {})", sot_protocol::app_version(), sot_protocol::PROTOCOL_VERSION)),
            "{msg}"
        );
    }

    #[test]
    fn protocol_mismatch_headline_names_the_side_that_is_behind() {
        let headline = |theirs: u32, version: &str| {
            let payload = serde_json::json!({
                "code": "protocol_mismatch",
                "backend_protocol": theirs,
                "backend_version": version,
            });
            let msg = super::protocol_mismatch_message(&payload, "protocol mismatch");
            msg.lines().next().unwrap().to_string()
        };
        let ahead = sot_protocol::PROTOCOL_VERSION + 1;
        let first = headline(ahead, "9.9.9");
        assert!(first.starts_with("frontend out of date"), "{first}");
        assert!(first.contains(&format!("protocol {ahead}")), "{first}");
        let first = headline(sot_protocol::PROTOCOL_VERSION - 1, "0.5.9");
        assert!(first.starts_with("daemon out of date"), "{first}");
    }
}
