//! agent.send and agent.filed: relay a message, or a filer's receipt, to every connection.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::AgentFiledReq;
use sot_protocol::AgentFiledRes;
use sot_protocol::AgentSendReq;
use sot_protocol::AgentSendRes;
use sot_protocol::Frame;
use crate::workspaces::AgentMessage;
use tokio::sync::broadcast;
use crate::handlers::{iso8601_utc_now, HandlerOutput};

/// Relay one agent-to-agent message (`agent.send`). Parse the request,
/// snapshot the roster's `receivers_for` the target BEFORE publishing
/// (bias must be false-negative, never false-positive: a receiver that
/// disconnects between the snapshot and the publish still gets counted,
/// which is the safe direction — the alternative would let a receiver
/// that connects in that same window make an honest "nobody's there" ack
/// look wrong), stamp an ISO-8601 UTC `ts`, publish onto the agent
/// broadcast channel (every connection, including the sender's own,
/// subscribes at connection start — `receivers_for` is what makes this
/// ack meaningful, not the broadcast's own delivery, since the channel
/// always "succeeds" once at least one subscriber exists), and ack with
/// the receivers the sender can use to judge whether the send landed
/// anywhere. Mirrors the `ws_events.send(...)` leg of
/// `handle_workspace_create`.
pub async fn handle_agent_send(
    req_id: u64,
    payload_json: serde_json::Value,
    agent_tx: &broadcast::Sender<AgentMessage>,
    clients: &crate::clients::Clients,
    self_serial: Option<u64>,
) -> Result<HandlerOutput> {
    let req: AgentSendReq = serde_json::from_value(payload_json).context("agent.send payload")?;
    let receivers = clients.receivers_for(&req.to, self_serial.unwrap_or(0));
    // `id` in the log line is what correlates this send with the
    // `agent.filed` receipt below it in the journal (ADR 0048) — the
    // receipt carries no handle of its own to correlate by.
    tracing::info!(from = %req.from, to = %req.to, id = ?req.id, ?receivers, "agent.send relay");
    let msg = AgentMessage {
        from: req.from,
        to: req.to,
        text: req.text,
        ts: iso8601_utc_now(),
        id: req.id.clone(),
    };
    // Fire-and-forget broadcast; every connection subscribed at connection
    // start, so this never errs for "no receivers" — `receivers` above is
    // the roster's honest answer to that question instead.
    let _ = agent_tx.send(msg);
    Ok(vec![(
        Frame::res(
            req_id,
            op::AGENT_SEND,
            serde_json::to_value(AgentSendRes {
                ok: true,
                receivers,
                id: req.id,
            })?,
        ),
        None,
    )])
}

/// Relay one filer receipt (`agent.filed`, ADR 0048). The mirror of
/// `handle_agent_send` and, like it, stateless: there is no pending table
/// to leak, expire or lie from — the daemon relays a receipt exactly as it
/// relays a message, and the sender reading its own `id` back is the only
/// bookkeeping anyone does.
///
/// `filer_name` is THIS connection's declared hello `name`, passed in by
/// the caller and stamped onto the published receipt. Nothing in the
/// request body can name a filer (`AgentFiledReq` is one field), so a
/// receipt always names something the roster can be checked against —
/// attribution, not authentication: nothing validates a hello `name`, and
/// every client sees the frame id, so this is not a boundary against a
/// hostile client (not the threat model). A connection with no declared
/// name has nothing to be named as and is refused `bad_filer`: a receipt
/// naming nobody would put the verdict back where the guess was.
pub async fn handle_agent_filed(
    req_id: u64,
    payload_json: serde_json::Value,
    receipt_tx: &broadcast::Sender<crate::workspaces::AgentReceipt>,
    filer_name: Option<&str>,
) -> Result<HandlerOutput> {
    let req: AgentFiledReq = serde_json::from_value(payload_json).context("agent.filed payload")?;
    let Some(filer) = filer_name.filter(|n| !n.is_empty()) else {
        return Ok(vec![(
            Frame::res(
                req_id,
                op::AGENT_FILED,
                json!({
                    "error": "this connection declared no name; it cannot vouch for an append",
                    "code": "bad_filer",
                }),
            ),
            None,
        )]);
    };
    tracing::info!(id = %req.id, %filer, "agent.filed receipt");
    let _ = receipt_tx.send(crate::workspaces::AgentReceipt {
        id: req.id,
        filer: filer.to_string(),
    });
    Ok(vec![(
        Frame::res(
            req_id,
            op::AGENT_FILED,
            serde_json::to_value(AgentFiledRes { ok: true })?,
        ),
        None,
    )])
}

#[cfg(test)]
mod agent_relay_tests {
    use super::*;

    #[tokio::test]
    async fn agent_send_copies_the_id_into_the_published_message_and_echoes_it() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        let clients = crate::clients::Clients::new();
        let out = handle_agent_send(
            7,
            serde_json::json!({"from": "a", "to": "peer-otherbox", "text": "hi", "id": "x-1"}),
            &tx,
            &clients,
            None,
        )
        .await
        .expect("handler must not error");
        let published = rx.try_recv().expect("one published message");
        assert_eq!(published.id.as_deref(), Some("x-1"));
        assert_eq!(out[0].0.payload["id"], "x-1", "the ack must echo the id");
        assert_eq!(out[0].0.payload["ok"], true);
    }

    #[tokio::test]
    async fn agent_send_without_an_id_publishes_none_and_omits_it_from_the_ack() {
        // The old-sender row of the compat matrix: nothing downstream may
        // invent an id, or a filer would claim against a frame its sender
        // cannot recognize.
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        let clients = crate::clients::Clients::new();
        let out = handle_agent_send(
            8,
            serde_json::json!({"from": "a", "to": "b", "text": "hi"}),
            &tx,
            &clients,
            None,
        )
        .await
        .expect("handler must not error");
        assert_eq!(rx.try_recv().expect("one message").id, None);
        assert!(out[0].0.payload.get("id").is_none(), "got {}", out[0].0.payload);
    }

    #[tokio::test]
    async fn agent_filed_names_the_connection_not_whatever_the_request_said() {
        // The request below TRIES to name another filer; the published
        // receipt must name the connection the claim arrived on. (That is
        // attribution, not authentication — see the handler's own doc.)
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        let out = handle_agent_filed(
            9,
            serde_json::json!({"id": "x-1", "filer": "fe@someone-else"}),
            &tx,
            Some("fe@otherbox"),
        )
        .await
        .expect("handler must not error");
        assert_eq!(out[0].0.payload["ok"], true);
        let r = rx.try_recv().expect("one published receipt");
        assert_eq!(r.filer, "fe@otherbox");
        assert_eq!(r.id, "x-1");
    }

    #[tokio::test]
    async fn agent_filed_from_an_unnamed_connection_publishes_nothing() {
        // An anonymous vouch is indistinguishable from a forged one.
        for name in [None, Some("")] {
            let (tx, mut rx) = tokio::sync::broadcast::channel(4);
            let out = handle_agent_filed(
                10,
                serde_json::json!({"id": "x-1"}),
                &tx,
                name,
            )
            .await
            .expect("handler must not error");
            assert_eq!(out[0].0.payload["code"], "bad_filer", "name {name:?}");
            assert!(out[0].0.payload.get("ok").is_none(), "name {name:?}");
            assert!(rx.try_recv().is_err(), "nothing may be published for {name:?}");
        }
    }
}
