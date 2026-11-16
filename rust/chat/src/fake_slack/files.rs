//! One file's reads for the fake: `files.info` answers its facts and its private URLs on `files.slack.com`, and each
//! URL answers bytes. The key names (`channels`, `groups`, `shares.public|private`, `thumb_360|480|160`,
//! `url_private_download`, `url_private`) are the ones SlackClaw's shelved client read; that a bot token's `files.info`
//! lists the channels a file is shared in is inferred and not measured. Every request is recorded by the fake as any
//! other, its `Authorization` header included.

use serde_json::{json, Value};

use super::{Reply, Seen};

/// File `F0PIC`, a PNG shared in `C0CHAN`, with a `thumb_360`; a test says what differs: `Files { size: Some(9), ..Files::new() }`.
pub struct Files {
    pub mimetype: String,
    /// The size `files.info` claims; `None` is the original's true length.
    pub size: Option<u64>,
    /// `files.info` has no `size` key at all.
    pub no_size: bool,
    pub original: Vec<u8>,
    pub thumb: Vec<u8>,
    /// Which `thumb_*` URLs `files.info` names.
    pub thumb_keys: Vec<&'static str>,
    /// `files.info` names `url_private_download` (else only `url_private`).
    pub download: bool,
    /// The `Content-Type` of every file answer.
    pub content_type: Option<String>,
    /// The channels the file is shared in: as `shares` (`public` for `C…`, `private` for `G…`), or, with `older_keys`,
    /// as the older `channels` and `groups` arrays.
    pub channels: Vec<String>,
    pub older_keys: bool,
    /// Every file URL but `url_private`'s answers a redirect to `url_private`'s path on this host.
    pub redirect_to: Option<String>,
}

impl Files {
    /// The path of the original as `url_private_download` names it, and as `url_private` names it.
    pub const DOWNLOAD: &'static str = "/files-pri/F0PIC/download/pic.png";
    pub const PRIVATE: &'static str = "/files-pri/F0PIC/pic.png";

    pub fn new() -> Files {
        Files {
            mimetype: "image/png".into(),
            size: None,
            no_size: false,
            original: b"\x89PNG original bytes".to_vec(),
            thumb: b"\x89PNG thumbnail bytes".to_vec(),
            thumb_keys: vec!["thumb_360"],
            download: true,
            content_type: Some("image/png".into()),
            channels: vec!["C0CHAN".into()],
            older_keys: false,
            redirect_to: None,
        }
    }

    pub fn thumb_path(key: &str) -> String {
        format!("/files-tmb/F0PIC/{key}")
    }

    fn info(&self, port: u16) -> Value {
        let at = |path: &str| format!("https://files.slack.com:{port}{path}");
        let mut file = json!({"id": "F0PIC", "mimetype": self.mimetype, "url_private": at(Files::PRIVATE)});
        if !self.no_size {
            file["size"] = self.size.unwrap_or(self.original.len() as u64).into();
        }
        if self.download {
            file["url_private_download"] = at(Files::DOWNLOAD).into();
        }
        for key in &self.thumb_keys {
            file[*key] = at(&Files::thumb_path(key)).into();
        }
        let (groups, public): (Vec<_>, Vec<_>) = self.channels.iter().partition(|c| c.starts_with('G'));
        if self.older_keys {
            file["channels"] = json!(public);
            file["groups"] = json!(groups);
        } else {
            let shares = |ids: Vec<&String>| ids.into_iter().map(|c| (c.clone(), json!([{"ts": "1700000800.000100"}]))).collect::<serde_json::Map<_, _>>();
            file["shares"] = json!({"public": shares(public), "private": shares(groups)});
        }
        json!({"ok": true, "file": file})
    }

    /// The answer to `files.info` or to a file URL, or `None` for a request that is neither.
    pub fn reply(&self, seen: &Seen, port: u16) -> Option<Reply> {
        let path = seen.path();
        if path == "/api/files.info" {
            return Some(match seen.param("file").as_deref() {
                Some("F0PIC") => Reply::json(&self.info(port)),
                _ => Reply::json(&json!({"ok": false, "error": "file_not_found"})),
            });
        }
        let body = match path.as_str() {
            Files::DOWNLOAD | Files::PRIVATE => &self.original,
            p if self.thumb_keys.iter().any(|k| p == Files::thumb_path(k)) => &self.thumb,
            _ => return None,
        };
        match &self.redirect_to {
            Some(host) if path != Files::PRIVATE => {
                return Some(Reply::redirect(&format!("https://{host}:{port}{}", Files::PRIVATE)))
            }
            _ => {}
        }
        let headers = self.content_type.iter().map(|c| ("Content-Type".to_string(), c.clone())).collect();
        Some(Reply::Status { code: 200, headers, body: body.clone() })
    }
}
