//! One file's reads for the fake: `files.info` answers its facts and its private URLs on `files.slack.com`, and each
//! URL answers bytes. The key names (`channels`, `groups`, `shares.public|private`, `thumb_360|480|160`,
//! `url_private_download`, `url_private`) are the ones SlackClaw's shelved client read; that a bot token's `files.info`
//! lists the channels a file is shared in is inferred and not measured. Every request is recorded by the fake as any
//! other, its `Authorization` header included.

use serde_json::{json, Value};

use super::{Reply, Seen};

const DOWNLOAD: &str = "/files-pri/F0PIC/download/pic.png";
const PRIVATE: &str = "/files-pri/F0PIC/pic.png";

enum Members {
    /// `shares.public` for a `C…` channel and `shares.private` for a `G…` one.
    Shared(Vec<String>),
    /// The older keys: `channels` for a `C…` channel and `groups` for a `G…` one.
    Listed(Vec<String>),
}

/// File `F0PIC`, a PNG shared in `C0CHAN`, with a `thumb_360`; a test changes what it needs.
pub struct Files {
    mimetype: String,
    size: Option<u64>,
    original: Vec<u8>,
    thumb: Vec<u8>,
    thumb_keys: Vec<&'static str>,
    download: bool,
    content_type: Option<String>,
    members: Members,
    redirect_to: Option<String>,
}

impl Files {
    pub fn new() -> Files {
        Files {
            mimetype: "image/png".into(),
            size: None,
            original: b"\x89PNG original bytes".to_vec(),
            thumb: b"\x89PNG thumbnail bytes".to_vec(),
            thumb_keys: vec!["thumb_360"],
            download: true,
            content_type: Some("image/png".into()),
            members: Members::Shared(vec!["C0CHAN".into()]),
            redirect_to: None,
        }
    }

    pub fn mimetype(mut self, mimetype: &str) -> Files {
        self.mimetype = mimetype.into();
        self
    }

    /// The size `files.info` claims, whatever the original's bytes are (they default to its true length).
    pub fn size(mut self, size: u64) -> Files {
        self.size = Some(size);
        self
    }

    pub fn original(mut self, bytes: Vec<u8>) -> Files {
        self.original = bytes;
        self
    }

    pub fn thumb(mut self, bytes: Vec<u8>) -> Files {
        self.thumb = bytes;
        self
    }

    /// Which `thumb_*` URLs `files.info` names (none: a file with no thumbnail).
    pub fn thumb_keys(mut self, keys: &[&'static str]) -> Files {
        self.thumb_keys = keys.to_vec();
        self
    }

    /// No `url_private_download`: only `url_private` names the original.
    pub fn no_download(mut self) -> Files {
        self.download = false;
        self
    }

    /// The `Content-Type` of every file answer (`None`: no such header).
    pub fn content_type(mut self, content_type: Option<&str>) -> Files {
        self.content_type = content_type.map(String::from);
        self
    }

    /// The file is shared in these channels, as `shares` gives them.
    pub fn shared_in(mut self, channels: &[&str]) -> Files {
        self.members = Members::Shared(channels.iter().map(|c| c.to_string()).collect());
        self
    }

    /// The file is shared in these channels, as the older `channels` and `groups` keys give them.
    pub fn listed_in(mut self, channels: &[&str]) -> Files {
        self.members = Members::Listed(channels.iter().map(|c| c.to_string()).collect());
        self
    }

    /// Every file URL but `url_private`'s answers a redirect to `url_private`'s path on this host.
    pub fn redirecting_to(mut self, host: &str) -> Files {
        self.redirect_to = Some(host.into());
        self
    }

    /// The path a thumbnail key's URL has.
    pub fn thumb_path(key: &str) -> String {
        format!("/files-tmb/F0PIC/{key}")
    }

    /// The path of the original, as `url_private_download` names it.
    pub fn download_path() -> &'static str {
        DOWNLOAD
    }

    /// The path of the original, as `url_private` names it.
    pub fn private_path() -> &'static str {
        PRIVATE
    }

    fn info(&self, port: u16) -> Value {
        let at = |path: &str| format!("https://files.slack.com:{port}{path}");
        let mut file = json!({
            "id": "F0PIC",
            "mimetype": self.mimetype,
            "size": self.size.unwrap_or(self.original.len() as u64),
            "url_private": at(PRIVATE),
        });
        if self.download {
            file["url_private_download"] = at(DOWNLOAD).into();
        }
        for key in &self.thumb_keys {
            file[*key] = at(&Files::thumb_path(key)).into();
        }
        match &self.members {
            Members::Shared(channels) => {
                let (mut public, mut private) = (json!({}), json!({}));
                for c in channels {
                    let kind = if c.starts_with('G') { &mut private } else { &mut public };
                    kind[c.as_str()] = json!([{"ts": "1700000800.000100"}]);
                }
                file["shares"] = json!({"public": public, "private": private});
            }
            Members::Listed(channels) => {
                let (groups, public): (Vec<_>, Vec<_>) = channels.iter().partition(|c| c.starts_with('G'));
                file["channels"] = json!(public);
                file["groups"] = json!(groups);
            }
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
            DOWNLOAD | PRIVATE => &self.original,
            p if self.thumb_keys.iter().any(|k| p == Files::thumb_path(k)) => &self.thumb,
            _ => return None,
        };
        match &self.redirect_to {
            Some(host) if path != PRIVATE => return Some(Reply::redirect(&format!("https://{host}:{port}{PRIVATE}"))),
            _ => {}
        }
        let headers = self.content_type.iter().map(|c| ("Content-Type".to_string(), c.clone())).collect();
        Some(Reply::Status { code: 200, headers, body: body.clone() })
    }
}

impl Default for Files {
    fn default() -> Files {
        Files::new()
    }
}
