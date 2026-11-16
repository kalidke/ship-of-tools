//! The chat's request ops: `chat.resolve`, `chat.history`, `chat.post` and `chat.post_file`, each a job (never the
//! loop), and `chat.watch` and `chat.unwatch`, which run in the loop. Every action that reaches Slack passes `Chat::admit` first, on the blocking pool, since admission
//! canonicalizes the row's folder on the shared home; the Slack call then runs on the job's own task, and each of its
//! bounds is the API's own deadline. A post is made once here and never again: no path of this file retries, and an
//! unknown outcome is the reply's, for the client to settle by reading history.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;
use sot_chat::api::{ApiError, Bounds, Continuation, FileOutcome, HistoryQuery, PostKind, PostOutcome};
use sot_chat::config::{self, Resolution};
use sot_chat::http::HttpError;
use sot_protocol::ops::{
    op, ChatChannel, ChatCode, ChatHistoryReq, ChatHistoryRes, ChatPostFileReq, ChatPostFileRes, ChatPostKind,
    ChatPostReq, ChatPostRes, ChatResolveReq, ChatResolveRes, ChatTarget, ChatUnwatchReq, ChatUnwatchRes, ChatUpper,
    ChatWatchReq, ChatWatchRes, MAX_CHAT_FILE,
};
use sot_protocol::reply::HandlerOutput;
use sot_protocol::Frame;

use super::watch::Conn;
use super::Chat;
use crate::rows::handle::bound_handle;
use crate::rows::{Workspace, Workspaces};

/// A refusal: the standard error payload with the fixed text and the code, never Slack's or a library's words.
pub(super) fn refuse(req_id: u64, op: &str, code: ChatCode) -> HandlerOutput {
    let payload = serde_json::json!({ "error": code.text(), "code": code });
    vec![(Frame::res(req_id, op, payload), None)]
}

pub(super) fn reply<T: serde::Serialize>(req_id: u64, op: &str, body: &T) -> Result<HandlerOutput> {
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
pub(super) fn decode<T: for<'de> Deserialize<'de>>(payload: serde_json::Value) -> Result<T, ChatCode> {
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
    let resolved = resolved.unwrap_or(Err(ChatCode::Failed));
    // After the hub fact and the row, before the mapping: a chat a connect would fix says so.
    // A daemon that is not the hub is off as `NotHub`, which no connect fixes, so it never reads as unconfigured here.
    if let (Ok(_), Some(reason)) = (&resolved, super::setup::unconfigured(chat.status())) {
        return reply(req_id, NAME, &ChatResolveRes::Unconfigured { reason });
    }
    match resolved {
        Err(code) => Ok(refuse(req_id, NAME, code)),
        Ok((Resolution::NotHub, _)) => Ok(refuse(req_id, NAME, ChatCode::Unavailable)),
        Ok((Resolution::Unmapped, _)) => reply(req_id, NAME, &ChatResolveRes::Unmapped),
        Ok((Resolution::Invalid(_), _)) => reply(req_id, NAME, &ChatResolveRes::Invalid),
        Ok((Resolution::Mapped { workspace, channel, .. }, delivers)) => match ChatChannel::try_from(channel) {
            Ok(channel) => {
                // The cache alone: a resolve makes no Slack call, and a name not yet found leaves the id.
                let name = chat.service().and_then(|s| s.api().ok()).and_then(|api| api.cached_name(channel.as_str()));
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

/// Who posts as whom, for `chat.post` and `chat.post_file` alike, checked after admission and before any Slack call: a
/// person's post only from a window (a row's own `Drawer` post would be mail to itself), and a row's post only under its
/// own bound handle. The role is the client's own declaration, so this is attribution among one account's programs, not
/// a security boundary.
fn may_post(kind: ChatPostKind, name: &str, row: &str, rows: &Workspaces, window: bool) -> bool {
    match kind {
        ChatPostKind::Person => window,
        ChatPostKind::Row => {
            let all = rows.list();
            rows.resolve(Some(row)).is_some_and(|ws| bound_handle(&ws, &all) == name)
        }
    }
}

fn post_kind(kind: ChatPostKind) -> PostKind {
    match kind {
        ChatPostKind::Person => PostKind::Person,
        ChatPostKind::Row => PostKind::Row,
    }
}

/// `chat.post`. After admission, [`may_post`]'s two refusals, each `bad_request` before any Slack call.
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
    if !may_post(req.kind, &req.name, &req.target.row, rows, window) {
        return Ok(refuse(req_id, NAME, ChatCode::BadRequest));
    }
    let api = match service.api() {
        Ok(a) => a,
        Err(_) => return Ok(refuse(req_id, NAME, ChatCode::Unavailable)),
    };
    let refused_post = |code| reply(req_id, NAME, &ChatPostRes::Refused { code });
    // The one call: nothing below sends again, whatever it answers.
    let outcome = api.post(req.target.channel.as_str(), req.thread.as_ref().map(|t| t.as_str()), post_kind(req.kind), &req.name, &req.text).await;
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

/// `chat.post_file`: one file of the row's folder shared once. In order, each refusal before anything is read or sent
/// past it: admission, [`may_post`] (`bad_request`), a transfer permit (`busy`), the file ([`read_file`]: `not_a_file`,
/// `too_large`), then the one `Api::post_file`, whose outcome is the reply's and is never retried.
pub(crate) async fn post_file(req_id: u64, payload: serde_json::Value, chat: &Arc<Chat>, rows: &Workspaces, window: bool) -> Result<HandlerOutput> {
    const NAME: &str = op::CHAT_POST_FILE;
    let req = match decode::<ChatPostFileReq>(payload) {
        Ok(r) => r,
        Err(c) => return Ok(refuse(req_id, NAME, c)),
    };
    let service = match admitted(chat, rows, req.target.clone()).await {
        Ok(s) => s,
        Err(c) => return Ok(refuse(req_id, NAME, c)),
    };
    if !may_post(req.kind, &req.name, &req.target.row, rows, window) {
        return Ok(refuse(req_id, NAME, ChatCode::BadRequest));
    }
    let api = match service.api() {
        Ok(a) => a,
        Err(_) => return Ok(refuse(req_id, NAME, ChatCode::Unavailable)),
    };
    let Some(ws) = rows.resolve(Some(&req.target.row)) else {
        return Ok(refuse(req_id, NAME, ChatCode::StaleRow));
    };
    // The permit before the read: no file is read into memory without one, so `MAX_TRANSFERS` files at most are held.
    let transfer = match api.transfer() {
        Ok(t) => t,
        Err(e) => return Ok(refuse(req_id, NAME, code_of(&e))),
    };
    let (node_id, secrets) = (req.node_id.clone(), config::config_dir(&chat.env));
    let read = tokio::task::spawn_blocking(move || read_file(&ws, &node_id, secrets.as_deref())).await;
    let (file_name, bytes) = match read.unwrap_or(Err(ChatCode::Failed)) {
        Ok(file) => file,
        Err(c) => return Ok(refuse(req_id, NAME, c)),
    };
    let refused_post = |code| reply(req_id, NAME, &ChatPostFileRes::Refused { code });
    let (channel, thread) = (req.target.channel.as_str(), req.thread.as_ref().map(|t| t.as_str()));
    // The one call: nothing below sends again, whatever it answers.
    let outcome = api.post_file(transfer, channel, thread, post_kind(req.kind), &req.name, &file_name, bytes, req.comment.as_deref()).await;
    match outcome {
        Err(e) => refused_post(code_of(&e)),
        Ok(FileOutcome::Posted { file, ts }) => reply(req_id, NAME, &ChatPostFileRes::Posted { file, ts }),
        Ok(FileOutcome::Refused(why)) => refused_post(post_refusal_code(&why)),
        Ok(FileOutcome::Limited { retry_after, assumed }) => {
            reply(req_id, NAME, &ChatPostFileRes::Limited { retry_after_secs: retry_after.as_secs(), assumed })
        }
        Ok(FileOutcome::Unknown) => reply(req_id, NAME, &ChatPostFileRes::Unknown),
    }
}

/// The file `node_id` names beneath the row's root, read whole once, so the bytes shared are the bytes read: its base
/// name (the node's own, a link's name and not its target's) and 1 to `MAX_CHAT_FILE` bytes. The id is composed by the
/// read resolver, which refuses `..` and absolute parts and follows links (a `data/results` link to another disk
/// posts), and opened as a regular file only (`open_regular`: a FIFO, a device or a folder is refused without a wait).
/// The opened file is then judged, so no spelling and no swap between a check and the open reaches the secrets: on Unix
/// the file is refused when its (device, inode) is the configuration folder's or any of its entries' (a hard link
/// included); on Windows, where std has no stable file id, when its canonical path lies in that folder. Only the size
/// the open file states is read: 0 (an empty file, or a kernel file such as `/proc/self/environ`) is `not_a_file`, more
/// than the bound `too_large`, and a file that gives fewer bytes is `not_a_file`. Runs on the blocking pool, under the
/// caller's transfer permit.
fn read_file(ws: &Workspace, node_id: &str, secrets: Option<&Path>) -> Result<(String, Vec<u8>), ChatCode> {
    let files = ws.files_mode().map_err(|_| ChatCode::NotAFile)?;
    let path = files.node_id_to_path(node_id).map_err(|_| ChatCode::NotAFile)?;
    let name = path.file_name().and_then(|n| n.to_str()).ok_or(ChatCode::NotAFile)?.to_string();
    #[cfg(test)]
    run_before_open(&path);
    let file = sot_platform::host::open_regular(&path).map_err(|_| ChatCode::NotAFile)?;
    let meta = file.metadata().map_err(|_| ChatCode::NotAFile)?;
    if is_configuration(&path, &meta, secrets)? {
        return Err(ChatCode::NotAFile);
    }
    let len = match meta.len() {
        0 => return Err(ChatCode::NotAFile),
        n if n > MAX_CHAT_FILE as u64 => return Err(ChatCode::TooLarge),
        n => n,
    };
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(len).read_to_end(&mut bytes).map_err(|_| ChatCode::NotAFile)?;
    if bytes.len() as u64 != len {
        return Err(ChatCode::NotAFile);
    }
    Ok((name, bytes))
}

/// Whether the opened file (`meta`, its handle's) is the configuration folder or one of its entries, by (device,
/// inode). A folder that is missing protects nothing; one that cannot be listed refuses every file.
#[cfg(unix)]
fn is_configuration(_path: &Path, meta: &std::fs::Metadata, dir: Option<&Path>) -> Result<bool, ChatCode> {
    use std::os::unix::fs::MetadataExt;
    let Some(dir) = dir else { return Ok(false) };
    let same = |m: &std::fs::Metadata| (m.dev(), m.ino()) == (meta.dev(), meta.ino());
    match std::fs::metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(ChatCode::NotAFile),
        Ok(m) if same(&m) => return Ok(true),
        Ok(_) => {}
    }
    for entry in std::fs::read_dir(dir).map_err(|_| ChatCode::NotAFile)? {
        let entry = entry.map_err(|_| ChatCode::NotAFile)?;
        if std::fs::metadata(entry.path()).is_ok_and(|m| same(&m)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Windows: the canonical path of the opened name in the configuration folder (canonical). Limit: a name, not the
/// file, so a hard link or a swap of a folder between this and the open reaches the folder's files.
#[cfg(not(unix))]
fn is_configuration(path: &Path, _meta: &std::fs::Metadata, dir: Option<&Path>) -> Result<bool, ChatCode> {
    let real = super::canonical(path).ok_or(ChatCode::NotAFile)?;
    Ok(dir.and_then(super::canonical).is_some_and(|dir| crate::files::confine::path_within_root(&real, &dir)))
}

/// A test's actions to run just before the file at a path is opened, each once.
#[cfg(test)]
pub(crate) static BEFORE_OPEN: std::sync::Mutex<Vec<(std::path::PathBuf, Box<dyn FnOnce() + Send>)>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn run_before_open(path: &Path) {
    let then = {
        let mut held = BEFORE_OPEN.lock().unwrap();
        held.iter().position(|(p, _)| p == path).map(|i| held.remove(i).1)
    };
    if let Some(then) = then {
        then();
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
