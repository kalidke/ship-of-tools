//! The payloads of the daemon's agent.message and agent.receipt broadcast buses.

/// One relayed agent-to-agent message. The daemon broadcasts one per
/// `agent.send`; each connection turns it into an `agent.message` evt
/// frame. Mirrors `WorkspaceChanged` — a small Clone+Debug payload type
/// fanned out over a `broadcast::channel`. `to == ""` means broadcast.
/// `ts` is an ISO-8601 UTC string stamped by the daemon on receipt.
#[derive(Clone, Debug)]
pub struct AgentMessage {
    pub from: String,
    pub to: String,
    pub text: String,
    pub ts: String,
    /// The sender's opaque frame id (ADR 0048), when the request carried
    /// one. Copied through untouched so the filer can attribute its
    /// `agent.filed` claim to exactly this frame; `None` from a sender
    /// that predates receipts, and then nobody claims anything.
    pub id: Option<String>,
}

/// One relayed filer receipt (ADR 0048) — the daemon broadcasts one per
/// `agent.filed`; each connection turns it into an `agent.receipt` evt.
/// Mirrors `AgentMessage`. `filer` is stamped by the daemon from the
/// answering connection's declared hello `name`, so a filer names itself
/// rather than being guessed at; it is attribution, not authentication.
#[derive(Clone, Debug)]
pub struct AgentReceipt {
    pub id: String,
    pub filer: String,
}
