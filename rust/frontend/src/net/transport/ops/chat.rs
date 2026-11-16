//! chat.resolve, chat.watch, chat.unwatch, chat.history, chat.post, chat.file, chat.connect, chat.channels, chat.pair: the requests (send_chat: write the frame, then record its PendingKind).
//! Their replies (on_chat_*: the reply frame becomes an `IncomingEvt::Chat`; `chat.file`'s alone carries a blob), and the `chat.changed` and `chat.status` events.

use super::*;
use sot_protocol::ops::{
    ChatChangedEvt, ChatChannel, ChatChannelsRes, ChatCode, ChatConnectRes, ChatFileReq, ChatFileRes, ChatFileVariant, ChatHistoryReq, ChatHistoryRes, ChatPairRes, ChatPostReq,
    ChatPostRes, ChatRefusal, ChatResolveRes, ChatStatusEvt, ChatTarget, ChatWatchRes, MAX_CHAT_FILE, MAX_THUMB,
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
    /// A `chat.connect`'s reply. It names nothing of the request, so no token is held here.
    Connect { reply: ChatReply<ChatConnectRes> },
    /// The connection ended, or the daemon's answer did not prove it, before a `chat.connect` was answered: the tokens
    /// may have been saved, so the owner resolves again and never sends the request again.
    ConnectUnknown,
    Channels { reply: ChatReply<ChatChannelsRes> },
    /// A `chat.pair`'s reply, with the row and the channel it named (none: the row was unpaired).
    Paired { row: String, channel: Option<ChatChannel>, reply: ChatReply<ChatPairRes> },
    /// As `ConnectUnknown`, for a `chat.pair`: the file may have been written.
    PairUnknown { row: String, channel: Option<ChatChannel> },
    /// A `chat.file`'s reply: the picture's bytes, within its variant's bound, or why there are none. A read: a connection
    /// that ends first gives no event, and the owner asks again at `Connected`.
    File { req: ChatFileReq, reply: ChatReply<ChatPicture> },
    Changed(ChatChangedEvt),
    Status(ChatStatusEvt),
    /// A pushed `chat.changed` or `chat.status` that did not parse: what it carried is lost, so the owner resyncs.
    Unreadable,
}

/// A picture's bytes from `chat.file`, as the daemon served them; its `Debug` prints the byte count, not the bytes.
pub(crate) struct ChatPicture {
    pub(crate) mime: String,
    pub(crate) bytes: Vec<u8>,
}

impl std::fmt::Debug for ChatPicture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChatPicture {{ mime: {:?}, bytes: {} }}", self.mime, self.bytes.len())
    }
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

/// The codec read the blob before this decode, so its length is checked here against the request's own variant (a
/// thumbnail's `MAX_THUMB`, not the wire's `MAX_CHAT_FILE`): a reply over it, or whose blob is not the one it declares,
/// is `Refused(Failed)`.
pub(crate) fn on_chat_file(frame: Frame, blob: Option<Vec<u8>>, emit: &impl Fn(IncomingEvt), req: ChatFileReq) {
    let bound = match req.variant {
        ChatFileVariant::Thumb => MAX_THUMB,
        ChatFileVariant::Original => MAX_CHAT_FILE,
    };
    let reply = match chat_reply::<ChatFileRes>(frame) {
        ChatReply::Done(res) => match blob {
            Some(bytes) if bytes.len() as u64 == res.blob.len && bytes.len() <= bound => ChatReply::Done(ChatPicture { mime: res.mime, bytes }),
            _ => ChatReply::Refused(ChatCode::Failed),
        },
        ChatReply::Unsupported => ChatReply::Unsupported,
        ChatReply::Refused(code) => ChatReply::Refused(code),
    };
    emit(IncomingEvt::Chat(ChatEvt::File { req, reply }));
}

/// A connect or a pair is written once, so, as for a post, only a reply that proves nothing was saved says so: an error
/// reply whose code is not one of the wire's refusals (`handler_error`) and an answer that does not parse may have been
/// carried out, and are the unknown outcome (`None`).
fn proven<T: serde::de::DeserializeOwned>(frame: Frame) -> Option<ChatReply<T>> {
    match chat_reply(frame) {
        ChatReply::Refused(ChatCode::Other) => None,
        reply => Some(reply),
    }
}

pub(crate) fn on_chat_connect(frame: Frame, emit: &impl Fn(IncomingEvt)) {
    emit(IncomingEvt::Chat(match proven(frame) {
        Some(reply) => ChatEvt::Connect { reply },
        None => ChatEvt::ConnectUnknown,
    }));
}

pub(crate) fn on_chat_channels(frame: Frame, emit: &impl Fn(IncomingEvt)) {
    emit(IncomingEvt::Chat(ChatEvt::Channels { reply: chat_reply(frame) }));
}

pub(crate) fn on_chat_pair(frame: Frame, emit: &impl Fn(IncomingEvt), row: String, channel: Option<ChatChannel>) {
    emit(IncomingEvt::Chat(match proven(frame) {
        Some(reply) => ChatEvt::Paired { row, channel, reply },
        None => ChatEvt::PairUnknown { row, channel },
    }));
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
