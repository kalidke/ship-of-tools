//! A channel for the fake Slack that answers `conversations.history` the way SlackClaw measured it (its facts note,
//! "Page order with `oldest` alone, measured" and "The walk back with no cursor, measured"; "Posts arriving during
//! reads and the walk (P-ORDER), measured" for the parent's `latest_reply`). Both bounds are exclusive; with both, page 1
//! holds the newest messages in range and a read with `latest` lowered to a page's oldest runs on older; the last page
//! that holds messages says `has_more: false`, full or not, and the read after it is empty; inside a page the messages
//! are newest first. With `oldest` alone, page 1 holds the OLDEST messages, so a walk that lowers `latest` from there sees
//! one page and then an empty one. A parent's `latest_reply` is whatever the test gave it, and is not held to the read's
//! `latest` while the list is. A cursor is refused (`invalid_cursor`): no pass sends one. Replies are not played here.
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
        if seen.param("cursor").is_some() {
            return Reply::json(&json!({"ok": false, "error": "invalid_cursor"}));
        }
        let (oldest, latest) = (bound("oldest"), bound("latest"));
        let limit: usize = seen.param("limit").and_then(|l| l.parse().ok()).unwrap_or(100);
        let take = self.cap.map_or(limit, |c| c.min(limit));
        let mut inside: Vec<_> = self
            .messages
            .iter()
            .filter(|(k, _)| oldest.is_none_or(|o| *k > o) && latest.is_none_or(|l| *k < l))
            .collect();
        inside.sort_by_key(|(k, _)| *k);
        // With `oldest` alone the page is the oldest messages; with `latest` it is the newest.
        let (page, more) = if latest.is_some() {
            let from = inside.len().saturating_sub(take);
            (&inside[from..], from > 0)
        } else {
            let to = take.min(inside.len());
            (&inside[..to], to < inside.len())
        };
        let messages: Vec<&Value> = page.iter().rev().map(|(_, m)| m).collect();
        let mut body = json!({"ok": true, "messages": messages, "has_more": more});
        if self.limited {
            body["is_limited"] = json!(true);
        }
        Reply::json(&body)
    }
}
