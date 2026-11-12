//! A channel for the fake Slack that answers `conversations.history` and `conversations.replies` the way SlackClaw
//! measured them (its facts note, "Page order with `oldest` alone, measured" and "The walk back with no cursor,
//! measured"; "Posts arriving during reads and the walk (P-ORDER), measured" for the parent's `latest_reply`). Both bounds are exclusive; with both, page 1
//! holds the newest messages in range and a read with `latest` lowered to a page's oldest runs on older; the last page
//! that holds messages says `has_more: false`, full or not, and the read after it is empty; inside a page the messages
//! are newest first. With `oldest` alone, page 1 holds the OLDEST messages, so a walk that lowers `latest` from there sees
//! one page and then an empty one. A parent's `latest_reply` is whatever the test gave it, and is not held to the read's
//! `latest` while the list is. A cursor is refused (`invalid_cursor`): no pass sends one.
//!
//! Replies are played in the same forms. A thread's page holds the parent first, on every page on top of the limit and on
//! the last, otherwise empty, one (`has_more: false`), then the page's replies oldest first; with both bounds page 1
//! holds the newest replies, with `oldest` alone the oldest, and there the one form that carries a cursor drops one
//! reply at every page boundary and never returns it (the same section of the note). A reply is listed in the channel only when it is a broadcast.
//!
//! What it infers, not measured: a page of 999 with more behind it (no measured channel held more than 999) is the
//! newest 999 in range, and a page may hold fewer than asked with more behind it (`cap`, Slack's documented right).

use std::sync::Mutex;
use std::time::Instant;

use serde_json::{json, Value};

use super::{Reply, Seen};
use crate::normalize::ts_key;

#[derive(Default)]
pub struct History {
    messages: Vec<((u128, u32), Value)>,
    cap: Option<usize>,
    limited: bool,
    asked: Mutex<Vec<Instant>>,
}

impl History {
    pub fn new() -> History {
        History::default()
    }

    /// A message from a channel member.
    pub fn post(self, ts: &str) -> History {
        self.raw(json!({"ts": ts, "user": "U0MEM", "text": format!("text {ts}")}))
    }

    /// A parent whose newest reply is at `latest_reply`, wherever that lies.
    pub fn parent(self, ts: &str, latest_reply: &str) -> History {
        self.raw(json!({"ts": ts, "thread_ts": ts, "user": "U0MEM", "text": format!("text {ts}"), "reply_count": 1, "latest_reply": latest_reply}))
    }

    /// A reply to the thread of `parent`, listed in the thread only.
    pub fn reply(self, parent: &str, ts: &str) -> History {
        self.raw(json!({"ts": ts, "thread_ts": parent, "user": "U0MEM", "text": format!("text {ts}")}))
    }

    /// A reply that is also shown in the channel, so it comes back in history and in its thread.
    pub fn broadcast(self, parent: &str, ts: &str) -> History {
        self.raw(json!({"ts": ts, "thread_ts": parent, "subtype": "thread_broadcast", "user": "U0MEM", "text": format!("text {ts}")}))
    }

    /// Any raw message, as Slack would send it.
    pub fn raw(mut self, message: Value) -> History {
        let key = message["ts"].as_str().map(|t| ts_key(&sot_protocol::chat::Ts::parse(t).expect("a ts"))).expect("a ts");
        self.messages.push((key, message));
        self
    }

    /// Slack returns at most this many messages a page whatever the limit, with `has_more` while more are in range.
    pub fn cap(mut self, per_page: usize) -> History {
        self.cap = Some(per_page);
        self
    }

    /// Every page carries `is_limited: true`.
    pub fn limited(mut self) -> History {
        self.limited = true;
        self
    }

    /// When each read arrived, in order.
    pub fn asked(&self) -> Vec<Instant> {
        self.asked.lock().unwrap().clone()
    }

    pub fn answer(&self, seen: &Seen) -> Reply {
        self.asked.lock().unwrap().push(Instant::now());
        let bound = |name: &str| seen.param(name).map(|t| ts_key(&sot_protocol::chat::Ts::parse(&t).expect("a bound")));
        let (oldest, latest, cursor) = (bound("oldest"), bound("latest"), seen.param("cursor"));
        // The one cursor this plays is the replies cursor under `oldest` alone (the walk never sends it).
        let replies = seen.path().ends_with("/conversations.replies");
        if cursor.is_some() && !(replies && latest.is_none()) {
            return Reply::json(&json!({"ok": false, "error": "invalid_cursor"}));
        }
        let limit: usize = seen.param("limit").and_then(|l| l.parse().ok()).unwrap_or(100);
        let take = self.cap.map_or(limit, |c| c.min(limit));
        let parent = seen.param("ts").map(|t| ts_key(&sot_protocol::chat::Ts::parse(&t).expect("a parent")));
        let thread_of = |m: &Value| m["thread_ts"].as_str().map(|t| ts_key(&sot_protocol::chat::Ts::parse(t).expect("a ts")));
        let mut head = None;
        let mut inside: Vec<_> = self
            .messages
            .iter()
            .filter(|(k, m)| {
                if !replies {
                    // A reply is in the channel only as a broadcast.
                    return thread_of(m).is_none_or(|t| t == *k) || m["subtype"] == "thread_broadcast";
                }
                head = head.or((Some(*k) == parent).then_some(m));
                thread_of(m) == parent && Some(*k) != parent
            })
            .filter(|(k, _)| oldest.is_none_or(|o| *k > o) && latest.is_none_or(|l| *k < l))
            .collect();
        inside.sort_by_key(|(k, _)| *k);
        let mut body = json!({"ok": true});
        // With `oldest` alone the page is the oldest messages; with `latest` it is the newest.
        let (page, more) = if latest.is_some() {
            let from = inside.len().saturating_sub(take);
            (&inside[from..], from > 0)
        } else {
            // The replies cursor names the reply its page starts at, which is the one after the dropped one.
            let from = cursor.as_deref().and_then(|c| c.strip_prefix('c')?.parse().ok()).unwrap_or(0).min(inside.len());
            let to = (from + take).min(inside.len());
            if to < inside.len() {
                body["response_metadata"] = json!({"next_cursor": format!("c{}", to + 1)});
            }
            (&inside[from..to], to < inside.len())
        };
        let mut messages: Vec<&Value> = Vec::new();
        if replies {
            let Some(head) = head else {
                return Reply::json(&json!({"ok": false, "error": "thread_not_found"}));
            };
            messages.push(head);
            messages.extend(page.iter().map(|(_, m)| m));
        } else {
            messages.extend(page.iter().rev().map(|(_, m)| m));
        }
        body["messages"] = json!(messages);
        body["has_more"] = json!(more);
        if self.limited {
            body["is_limited"] = json!(true);
        }
        Reply::json(&body)
    }
}
