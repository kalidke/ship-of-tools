//! A workspace's people for the fake: `users.list` answers them in pages of the request's `limit`, its cursor the index of
//! the next page's first person. The entry's keys (`id`, `name`, `deleted`, `is_bot`, `profile.display_name`,
//! `profile.real_name`) are Slack's documented ones, not measured against a live workspace.

use serde_json::{json, Value};

use super::{Reply, Seen};

pub struct Members(pub Vec<Value>);

impl Members {
    /// A person's entry as `users.list` lists one.
    pub fn person(id: &str, display: &str, real: &str) -> Value {
        json!({"id": id, "name": id.to_lowercase(), "deleted": false, "is_bot": false, "profile": {"display_name": display, "real_name": real}})
    }

    /// The answer to `users.list`, or `None` for another request.
    pub fn reply(&self, seen: &Seen) -> Option<Reply> {
        if !seen.path().ends_with("/users.list") {
            return None;
        }
        let limit = seen.param("limit").and_then(|l| l.parse().ok()).unwrap_or(100usize);
        let from = seen.param("cursor").and_then(|c| c.parse().ok()).unwrap_or(0usize);
        let page: Vec<&Value> = self.0.iter().skip(from).take(limit).collect();
        let next = if from + limit < self.0.len() { (from + limit).to_string() } else { String::new() };
        Some(Reply::json(&json!({"ok": true, "members": page, "response_metadata": {"next_cursor": next}})))
    }
}
