//! A channel set's messages and files for the fake Slack, answering the five methods a delete of a post uses
//! (`conversations.history`, `conversations.replies`, `files.info`, `files.delete`, `chat.delete`) from one state, so that
//! each call changes what the others then say, as Slack was measured to do on 2026-11-26 (a throwaway channel, a bot
//! token, four shares made by `files.completeUploadExternal`, every call HTTP 200):
//! - a share's message has no `subtype`, `upload: true` and one `files` entry (`listed_share`);
//! - `files.info` of a shared file lists the channel in `channels` and one entry in `shares.public`, with the keys
//!   `channel_name`, `is_silent_share`, `reply_count`, `reply_users`, `reply_users_count`, `share_user_id`, `source`,
//!   `team_id` and `ts` (a share made in a thread also `latest_reply` and `thread_ts`, and its `ts` is the reply's own),
//!   and `ims` empty; `user` is the bot's; an image is the same as a text file apart from its `mode`;
//! - `chat.delete` of the message alone removes it (no tombstone) and leaves the file alive and unshared: `files.info`
//!   still answers `ok`, with `shares: {}` and no channels;
//! - `files.delete` of the file alone leaves the message listed, text unchanged, its `files` entry reduced to
//!   `{id, mode: "tombstone"}`; a later `chat.delete` of that message answers `ok`;
//! - repeats: `files.delete` of a deleted file answers `file_deleted`, of a made-up id `file_not_found`; `chat.delete`
//!   of a message already gone answers `message_not_found`.
//!
//! `files.delete` and `chat.delete` took a JSON body `{"file": id}` and `{"channel", "ts"}`, and a history read a second
//! either side of a share's time listed it. What it infers, not measured: a private channel's share has the shape above
//! under `shares.private` and `groups` (the rule fails closed on any other); a message deleted while it has replies is
//! removed like any other (`tombstone` plays the placeholder Slack is said to keep, in a shape no one measured);
//! `cant_delete_file` for a file the bot does not own and `cant_delete_message` for a message without the bot's `bot_id`
//! are Slack's documented codes. A request that is none of the five methods is not answered here (`reply` is `None`).
//!
//! A test makes one call fail once with `fault` (the answer, the change, or both missing) and changes the world while a
//! call is in flight with `before`.

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use super::{listed_share, History, Reply, Seen};

/// What goes wrong with the next call of one method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The connection closes with no answer and no change.
    Drop,
    /// Slack makes the change, then the connection closes with no answer.
    DropAfter,
    /// HTTP 429, with this `Retry-After` when there is one; no change.
    Limit(Option<u64>),
    /// `ok: false` with this code; no change.
    Error(&'static str),
    /// The request is read and never answered; no change.
    Hang,
    /// HTTP 200 with a body that is no JSON; no change.
    Garbled,
}

const METHODS: [&str; 5] = [
    "conversations.history",
    "conversations.replies",
    "files.info",
    "files.delete",
    "chat.delete",
];

struct Held {
    channel: String,
    message: Value,
}

struct Share {
    channel: String,
    ts: String,
    thread: Option<String>,
}

struct HeldFile {
    id: String,
    user: String,
    mimetype: String,
    shares: Vec<Share>,
    ims: Vec<String>,
    deleted: bool,
}

type Hook = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct State {
    messages: Vec<Held>,
    files: Vec<HeldFile>,
    faults: Vec<(String, Fault)>,
    hooks: Vec<(String, Hook)>,
}

pub struct Posts {
    bot_user: String,
    bot_id: String,
    state: Mutex<State>,
}

impl Posts {
    /// A world with no messages, whose bot is `bot_user` with the bot id `bot_id`.
    pub fn new(bot_user: &str, bot_id: &str) -> Posts {
        Posts { bot_user: bot_user.into(), bot_id: bot_id.into(), state: Mutex::new(State::default()) }
    }

    fn with(self, change: impl FnOnce(&mut State)) -> Posts {
        change(&mut self.state.lock().unwrap());
        self
    }

    /// The app's text post.
    pub fn bot_post(self, channel: &str, ts: &str, thread: Option<&str>) -> Posts {
        let mut message = json!({
            "type": "message", "ts": ts, "user": self.bot_user, "bot_id": self.bot_id, "app_id": "A0APP", "text": format!("text {ts}"),
        });
        if let Some(thread) = thread {
            message["thread_ts"] = json!(thread);
        }
        self.with(|s| s.messages.push(Held { channel: channel.into(), message }))
    }

    /// The app's share of one file, the file the app's own (`F…` named `<id>.png`).
    pub fn share(self, channel: &str, ts: &str, thread: Option<&str>, file: &str) -> Posts {
        self.share_many(channel, ts, thread, &[file])
    }

    /// The app's share of these files in one message.
    pub fn share_many(self, channel: &str, ts: &str, thread: Option<&str>, files: &[&str]) -> Posts {
        let entry = |id: &str| json!({"id": id, "name": format!("{id}.png"), "mimetype": "image/png"});
        let mut message = listed_share(ts, thread, &self.bot_id, &self.bot_user, &format!("text {ts}"), &entry(files[0]));
        for id in &files[1..] {
            let mut more = entry(id);
            more["title"] = more["name"].clone();
            more["mode"] = json!("hosted");
            more["file_access"] = json!("visible");
            message["files"].as_array_mut().unwrap().push(more);
        }
        let user = self.bot_user.clone();
        self.with(|s| {
            s.messages.push(Held { channel: channel.into(), message });
            for id in files {
                s.files.push(HeldFile::new(id, &user, channel, ts, thread));
            }
        })
    }

    /// A channel member's message, with the member's own file when `file` is given.
    pub fn member_post(self, channel: &str, ts: &str, thread: Option<&str>, file: Option<&str>) -> Posts {
        let mut message = json!({"type": "message", "ts": ts, "user": "U0MEM", "text": format!("text {ts}")});
        if let Some(thread) = thread {
            message["thread_ts"] = json!(thread);
        }
        if let Some(id) = file {
            message["files"] = json!([{"id": id, "name": format!("{id}.png"), "mimetype": "image/png", "mode": "hosted"}]);
        }
        self.with(|s| {
            s.messages.push(Held { channel: channel.into(), message });
            if let Some(id) = file {
                s.files.push(HeldFile::new(id, "U0MEM", channel, ts, thread));
            }
        })
    }

    /// The file is also shared in `channel` at `ts`, by someone else's message.
    pub fn also_shared(self, file: &str, channel: &str, ts: &str) -> Posts {
        self.with(|s| s.file_mut(file).shares.push(Share { channel: channel.into(), ts: ts.into(), thread: None }))
    }

    /// The file is also in a direct message `im`.
    pub fn in_im(self, file: &str, im: &str) -> Posts {
        self.with(|s| s.file_mut(file).ims.push(im.into()))
    }

    /// Slack no longer lists any share of the file, though a message still names it.
    pub fn unshared(self, file: &str) -> Posts {
        self.with(|s| {
            let held = s.file_mut(file);
            held.shares.clear();
            held.ims.clear();
        })
    }

    /// The placeholder Slack keeps for a deleted message whose thread has replies.
    pub fn tombstone(self, channel: &str, ts: &str) -> Posts {
        let message = json!({
            "type": "message", "subtype": "tombstone", "ts": ts, "thread_ts": ts, "user": "USLACKBOT",
            "text": "This message was deleted.", "hidden": true, "reply_count": 1,
        });
        self.with(|s| s.messages.push(Held { channel: channel.into(), message }))
    }

    /// The next call of `method` goes wrong this way, once; several faults of one method fire in the order given.
    pub fn fault(&self, method: &str, fault: Fault) {
        self.state.lock().unwrap().faults.push((method.into(), fault));
    }

    /// Every call of `method` runs `change` first, before it is answered: the world moves while the call is in flight.
    pub fn before(&self, method: &str, change: impl Fn() + Send + Sync + 'static) {
        self.state.lock().unwrap().hooks.push((method.into(), Arc::new(change)));
    }

    /// Whether the channel lists a message at `ts` (a tombstone included).
    pub fn holds_message(&self, channel: &str, ts: &str) -> bool {
        self.state.lock().unwrap().messages.iter().any(|m| m.channel == channel && m.message["ts"] == ts)
    }

    /// Whether the file exists and is not deleted.
    pub fn holds_file(&self, id: &str) -> bool {
        self.state.lock().unwrap().files.iter().any(|f| f.id == id && !f.deleted)
    }

    /// The answer to one of the five methods, or `None` for any other request.
    pub fn reply(&self, seen: &Seen) -> Option<Reply> {
        let path = seen.path();
        let method = path.strip_prefix("/api/").filter(|m| METHODS.contains(m))?;
        let hooks: Vec<Hook> = {
            let state = self.state.lock().unwrap();
            state.hooks.iter().filter(|(m, _)| m == method).map(|(_, h)| h.clone()).collect()
        };
        for hook in hooks {
            hook();
        }
        let fault = {
            let mut state = self.state.lock().unwrap();
            let at = state.faults.iter().position(|(m, _)| m == method);
            at.map(|i| state.faults.remove(i).1)
        };
        Some(match fault {
            None => self.perform(method, seen),
            Some(Fault::Drop) => Reply::Drop,
            Some(Fault::DropAfter) => {
                self.perform(method, seen);
                Reply::Drop
            }
            Some(Fault::Limit(wait)) => Reply::Status {
                code: 429,
                headers: wait.iter().map(|w| ("Retry-After".to_string(), w.to_string())).collect(),
                body: b"{}".to_vec(),
            },
            Some(Fault::Error(code)) => failed(code),
            Some(Fault::Hang) => Reply::Hang,
            Some(Fault::Garbled) => Reply::ok(b"this is not json"),
        })
    }

    fn perform(&self, method: &str, seen: &Seen) -> Reply {
        let mut state = self.state.lock().unwrap();
        match method {
            "conversations.history" | "conversations.replies" => {
                let Some(channel) = seen.param("channel") else { return failed("channel_not_found") };
                let history = state
                    .messages
                    .iter()
                    .filter(|m| m.channel == channel)
                    .fold(History::new(), |h, m| h.raw(m.message.clone()));
                history.answer(seen)
            }
            "files.info" => state.info(file_asked(seen, "file")),
            "files.delete" => state.delete_file(&self.bot_user, file_asked(seen, "file")),
            _ => {
                let body = seen.json().unwrap_or_default();
                let (channel, ts) = (body["channel"].as_str().unwrap_or(""), body["ts"].as_str().unwrap_or(""));
                state.delete_message(&self.bot_id, channel, ts)
            }
        }
    }

}

/// The file a request names: in its query (a read) or its JSON body (a mutation).
fn file_asked(seen: &Seen, key: &str) -> String {
    seen.param(key).or_else(|| seen.json().and_then(|b| b[key].as_str().map(String::from))).unwrap_or_default()
}

fn failed(code: &str) -> Reply {
    Reply::json(&json!({"ok": false, "error": code}))
}

impl HeldFile {
    fn new(id: &str, user: &str, channel: &str, ts: &str, thread: Option<&str>) -> HeldFile {
        HeldFile {
            id: id.into(),
            user: user.into(),
            mimetype: "image/png".into(),
            shares: vec![Share { channel: channel.into(), ts: ts.into(), thread: thread.map(String::from) }],
            ims: Vec::new(),
            deleted: false,
        }
    }
}

impl State {
    fn file_mut(&mut self, id: &str) -> &mut HeldFile {
        self.files.iter_mut().find(|f| f.id == id).expect("a file the test made")
    }

    /// `files.info`: a live file in the measured shape, a deleted one `file_deleted`, any other `file_not_found`.
    fn info(&self, id: String) -> Reply {
        let Some(file) = self.files.iter().find(|f| f.id == id) else { return failed("file_not_found") };
        if file.deleted {
            return failed("file_deleted");
        }
        let mut shares = Map::new();
        let (mut channels, mut groups) = (Vec::new(), Vec::new());
        for share in &file.shares {
            let (kind, ids) = if share.channel.starts_with('G') { ("private", &mut groups) } else { ("public", &mut channels) };
            let mut entry = json!({
                "channel_name": "chan", "is_silent_share": false, "reply_count": 0, "reply_users": [], "reply_users_count": 0,
                "share_user_id": file.user, "source": "UPLOAD", "team_id": "T0AAAA", "ts": share.ts,
            });
            if let Some(thread) = &share.thread {
                entry["thread_ts"] = json!(thread);
                entry["latest_reply"] = json!(share.ts);
            }
            let listed = shares.entry(kind.to_string()).or_insert_with(|| json!({})).as_object_mut().unwrap();
            listed.entry(share.channel.clone()).or_insert_with(|| json!([])).as_array_mut().unwrap().push(entry);
            if !ids.contains(&share.channel) {
                ids.push(share.channel.clone());
            }
        }
        Reply::json(&json!({"ok": true, "file": {
            "id": file.id, "user": file.user, "mimetype": file.mimetype,
            "channels": channels, "groups": groups, "ims": file.ims,
            "shares": shares, "is_public": !channels.is_empty(), "is_external": false,
        }}))
    }

    /// `files.delete`: the file goes, and every message entry naming it becomes a tombstone entry.
    fn delete_file(&mut self, bot_user: &str, id: String) -> Reply {
        let Some(file) = self.files.iter_mut().find(|f| f.id == id) else { return failed("file_not_found") };
        if file.deleted {
            return failed("file_deleted");
        }
        if file.user != bot_user {
            return failed("cant_delete_file");
        }
        file.deleted = true;
        for held in &mut self.messages {
            let Some(files) = held.message["files"].as_array_mut() else { continue };
            for entry in files.iter_mut().filter(|e| e["id"] == id.as_str()) {
                *entry = json!({"id": id, "mode": "tombstone"});
            }
        }
        Reply::json(&json!({"ok": true}))
    }

    /// `chat.delete`: the message goes with no tombstone, and each file it shared loses that share.
    fn delete_message(&mut self, bot_id: &str, channel: &str, ts: &str) -> Reply {
        let Some(at) = self.messages.iter().position(|m| m.channel == channel && m.message["ts"] == ts) else {
            return failed("message_not_found");
        };
        if self.messages[at].message["bot_id"] != bot_id {
            return failed("cant_delete_message");
        }
        self.messages.remove(at);
        for file in &mut self.files {
            file.shares.retain(|s| !(s.channel == channel && s.ts == ts));
        }
        Reply::json(&json!({"ok": true, "channel": channel, "ts": ts}))
    }
}
