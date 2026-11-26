//! A session's delete of a post of the app's own, and what it reads before it deletes anything (chat guarantee 36): the
//! one read that finds the message (`Api::find_post`), the rule that says whether the app posted it
//! (`Api::is_apps_message`), the files it names (`own_post_files`), and each file's verdict from one `files.info`
//! (`file_verdict`). All of it is read-only and decided from what Slack answered; nothing here names a file a caller
//! gave.

use std::collections::BTreeSet;
use std::future::Future;
use std::time::Duration;

use serde_json::{json, Value};
use sot_protocol::chat::Ts;
use sot_protocol::ops::ChatCode;
use tokio::time::Instant;

use super::{page_parts, Ack, Api, ApiError, Bounds, DeleteOutcome, ScanQuery};
use crate::http::{MAX_BODY, MAX_SCAN_BODY};
use crate::normalize::{is_own, ts_key, valid_id, Context, MAX_FILES};

/// The messages one read of a post asks for: the page inside the one-second window around its time.
pub const POST_READ_LIMIT: u16 = 100;
/// A delete of a post, all of it, counted from its start: a call begins only when its own deadline fits in what is left.
/// A post with one file needs at most a read, a `files.info`, a `files.delete` and a `chat.delete`, 15 + 15 + 30 + 30 s.
pub const DELETE_POST_DEADLINE: Duration = Duration::from_secs(120);

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

/// Why a delete stopped with the post, or part of it, left standing.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// The message is not one this app posted, or Slack would not let the app delete it.
    NotOwnMessage,
    /// A file of the post could not be read as the app's own and shared in this post alone; nothing was deleted.
    UnclearFile,
    /// The page that should hold the message had more behind it, so nothing is known.
    Incomplete,
    /// The next call's own deadline did not fit in what was left of the delete's.
    Deadline,
    /// The fresh admission before a call refused.
    Recheck(ChatCode),
    /// A call that never left, or a read Slack or the client refused, as the caller's code for it.
    Call(ApiError),
    /// Slack refused a mutation with this code (or the call never left: `busy`, `invalid`, `destination`).
    Slack(String),
}

/// How a delete of a post ended.
#[derive(Clone, Debug, PartialEq)]
pub enum PostEnd {
    /// The message is gone, deleted by this ask.
    Deleted,
    /// The message is not listed: deleted already, or never posted (and, asked with no thread, a reply is not looked for).
    Gone,
    /// The message stands, and nothing is in doubt.
    Refused(Refusal),
    /// Slack asked to wait; nothing is resent. `assumed`: the wait is a guess.
    Limited { retry_after: Duration, assumed: bool },
    /// An answer was lost: a file or the message may be deleted. The same ask again finishes it. The code says why.
    Unknown(String),
}

/// A delete of a post: how it ended and what that left of its files, with where it ended for the log.
#[derive(Clone, Debug, PartialEq)]
pub struct PostDelete {
    pub end: PostEnd,
    /// The post's files that are deleted now, whichever ask deleted them, and the files kept; a file the delete had not
    /// reached is in neither.
    pub files: PostFiles,
    /// The Slack method that ended it, or `recheck` or `deadline` for a stop before a call; empty before any.
    pub step: &'static str,
    /// The file the step was about, or empty.
    pub file: String,
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

/// What a delete has done so far, and the budget its calls are held to.
struct Run {
    start: Instant,
    budget: Duration,
    calls: u32,
    step: &'static str,
    file: String,
    order: Vec<String>,
    files: PostFiles,
}

impl Run {
    /// Before a call: its own deadline must fit in what is left, and every call but the first is preceded by a fresh
    /// admission. A stop names itself as the step.
    async fn gate<F, Fut>(&mut self, need: Duration, step: &'static str, file: &str, recheck: &mut F) -> Result<(), PostEnd>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<(), ChatCode>>,
    {
        self.step = step;
        self.file = file.to_string();
        // Before the admission, so a delete that is out of time asks nothing, and after it, so a slow admission cannot let
        // a call begin past the deadline.
        let late = |run: &Run| run.start.elapsed() + need > run.budget;
        if late(self) {
            self.step = "deadline";
            return Err(PostEnd::Refused(Refusal::Deadline));
        }
        if self.calls > 0 {
            if let Err(code) = recheck().await {
                self.step = "recheck";
                return Err(PostEnd::Refused(Refusal::Recheck(code)));
            }
            if late(self) {
                self.step = "deadline";
                return Err(PostEnd::Refused(Refusal::Deadline));
            }
        }
        self.calls += 1;
        Ok(())
    }

    fn finish(mut self, end: PostEnd) -> PostDelete {
        let at = |id: &String| self.order.iter().position(|o| o == id);
        let mut deleted = std::mem::take(&mut self.files.deleted);
        deleted.sort_by_key(at);
        self.files.deleted = deleted;
        PostDelete { end, files: self.files, step: self.step, file: self.file }
    }
}

/// A read that failed ends the delete: a limit as itself, anything else as the caller's code for the error.
fn read_failed(e: ApiError) -> PostEnd {
    match e {
        ApiError::Limited { retry_after, assumed } => PostEnd::Limited { retry_after, assumed },
        e => PostEnd::Refused(Refusal::Call(e)),
    }
}

impl Api {
    /// Delete the message `ts` of `channel` (a reply with its `thread`) and every file the app itself posted with it,
    /// the files first, so that the message, which alone names them, goes last. The message is read once, before any
    /// other call, and its files come from that read alone; every file is judged by one `files.info` before any is
    /// deleted, and an unclear one stops the delete with nothing deleted (`file_verdict`). Then one `files.delete` for
    /// each file judged delete, and one `chat.delete` once every one of them is gone. Every mutation is sent once; the
    /// first answer that is not an acknowledgement ends the delete, and nothing more is sent. Each stop leaves a state
    /// the same ask, made again, finishes: a deleted file reads `file_deleted` and counts as deleted, and a message
    /// whose files are all gone is deleted. `recheck` is called before every call but the first and a refusal ends the
    /// delete before that call; one deadline, `DELETE_POST_DEADLINE`, holds the whole.
    pub async fn delete_post<F, Fut>(&self, channel: &str, thread: Option<&str>, ts: &str, mut recheck: F) -> PostDelete
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<(), ChatCode>>,
    {
        let mut run = Run { start: Instant::now(), budget: self.deadlines.delete, calls: 0, step: "", file: String::new(), order: Vec::new(), files: PostFiles::default() };
        let end = self.delete_post_run(channel, thread, ts, &mut recheck, &mut run).await;
        run.finish(end)
    }

    async fn delete_post_run<F, Fut>(&self, channel: &str, thread: Option<&str>, ts: &str, recheck: &mut F, run: &mut Run) -> PostEnd
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<(), ChatCode>>,
    {
        let (read, post) = (self.deadlines.read, self.deadlines.post);
        let method = if thread.is_some() { "conversations.replies" } else { "conversations.history" };
        if let Err(end) = run.gate(read, method, "", recheck).await {
            return end;
        }
        let message = match self.find_post(channel, thread, ts).await {
            Ok(Listed::Message(message)) => message,
            Ok(Listed::Gone) => return PostEnd::Gone,
            Ok(Listed::Incomplete) => return PostEnd::Refused(Refusal::Incomplete),
            Err(e) => return read_failed(e),
        };
        match self.is_apps_message(&message) {
            Ok(true) => {}
            Ok(false) => return PostEnd::Refused(Refusal::NotOwnMessage),
            Err(e) => return PostEnd::Refused(Refusal::Call(e)),
        }
        let ids = match own_post_files(&message) {
            Ok(ids) => ids,
            Err(refusal) => return PostEnd::Refused(refusal),
        };
        run.order = ids.clone();
        let own = match self.identity() {
            Ok(own) => own.user_id,
            Err(e) => return PostEnd::Refused(Refusal::Call(e)),
        };
        // Every file is judged before any is deleted.
        let mut doomed = Vec::new();
        for id in &ids {
            if let Err(end) = run.gate(read, "files.info", id, recheck).await {
                return end;
            }
            let answer = self.read("files.info", &[("file", id.clone())], MAX_BODY).await;
            match file_verdict(&own, &answer, id, channel, ts) {
                Verdict::Delete => doomed.push(id),
                Verdict::AlreadyDeleted => run.files.deleted.push(id.clone()),
                Verdict::Kept(why) => run.files.kept.push((id.clone(), why)),
                Verdict::Unclear => return PostEnd::Refused(Refusal::UnclearFile),
                Verdict::Failed(e) => return read_failed(e),
            }
        }
        for id in doomed {
            if let Err(end) = run.gate(post, "files.delete", id, recheck).await {
                return end;
            }
            match self.mutate("files.delete", &json!({"file": id})).await {
                Err(e) => return PostEnd::Refused(Refusal::Call(e)),
                Ok(Ack::Done(_)) => run.files.deleted.push(id.clone()),
                Ok(Ack::Refused(code)) if code == "file_deleted" => run.files.deleted.push(id.clone()),
                Ok(Ack::Refused(code)) => return PostEnd::Refused(Refusal::Slack(code)),
                Ok(Ack::Limited { retry_after, assumed }) => return PostEnd::Limited { retry_after, assumed },
                Ok(Ack::Unknown(why)) => return PostEnd::Unknown(why),
            }
        }
        if let Err(end) = run.gate(post, "chat.delete", "", recheck).await {
            return end;
        }
        match self.delete_message(channel, ts).await {
            Err(e) => PostEnd::Refused(Refusal::Call(e)),
            Ok(DeleteOutcome::Deleted) => PostEnd::Deleted,
            Ok(DeleteOutcome::Gone) => PostEnd::Gone,
            Ok(DeleteOutcome::Refused(code)) if code == "cant_delete_message" => PostEnd::Refused(Refusal::NotOwnMessage),
            Ok(DeleteOutcome::Refused(code)) => PostEnd::Refused(Refusal::Slack(code)),
            Ok(DeleteOutcome::Limited { retry_after, assumed }) => PostEnd::Limited { retry_after, assumed },
            Ok(DeleteOutcome::Unknown(why)) => PostEnd::Unknown(why),
        }
    }
}
