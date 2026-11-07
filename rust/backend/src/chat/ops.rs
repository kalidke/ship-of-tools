//! The chat's request ops, served from a control session's job (never its loop): `chat.resolve`, `chat.history` and
//! `chat.post`. Every action that reaches Slack passes `Chat::admit` first, on the blocking pool, since admission
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
    ChatResolveRes, ChatTarget, ChatUpper,
};
use sot_protocol::reply::HandlerOutput;
use sot_protocol::Frame;

use super::Chat;
use crate::rows::Workspaces;

fn text(code: ChatCode) -> &'static str {
    match code {
        ChatCode::Unavailable => "the chat is not available here",
        ChatCode::Unmapped => "the row's folder maps to no channel",
        ChatCode::StaleRow => "the row is gone or was replaced",
        ChatCode::StaleMapping => "the row now maps to another channel",
        ChatCode::BadRequest => "the request is not valid",
        ChatCode::TooManyWatches => "too many channels are watched",
        ChatCode::Limited => "the chat service is rate limited",
        ChatCode::Busy => "the chat service is busy",
        ChatCode::Failed | ChatCode::Other => "the chat request failed",
    }
}

/// A refusal: the standard error payload with the fixed text and the code, never Slack's or a library's words.
fn refuse(req_id: u64, op: &str, code: ChatCode) -> HandlerOutput {
    let payload = serde_json::json!({ "error": text(code), "code": code });
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
        ApiError::Http(_)
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
        "busy" => ChatCode::Busy,
        "invalid" => ChatCode::BadRequest,
        _ => ChatCode::Failed,
    }
}

/// Admission off the runtime's workers: it canonicalizes on the shared home, which can block.
async fn admitted(chat: &Arc<Chat>, rows: &Workspaces, target: ChatTarget) -> Result<Arc<sot_chat::service::Service>, ChatCode> {
    let (chat, rows) = (chat.clone(), rows.clone());
    tokio::task::spawn_blocking(move || chat.admit(&rows, &target)).await.unwrap_or(Err(ChatCode::Failed))
}

/// One chat request. A payload that does not decode (an unknown post `kind` included) is `bad_request`.
pub(crate) async fn handle(
    req_id: u64, name: &str, payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces,
) -> Result<HandlerOutput> {
    fn decode<T: for<'de> Deserialize<'de>>(payload: serde_json::Value) -> Result<T, ChatCode> {
        serde_json::from_value(payload).map_err(|_| ChatCode::BadRequest)
    }
    let refused = |code| Ok(refuse(req_id, name, code));
    match name {
        op::CHAT_RESOLVE => {
            let req = match decode::<ChatResolveReq>(payload) {
                Ok(r) => r,
                Err(c) => return refused(c),
            };
            let (chat, rows) = (chat.clone(), rows.clone());
            let resolved = tokio::task::spawn_blocking(move || chat.resolve(&rows, &req.row)).await;
            match resolved.unwrap_or(Err(ChatCode::Failed)) {
                Err(code) => refused(code),
                Ok((Resolution::NotHub, _)) => refused(ChatCode::Unavailable),
                Ok((Resolution::Unmapped, _)) => reply(req_id, name, &ChatResolveRes::Unmapped),
                Ok((Resolution::Invalid(_), _)) => reply(req_id, name, &ChatResolveRes::Invalid),
                Ok((Resolution::Mapped { workspace, channel, .. }, delivers)) => match ChatChannel::try_from(channel) {
                    Ok(channel) => reply(req_id, name, &ChatResolveRes::Mapped { workspace, channel, delivers }),
                    Err(_) => reply(req_id, name, &ChatResolveRes::Invalid),
                },
            }
        }
        op::CHAT_HISTORY => {
            let req = match decode::<ChatHistoryReq>(payload) {
                Ok(r) => r,
                Err(c) => return refused(c),
            };
            let service = match admitted(chat, rows, req.target.clone()).await {
                Ok(s) => s,
                Err(c) => return refused(c),
            };
            let api = match service.api() {
                Ok(a) => a,
                Err(_) => return refused(ChatCode::Unavailable),
            };
            let (before, until) = match &req.upper {
                Some(ChatUpper::Before(t)) => (Some(t.as_str()), None),
                Some(ChatUpper::Until(t)) => (None, Some(t.as_str())),
                None => (None, None),
            };
            let query = Bounds::new(req.after.as_ref().map(|t| t.as_str()), before, until).and_then(|b| {
                HistoryQuery::new(req.target.channel.as_str(), req.thread.as_ref().map(|t| t.as_str()), b, req.limit)
            });
            let cont = req.cursor.clone().map(Continuation::parse).transpose();
            let (query, cont) = match (query, cont) {
                (Ok(q), Ok(c)) => (q, c),
                (Err(e), _) | (_, Err(e)) => return refused(code_of(&e)),
            };
            let mut page = match api.history(&query, cont.as_ref()).await {
                Ok(p) => p,
                Err(e) => return refused(code_of(&e)),
            };
            api.fill_names(&mut page.records).await;
            reply(
                req_id,
                name,
                &ChatHistoryRes { records: page.records, next: page.next.map(|c| c.as_str().to_string()), limited: page.limited },
            )
        }
        op::CHAT_POST => {
            let req = match decode::<ChatPostReq>(payload) {
                Ok(r) => r,
                Err(c) => return refused(c),
            };
            let service = match admitted(chat, rows, req.target.clone()).await {
                Ok(s) => s,
                Err(c) => return refused(c),
            };
            let api = match service.api() {
                Ok(a) => a,
                Err(_) => return refused(ChatCode::Unavailable),
            };
            let kind = match req.kind {
                ChatPostKind::Person => PostKind::Person,
                ChatPostKind::Row => PostKind::Row,
            };
            let refused_post = |code| reply(req_id, name, &ChatPostRes::Refused { code });
            // The one call: nothing below sends again, whatever it answers.
            let outcome = api
                .post(req.target.channel.as_str(), req.thread.as_ref().map(|t| t.as_str()), kind, &req.name, &req.text)
                .await;
            match outcome {
                Err(e) => refused_post(code_of(&e)),
                Ok(PostOutcome::Posted(ts)) => reply(req_id, name, &ChatPostRes::Posted { ts }),
                Ok(PostOutcome::Refused(why)) => refused_post(post_refusal_code(&why)),
                Ok(PostOutcome::Limited { retry_after, assumed }) => {
                    reply(req_id, name, &ChatPostRes::Limited { retry_after_secs: retry_after.as_secs(), assumed })
                }
                Ok(PostOutcome::Unknown) => reply(req_id, name, &ChatPostRes::Unknown),
            }
        }
        // `chat.watch` and `chat.unwatch`, the only other names dispatch sends here: no subscription is made until
        // watches are served, and nothing is admitted.
        _ => refused(ChatCode::Unavailable),
    }
}
