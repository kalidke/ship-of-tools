//! Slack's file upload for the fake, in the three steps SlackClaw measured against a workspace (its facts note,
//! section 2): `files.getUploadURLExternal` (a GET with `filename` and `length`) answers a file id and an upload URL;
//! the raw bytes are POSTed to that URL; `files.completeUploadExternal` (a JSON POST of `files`, `channel_id`,
//! `initial_comment` and, inferred and not measured, `thread_ts`) shares the file and answers it with its shares.
//! Every request is recorded by the fake as any other, the upload's raw body and its `Authorization` header included.

use serde_json::json;

use super::{Reply, Seen};

/// The three steps answered as Slack answered them; a test answers a step its own way by matching it first.
pub struct Upload {
    file: String,
    host: String,
    path: String,
    ts: Option<String>,
    private: bool,
}

impl Upload {
    /// File `F0FILE`, uploaded to `files.slack.com`, its share in the channel at `1700000800.000100`.
    pub fn new() -> Upload {
        Upload {
            file: "F0FILE".into(),
            host: "files.slack.com".into(),
            path: "/upload/v1/F0FILE".into(),
            ts: Some("1700000800.000100".into()),
            private: false,
        }
    }

    /// The upload URL names this host and path (the fixture's port is added).
    pub fn upload_to(mut self, host: &str, path: &str) -> Upload {
        self.host = host.into();
        self.path = path.into();
        self
    }

    /// The share answers no `ts` (Slack's answer can lack one).
    pub fn no_ts(mut self) -> Upload {
        self.ts = None;
        self
    }

    /// The share is listed under `shares.private`, as a private channel's is.
    pub fn private(mut self) -> Upload {
        self.private = true;
        self
    }

    /// The path of the upload URL, for a test to find step 2 among what the fake saw.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The step's answer, or `None` for a request that is none of the three.
    pub fn reply(&self, seen: &Seen, port: u16) -> Option<Reply> {
        match seen.path().as_str() {
            "/api/files.getUploadURLExternal" => Some(Reply::json(&json!({
                "ok": true,
                "file_id": self.file,
                "upload_url": format!("https://{}:{port}{}", self.host, self.path),
            }))),
            p if p == self.path => Some(Reply::ok(format!("OK - {}", seen.body.len()).as_bytes())),
            "/api/files.completeUploadExternal" => {
                let channel = seen.json().and_then(|b| b["channel_id"].as_str().map(String::from)).unwrap_or_default();
                let shares = match &self.ts {
                    Some(ts) => json!({ channel: [{"ts": ts}] }),
                    None => json!({}),
                };
                let kind = if self.private { "private" } else { "public" };
                Some(Reply::json(&json!({
                    "ok": true,
                    "files": [{"id": self.file, "title": "", "shares": { kind: shares }}],
                })))
            }
            _ => None,
        }
    }
}

impl Default for Upload {
    fn default() -> Upload {
        Upload::new()
    }
}
