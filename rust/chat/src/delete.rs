//! A session's delete of a post of the app's own, and what it reads before it deletes anything (chat guarantee 36): the
//! one read that finds the message (`Api::find_post`), the rule that says whether the app posted it
//! (`Api::is_apps_message`), the files it names (`own_post_files`), and each file's verdict from one `files.info`
//! (`file_verdict`). All of it is read-only and decided from what Slack answered; nothing here names a file a caller
//! gave.

use std::collections::BTreeSet;

use serde_json::Value;
use sot_protocol::chat::Ts;

use super::{page_parts, ApiError, Api, Bounds, ScanQuery};
use crate::http::MAX_SCAN_BODY;
use crate::normalize::{is_own, ts_key, valid_id, Context, MAX_FILES};

/// The messages one read of a post asks for: the page inside the one-second window around its time.
pub const POST_READ_LIMIT: u16 = 100;

/// What the read of a post found.
#[derive(Clone, Debug, PartialEq)]
pub enum Listed {
    /// The message as Slack listed it.
    Message(Value),
    /// No such message is listed: a complete page that lacks it, a thread Slack does not hold, or the placeholder Slack
    /// keeps for a deleted message whose thread has replies.
    Gone,
    /// The page that should hold the message is one of several: nothing is known, and nothing is deleted.
    Incomplete,
}

/// Why the delete stops before it sends anything but the read and the file reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The message is not one this app posted.
    NotOwnMessage,
    /// A file of the post could not be read as the app's own and shared in this post alone.
    UnclearFile,
}

/// Why a file is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// A person's or another app's file.
    NotTheApps,
    /// Also shared somewhere else in Slack.
    SharedElsewhere,
}

/// What `files.info` says to do with one file of the post.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// The app's file, shared in this post alone: delete it.
    Delete,
    /// Slack says it is already deleted.
    AlreadyDeleted,
    /// Leave it, for this reason.
    Kept(Kept),
    /// The answer cannot be matched to this post: nothing is deleted.
    Unclear,
    /// The read itself failed (a limit, a refusal, a closed client): the delete ends with it.
    Failed(ApiError),
}

/// The end state of a post's files: what is deleted and what is kept with why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PostFiles {
    pub deleted: Vec<String>,
    pub kept: Vec<(String, Kept)>,
}

impl Api {
    /// The message `ts` of `channel`, or of the thread `thread` of it, from one read of Slack's history (or replies) for
    /// the second either side of `ts`, both bounds exclusive, no cursor. The message is the entry whose `ts` is `ts`
    /// exactly; the margin means Slack's treatment of a bound is never relied on. A reply is in its thread, not in the
    /// channel's history, so a reply asked for without its thread is `Gone`.
    pub async fn find_post(&self, channel: &str, thread: Option<&str>, ts: &str) -> Result<Listed, ApiError> {
        let at = Ts::parse(ts).map_err(|_| ApiError::BadBound)?;
        let key = ts_key(&at);
        let micros = key.0 * 1_000_000 + u128::from(key.1);
        let bound = |m: u128| format!("{}.{:06}", m / 1_000_000, m % 1_000_000);
        let bounds = Bounds::new(Some(&bound(micros.saturating_sub(1_000_000))), Some(&bound(micros + 1_000_000)), None)?;
        let query = ScanQuery::new(channel, thread, bounds, POST_READ_LIMIT)?;
        let (method, params) = query.0.request(None);
        let v = match self.read(method, &params, MAX_SCAN_BODY).await {
            Err(ApiError::Slack(code)) if thread.is_some() && code == "thread_not_found" => return Ok(Listed::Gone),
            read => read?,
        };
        let (messages, has_more, _) = page_parts(&v, query.0.capacity())?;
        let listed = messages.iter().find(|m| m.get("ts").and_then(Value::as_str).and_then(|t| Ts::parse(t).ok()).is_some_and(|t| ts_key(&t) == key));
        Ok(match listed {
            Some(m) if m.get("subtype").and_then(Value::as_str) == Some("tombstone") => Listed::Gone,
            Some(m) => Listed::Message(m.clone()),
            None if has_more => Listed::Incomplete,
            None => Listed::Gone,
        })
    }

    /// Whether the app posted this message: normalize's one rule for an own post, by the identity `auth.test` gave.
    pub fn is_apps_message(&self, message: &Value) -> Result<bool, ApiError> {
        let own = self.identity()?;
        let ctx = Context { workspace: &self.workspace, own_bot_id: own.bot_id.as_deref(), own_user_id: Some(&own.user_id), limited: false };
        let text = |key: &str| message.get(key).and_then(Value::as_str);
        Ok(is_own(&ctx, text("user"), text("bot_id")))
    }
}

/// The ids of the files a message names, in its order, each once. No `files`, or a null one, is no file. Anything else
/// that is not an array of at most `MAX_FILES` objects each with a valid `id` is `UnclearFile`: a delete that cannot say
/// what the post holds deletes nothing.
pub fn own_post_files(message: &Value) -> Result<Vec<String>, Refusal> {
    let entries = match message.get("files") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(entries)) if entries.len() <= MAX_FILES => entries,
        Some(_) => return Err(Refusal::UnclearFile),
    };
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    for entry in entries {
        let id = entry.get("id").and_then(Value::as_str).filter(|id| valid_id(id)).ok_or(Refusal::UnclearFile)?;
        if seen.insert(id) {
            ids.push(id.to_string());
        }
    }
    Ok(ids)
}

/// What to do with `file` of the post at `ts` in `channel`, from the answer to its `files.info`. `own_user` is the bot's
/// user id.
///
/// - `file_deleted` is deleted already. `file_not_found` is never that (Slack says it for a made-up id and, possibly,
///   for a file the app cannot see), so it is unclear. Any other failed read ends the delete.
/// - A file whose `user` is not the bot's is kept. Of the bot's files there are three cases: this message is among its
///   shares (listed twice is still this message) and nothing else is listed, in a share or in `channels`, `groups` or `ims`:
///   delete; this message is among them and something else is too, another channel, a direct message or this channel at
///   another time: kept as shared elsewhere; this message is not among them (a file no share lists, a share in this channel
///   at another time only, a share kind other than `public` or `private`, any other shape): unclear, since keeping such a
///   file would orphan it once the message is gone.
pub fn file_verdict(own_user: &str, answer: &Result<Value, ApiError>, file: &str, channel: &str, ts: &str) -> Verdict {
    let v = match answer {
        Err(ApiError::Slack(code)) if code == "file_deleted" => return Verdict::AlreadyDeleted,
        Err(ApiError::Slack(code)) if code == "file_not_found" => return Verdict::Unclear,
        Err(e) => return Verdict::Failed(e.clone()),
        Ok(v) => v,
    };
    let Some(info) = v.get("file").filter(|f| f.get("id").and_then(Value::as_str) == Some(file)) else {
        return Verdict::Unclear;
    };
    match info.get("user").and_then(Value::as_str) {
        None => return Verdict::Unclear,
        Some(user) if user != own_user => return Verdict::Kept(Kept::NotTheApps),
        Some(_) => {}
    }
    let (Some(entries), Some(listed)) = (share_entries(info), listed_ids(info)) else {
        return Verdict::Unclear;
    };
    let Ok(at) = Ts::parse(ts).map(|t| ts_key(&t)) else {
        return Verdict::Unclear;
    };
    // This message among the file's shares (listed twice is still this message), and anything else among them or listed.
    let here = |(c, t): &(String, (u128, u32))| c == channel && *t == at;
    let this_message = entries.iter().any(here);
    let something_else = entries.iter().any(|e| !here(e)) || listed.iter().any(|c| c != channel);
    match (this_message, something_else) {
        (true, false) => Verdict::Delete,
        (true, true) => Verdict::Kept(Kept::SharedElsewhere),
        (false, _) => Verdict::Unclear,
    }
}

/// Every share entry of a file as (channel, time), from `shares.public` and `shares.private`; `None` when any part has
/// another type than Slack's.
fn share_entries(file: &Value) -> Option<Vec<(String, (u128, u32))>> {
    let shares = match file.get("shares") {
        None | Some(Value::Null) => return Some(Vec::new()),
        Some(Value::Object(shares)) => shares,
        Some(_) => return None,
    };
    // A kind that is neither is a shape this rule has not seen: not read as shared nowhere.
    if shares.keys().any(|k| k != "public" && k != "private") {
        return None;
    }
    let mut out = Vec::new();
    for kind in ["public", "private"] {
        let by_channel = match shares.get(kind) {
            None | Some(Value::Null) => continue,
            Some(Value::Object(by_channel)) => by_channel,
            Some(_) => return None,
        };
        for (channel, list) in by_channel {
            for entry in list.as_array()? {
                let ts = entry.get("ts")?.as_str().and_then(|t| Ts::parse(t).ok())?;
                out.push((channel.clone(), ts_key(&ts)));
            }
        }
    }
    Some(out)
}

/// The ids in `channels`, `groups` and `ims`; a missing key is empty, and `None` when any is not an array of strings.
fn listed_ids(file: &Value) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for key in ["channels", "groups", "ims"] {
        match file.get(key) {
            None | Some(Value::Null) => {}
            Some(Value::Array(ids)) => {
                for id in ids {
                    out.push(id.as_str()?.to_string());
                }
            }
            Some(_) => return None,
        }
    }
    Some(out)
}
