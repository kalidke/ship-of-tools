//! chat.resolve, chat.watch, chat.unwatch, chat.history, chat.post: the requests (send_chat: write the frame, then record its PendingKind).
//! Their replies (on_chat_*: the reply frame becomes an `IncomingEvt::Chat`), and the `chat.changed` and `chat.status` events.

use super::*;
use sot_protocol::ops::{
    ChatChangedEvt, ChatCode, ChatHistoryReq, ChatHistoryRes, ChatPostReq, ChatPostRes, ChatRefusal, ChatResolveRes, ChatStatusEvt,
    ChatTarget, ChatWatchRes,
};

/// What a chat request's reply came to, apart from the answer itself.
#[derive(Debug)]
pub(crate) enum ChatReply<T> {
    Done(T),
    /// The daemon predates the op (its generic unknown-op answer, `is_unsupported`): the chat is not offered here.
    Unsupported,
    /// An error reply, by its `code` (`Other` when it names none the window knows), or an answer that does not parse.
    Refused(ChatCode),
}

/// The chat family's events: each reply carries what its request named, never what the window shows by then.
#[derive(Debug)]
pub(crate) enum ChatEvt {
    Resolved { row: String, reply: ChatReply<ChatResolveRes> },
    Watched { target: ChatTarget, reply: ChatReply<ChatWatchRes> },
    History { req: ChatHistoryReq, reply: ChatReply<ChatHistoryRes> },
    /// Never `Refused`: a refusal is `Done(ChatPostRes::Refused)`, and anything that may have posted is `Done(Unknown)`.
    Posted { req: ChatPostReq, reply: ChatReply<ChatPostRes> },
    /// Refused here, never written: the connection's daemon does not say its door posts plain text (T20), so the post was not made.
    PostNotSent { req: ChatPostReq },
    Changed(ChatChangedEvt),
    Status(ChatStatusEvt),
    /// A pushed `chat.changed` or `chat.status` that did not parse: what it carried is lost, so the owner resyncs.
    Unreadable,
}

pub(crate) async fn send_chat<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    op: &str,
    payload: impl serde::Serialize,
    kind: Option<PendingKind>,
) -> Result<()> {
    tracing::debug!(id, "→ {op}");
    let payload = serde_json::to_value(payload)?;
    // Recorded before the write, so a write that fails (an oversize envelope ends the session) still ends the request
    // once, by the guard. `chat.unwatch` is fire-and-forget: its empty answer is not tracked.
    pending.extend(kind.map(|k| (id, k)));
    codec::write_frame(&mut tx, &Frame::req(id, op, payload), None).await?;
    Ok(())
}

fn chat_reply<T: serde::de::DeserializeOwned>(frame: Frame) -> ChatReply<T> {
    if is_unsupported(&frame.op, &frame.payload) {
        return ChatReply::Unsupported;
    }
    if frame.payload.get("error").is_some() {
        let refusal = serde_json::from_value::<ChatRefusal>(frame.payload).ok();
        return ChatReply::Refused(refusal.and_then(|r| r.code).unwrap_or(ChatCode::Other));
    }
    serde_json::from_value(frame.payload).map_or(ChatReply::Refused(ChatCode::Other), ChatReply::Done)
}

pub(crate) fn on_chat_resolve(frame: Frame, emit: &impl Fn(IncomingEvt), row: String) {
    emit(IncomingEvt::Chat(ChatEvt::Resolved { row, reply: chat_reply(frame) }));
}

pub(crate) fn on_chat_watch(frame: Frame, emit: &impl Fn(IncomingEvt), target: ChatTarget) {
    emit(IncomingEvt::Chat(ChatEvt::Watched { target, reply: chat_reply(frame) }));
}

pub(crate) fn on_chat_history(frame: Frame, emit: &impl Fn(IncomingEvt), req: ChatHistoryReq) {
    emit(IncomingEvt::Chat(ChatEvt::History { req, reply: chat_reply(frame) }));
}

/// A post is never resent, so only a reply that proves it was not posted says so: an error reply whose code is not one of
/// the wire's refusals (a panicking handler answers `handler_error`, which reads as `Other`) and an answer that does not
/// parse may have posted, and are `Unknown`.
pub(crate) fn on_chat_post(frame: Frame, emit: &impl Fn(IncomingEvt), req: ChatPostReq) {
    let reply = match chat_reply::<ChatPostRes>(frame) {
        ChatReply::Refused(ChatCode::Other) => ChatReply::Done(ChatPostRes::Unknown),
        ChatReply::Refused(code) => ChatReply::Done(ChatPostRes::Refused { code }),
        other => other,
    };
    emit(IncomingEvt::Chat(ChatEvt::Posted { req, reply }));
}

/// `chat.changed` and `chat.status`, pushed unasked; one that does not parse is a warning and a `ChatEvt::Unreadable`.
pub(crate) fn on_chat_event(frame: Frame, emit: &impl Fn(IncomingEvt)) {
    let name = frame.op;
    let event = if name == op::CHAT_CHANGED {
        serde_json::from_value(frame.payload).map(ChatEvt::Changed)
    } else {
        serde_json::from_value(frame.payload).map(ChatEvt::Status)
    };
    match event {
        Ok(event) => emit(IncomingEvt::Chat(event)),
        Err(e) => {
            tracing::warn!(%name, error = %e, "chat evt parse failed");
            emit(IncomingEvt::Chat(ChatEvt::Unreadable));
        }
    }
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
