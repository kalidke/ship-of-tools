//! The chat's request ops: `chat.resolve`, `chat.history` and `chat.post`, each a job (never the loop), and
//! `chat.watch` and `chat.unwatch`, which run in the loop. Every action that reaches Slack passes `Chat::admit` first, on the blocking pool, since admission
//! canonicalizes the row's folder on the shared home; the Slack call then runs on the job's own task, and each of its
//! bounds is the API's own deadline. A post is made once here and never again: no path of this file retries, and an
//! unknown outcome is the reply's, for the client to settle by reading history.

use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use sot_chat::api::{ApiError, Bounds, Continuation, HistoryQuery, PostKind, PostOutcome};
use sot_chat::config::Resolution;
use sot_chat::http::HttpError;
use sot_protocol::ops::{
    op, ChatChannel, ChatCode, ChatHistoryReq, ChatHistoryRes, ChatPostKind, ChatPostReq, ChatPostRes, ChatResolveReq,
    ChatResolveRes, ChatTarget, ChatUnwatchReq, ChatUnwatchRes, ChatUpper, ChatWatchReq, ChatWatchRes,
};
use sot_protocol::reply::HandlerOutput;
use sot_protocol::Frame;

use super::watch::Conn;
use super::Chat;
use crate::rows::handle::bound_handle;
use crate::rows::Workspaces;

/// A refusal: the standard error payload with the fixed text and the code, never Slack's or a library's words.
fn refuse(req_id: u64, op: &str, code: ChatCode) -> HandlerOutput {
    let payload = serde_json::json!({ "error": code.text(), "code": code });
    vec![(Frame::res(req_id, op, payload), None)]
}

fn reply<T: serde::Serialize>(req_id: u64, op: &str, body: &T) -> Result<HandlerOutput> {
    Ok(vec![(Frame::res(req_id, op, serde_json::to_value(body)?), None)])
}

pub(crate) fn code_of(e: &ApiError) -> ChatCode {
    match e {
        ApiError::Http(HttpError::Busy) => ChatCode::Busy,
        ApiError::Limited { .. } => ChatCode::Limited,
        ApiError::Closed => ChatCode::Unavailable,
        ApiError::BadRequest | ApiError::BadBound | ApiError::Altered => ChatCode::BadRequest,
        ApiError::Http(
            HttpError::Timeout
            | HttpError::Destination
            | HttpError::Redirect
            | HttpError::Tls
            | HttpError::Connect
            | HttpError::Protocol
            | HttpError::TooLarge
            | HttpError::Invalid,
        )
        | ApiError::Slack(_)
        | ApiError::Status(_)
        | ApiError::Malformed
        | ApiError::NoIdentity
        | ApiError::WrongWorkspace
        | ApiError::Repeated
        | ApiError::Missing
        | ApiError::TooLarge => ChatCode::Failed,
    }
}

/// The code of a post Slack or the client refused before it was sent or definitely not posted.
pub(crate) fn post_refusal_code(why: &str) -> ChatCode {
    match why {
        why if why == HttpError::Busy.code() => ChatCode::Busy,
        why if why == HttpError::Invalid.code() => ChatCode::BadRequest,
        _ => ChatCode::Failed,
    }
}

/// Admission off the runtime's workers: it canonicalizes on the shared home, which can block.
pub(super) async fn admitted(chat: &Arc<Chat>, rows: &Workspaces, target: ChatTarget) -> Result<Arc<sot_chat::service::Service>, ChatCode> {
    let (chat, rows) = (chat.clone(), rows.clone());
    tokio::task::spawn_blocking(move || chat.admit(&rows, &target)).await.unwrap_or(Err(ChatCode::Failed))
}

/// A payload that does not decode (an unknown post `kind` included) is `bad_request`.
fn decode<T: for<'de> Deserialize<'de>>(payload: serde_json::Value) -> Result<T, ChatCode> {
    serde_json::from_value(payload).map_err(|_| ChatCode::BadRequest)
}

pub(crate) async fn resolve(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_RESOLVE;
    let req = match decode::<ChatResolveReq>(payload) {
        Ok(r) => r,
        Err(c) => return Ok(refuse(req_id, NAME, c)),
    };
    let (held, rows) = (chat.clone(), rows.clone());
    let resolved = tokio::task::spawn_blocking(move || held.resolve(&rows, &req.row)).await;
    match resolved.unwrap_or(Err(ChatCode::Failed)) {
        Err(code) => Ok(refuse(req_id, NAME, code)),
        Ok((Resolution::NotHub, _)) => Ok(refuse(req_id, NAME, ChatCode::Unavailable)),
        Ok((Resolution::Unmapped, _)) => reply(req_id, NAME, &ChatResolveRes::Unmapped),
        Ok((Resolution::Invalid(_), _)) => reply(req_id, NAME, &ChatResolveRes::Invalid),
        Ok((Resolution::Mapped { workspace, channel, .. }, delivers)) => match ChatChannel::try_from(channel) {
            Ok(channel) => {
                // The cache alone: a resolve makes no Slack call, and a name not yet found leaves the id.
                let name = chat.service().and_then(|s| s.api().ok()).and_then(|api| api.cached_name(channel.as_str())).filter(|n| !n.is_empty());
                reply(req_id, NAME, &ChatResolveRes::Mapped { workspace, channel, delivers, name })
            }
            Err(_) => reply(req_id, NAME, &ChatResolveRes::Invalid),
        },
    }
}

pub(crate) async fn history(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_HISTORY;
    let req = match decode::<ChatHistoryReq>(payload) {
        Ok(r) => r,
        Err(c) => return Ok(refuse(req_id, NAME, c)),
    };
    let service = match admitted(chat, rows, req.target.clone()).await {
        Ok(s) => s,
        Err(c) => return Ok(refuse(req_id, NAME, c)),
    };
    let api = match service.api() {
        Ok(a) => a,
        Err(_) => return Ok(refuse(req_id, NAME, ChatCode::Unavailable)),
    };
    let (before, until) = match &req.upper {
        Some(ChatUpper::Before(t)) => (Some(t.as_str()), None),
        Some(ChatUpper::Until(t)) => (None, Some(t.as_str())),
        None => (None, None),
    };
    let query = Bounds::new(req.after.as_ref().map(|t| t.as_str()), before, until)
        .and_then(|b| HistoryQuery::new(req.target.channel.as_str(), req.thread.as_ref().map(|t| t.as_str()), b, req.limit));
    let cont = req.cursor.clone().map(Continuation::parse).transpose();
    let (query, cont) = match (query, cont) {
        (Ok(q), Ok(c)) => (q, c),
        (Err(e), _) | (_, Err(e)) => return Ok(refuse(req_id, NAME, code_of(&e))),
    };
    let mut page = match api.history(&query, cont.as_ref()).await {
        Ok(p) => p,
        Err(e) => return Ok(refuse(req_id, NAME, code_of(&e))),
    };
    api.fill_names(&mut page.records).await;
    reply(req_id, NAME, &ChatHistoryRes { records: page.records, next: page.next.map(|c| c.as_str().to_string()), limited: page.limited })
}

/// `chat.post`. After admission, two refusals by the connection's declared role and the row's bound handle, each
/// `bad_request` before any Slack call: a person's post only from a window (a row's own `Drawer` post would be mail to
/// itself), and a row's post only under its own bound handle. The role is the client's own declaration, so this is
/// attribution among one account's programs, not a security boundary.
pub(crate) async fn post(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces, window: bool) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_POST;
    let req = match decode::<ChatPostReq>(payload) {
        Ok(r) => r,
        Err(c) => return Ok(refuse(req_id, NAME, c)),
    };
    let service = match admitted(chat, rows, req.target.clone()).await {
        Ok(s) => s,
        Err(c) => return Ok(refuse(req_id, NAME, c)),
    };
    let allowed = match req.kind {
        ChatPostKind::Person => window,
        ChatPostKind::Row => {
            let all = rows.list();
            rows.resolve(Some(&req.target.row)).is_some_and(|ws| bound_handle(&ws, &all) == req.name)
        }
    };
    if !allowed {
        return Ok(refuse(req_id, NAME, ChatCode::BadRequest));
    }
    let api = match service.api() {
        Ok(a) => a,
        Err(_) => return Ok(refuse(req_id, NAME, ChatCode::Unavailable)),
    };
    let kind = match req.kind {
        ChatPostKind::Person => PostKind::Person,
        ChatPostKind::Row => PostKind::Row,
    };
    let refused_post = |code| reply(req_id, NAME, &ChatPostRes::Refused { code });
    // The one call: nothing below sends again, whatever it answers.
    let outcome = api.post(req.target.channel.as_str(), req.thread.as_ref().map(|t| t.as_str()), kind, &req.name, &req.text).await;
    match outcome {
        Err(e) => refused_post(code_of(&e)),
        Ok(PostOutcome::Posted(ts)) => reply(req_id, NAME, &ChatPostRes::Posted { ts }),
        Ok(PostOutcome::Refused(why)) => refused_post(post_refusal_code(&why)),
        Ok(PostOutcome::Limited { retry_after, assumed }) => {
            reply(req_id, NAME, &ChatPostRes::Limited { retry_after_secs: retry_after.as_secs(), assumed })
        }
        Ok(PostOutcome::Unknown) => reply(req_id, NAME, &ChatPostRes::Unknown),
    }
}

/// `chat.watch`, run in the connection's loop (`INLINE`): admitted like any action that reaches Slack, then registered
/// with the owner, which begins the snapshot off the loop. The reply is written before the loop reads another event,
/// so no `chat.changed` for the channel precedes it. A watch refused at admission releases the connection's slot for
/// the channel, so a resync that finds its row gone frees what it held.
pub(crate) async fn watch(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces, conn: &Conn) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_WATCH;
    let req = match decode::<ChatWatchReq>(payload) {
        Ok(r) => r,
        Err(c) => return Ok(refuse(req_id, NAME, c)),
    };
    let channel = req.target.channel.as_str().to_string();
    let service = match admitted(chat, rows, req.target.clone()).await {
        Ok(s) => s,
        Err(c) => {
            conn.release(&channel);
            return Ok(refuse(req_id, NAME, c));
        }
    };
    match conn.watch(req.target, &service, chat, rows).await {
        Ok(state) => reply(req_id, NAME, &ChatWatchRes { state }),
        Err(c) => Ok(refuse(req_id, NAME, c)),
    }
}

/// `chat.unwatch`: ends this connection's watch of the channel and no other's. It needs a running service and no
/// current row (`Chat::admit_unwatch`): refusing it for a stale one would keep the slot.
pub(crate) async fn unwatch(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, conn: &Conn) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_UNWATCH;
    let req = match decode::<ChatUnwatchReq>(payload) {
        Ok(r) => r,
        Err(c) => return Ok(refuse(req_id, NAME, c)),
    };
    if let Err(c) = chat.admit_unwatch() {
        return Ok(refuse(req_id, NAME, c));
    }
    conn.release(req.target.channel.as_str());
    reply(req_id, NAME, &ChatUnwatchRes {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_carries_its_codes_one_text() {
        let out = refuse(7, "chat.watch", ChatCode::StaleMapping);
        assert_eq!(out[0].0.payload, serde_json::json!({ "error": ChatCode::StaleMapping.text(), "code": "stale_mapping" }));
    }
}
