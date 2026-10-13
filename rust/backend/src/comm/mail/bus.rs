//! The payload of the daemon's agent.message broadcast bus: UI notifications, never mail.

/// One UI notification. The daemon publishes one per accepted `agent.send` (whose `to` is always empty); each
/// connection turns it into an `agent.message` evt frame. Mirrors `WorkspaceChanged` — a small Clone+Debug payload
/// type fanned out over a `broadcast::channel`. `ts` is an ISO-8601 UTC string stamped by the daemon on receipt.
#[derive(Clone, Debug)]
pub struct AgentMessage {
    pub from: String,
    pub to: String,
    pub text: String,
    pub ts: String,
}
