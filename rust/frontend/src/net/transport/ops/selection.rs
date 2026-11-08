//! selection.set: the request (send_selection_set) and its reply (on_selection_set: the reply frame becomes one `IncomingEvt::Selection`).

use super::*;
use sot_protocol::ops::{SelectionSetReq, SelectionSetRes, SelectionState};

/// What a `selection.set` came to.
#[derive(Debug)]
pub(crate) enum SelectionReply {
    Done(SelectionState),
    /// The daemon predates the op (its generic unknown-op answer, `is_unsupported`).
    Unsupported,
    /// An error reply (`code` is the wire's refusal code, `None` when it names none), or an answer that does not parse.
    Refused { code: Option<String>, error: String },
}

pub(crate) async fn send_selection_set<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    req: SelectionSetReq,
) -> Result<()> {
    tracing::debug!(id, "→ {}", op::SELECTION_SET);
    // Recorded before the write, as the chat requests are, so a write that fails still ends the request once.
    pending.insert(id, PendingKind::SelectionSet { workspace_id: req.workspace_id.clone() });
    codec::write_frame(&mut tx, &Frame::req(id, op::SELECTION_SET, serde_json::to_value(&req)?), None).await?;
    Ok(())
}

/// The reply carries the row its request named, never the row the window shows by then.
pub(crate) fn on_selection_set(frame: Frame, emit: &impl Fn(IncomingEvt), workspace_id: String) {
    let reply = if is_unsupported(&frame.op, &frame.payload) {
        SelectionReply::Unsupported
    } else if let Some(error) = frame.payload.get("error").and_then(|v| v.as_str()) {
        let code = frame.payload.get("code").and_then(|v| v.as_str()).map(str::to_string);
        SelectionReply::Refused { code, error: error.to_string() }
    } else {
        match serde_json::from_value::<SelectionSetRes>(frame.payload) {
            Ok(res) => SelectionReply::Done(res.state),
            Err(e) => SelectionReply::Refused { code: None, error: format!("unreadable reply: {e}") },
        }
    };
    emit(IncomingEvt::Selection { workspace_id, reply });
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
