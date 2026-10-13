//! agent.send: an empty-target UI notification (the nav envelope, the ROI echo), published to every connection.
//! Mail never rides it: a directed send is refused, and mail goes through `comm.file`.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::AgentSendReq;
use sot_protocol::AgentSendRes;
use sot_protocol::Frame;
use crate::comm::mail::bus::AgentMessage;
use tokio::sync::broadcast;
use crate::comm::registry::registry::iso8601_utc_now;
use crate::server::reply::HandlerOutput;

/// `agent.send`: publish one UI notification onto the agent bus, which every connection writes as an
/// `agent.message` evt, and answer `ok`. The answer says the daemon published it, never that anyone read it or that
/// anything was filed. A request with a non-empty `to` is mail and is refused `mail_requires_comm_file`, publishing
/// nothing.
pub async fn handle_agent_send(
    req_id: u64,
    payload_json: serde_json::Value,
    agent_tx: &broadcast::Sender<AgentMessage>,
) -> Result<HandlerOutput> {
    let req: AgentSendReq = serde_json::from_value(payload_json).context("agent.send payload")?;
    if !req.to.is_empty() {
        tracing::info!(from = %req.from, to = %req.to, "agent.send refused: directed mail");
        let refusal = json!({ "error": "mail must use comm.file", "code": "mail_requires_comm_file" });
        return Ok(vec![(Frame::res(req_id, op::AGENT_SEND, refusal), None)]);
    }
    tracing::info!(from = %req.from, "agent.send notification");
    let msg = AgentMessage { from: req.from, to: req.to, text: req.text, ts: iso8601_utc_now() };
    // Every connection subscribed at its start, so this fails only with no connection at all, and then nobody was
    // there to show it to.
    let _ = agent_tx.send(msg);
    Ok(vec![(Frame::res(req_id, op::AGENT_SEND, serde_json::to_value(AgentSendRes { ok: true })?), None)])
}

#[cfg(test)]
mod agent_relay_tests {
    use super::*;

    #[tokio::test]
    async fn an_empty_target_notification_is_published_and_acknowledged() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        let out = handle_agent_send(7, json!({"from": "a", "to": "", "text": "{\"sot_ui\":{}}"}), &tx)
            .await
            .expect("handler must not error");
        let published = rx.try_recv().expect("one published notification");
        assert_eq!((published.from.as_str(), published.to.as_str()), ("a", ""));
        assert_eq!(published.text, "{\"sot_ui\":{}}");
        assert_eq!(out[0].0.payload, json!({"ok": true}));
    }

    #[tokio::test]
    async fn directed_mail_is_refused_and_publishes_nothing() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        let out = handle_agent_send(8, json!({"from": "a", "to": "peer-otherbox", "text": "hi", "id": "x-1"}), &tx)
            .await
            .expect("handler must not error");
        assert_eq!(out[0].0.payload, json!({"error": "mail must use comm.file", "code": "mail_requires_comm_file"}));
        assert!(rx.try_recv().is_err(), "directed mail was published");
    }
}
