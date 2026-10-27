// browser pages: pluto, video, docs, quarto opens and proxy.connect

use super::*;

/// The window sends this only after the person confirms its page reply bar. A direct reply (a page grant with a reply
/// scope, or any client of this account) sets `hold`: a busy row keeps the line in memory, one per row, and types it
/// when its prompt frees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageReplyReq {
    pub workspace_id: String,
    pub handle: String,
    pub line: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hold: bool,
}

/// A reply without `hold` is never retried. `sent` means the marked line and Enter were recorded;
/// `typed_not_sent` leaves text in the pane, and `unknown` may have delivered it. With `hold` the outcome is `sent`,
/// `held`, `refused` (with its reason in `detail`) or `unknown`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageReplyRes {
    pub outcome: String,
    pub detail: String,
}

/// Open a Pluto-flavored `.jl` notebook in the backend-supervised
/// Pluto server. Path must be absolute on the backend host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlutoOpenReq {
    pub path: String,
}

/// `pluto.open` response — the per-notebook edit URL the frontend
/// hands to the OS browser-open. URL is loopback-shaped
/// (`http://127.0.0.1:1234/edit?id=<uuid>`); reaching a remote backend
/// requires an SSH `-L 1234:127.0.0.1:1234` tunnel on the launcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlutoOpenRes {
    pub url: String,
}

/// Open a video file in the OS browser. Path must be absolute on the backend
/// host (the frontend sends the cursored file's absolute path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoOpenReq {
    pub path: String,
}

/// `video.open` response — the loopback HTTP URL the frontend hands to the OS
/// browser-open. Shaped `http://127.0.0.1:<videoPort><abs-path>`; reaching a
/// remote backend requires an SSH `-L <videoPort>:127.0.0.1:<videoPort>`
/// tunnel on the launcher. ADR 0018.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoOpenRes {
    pub url: String,
}

/// Authority requested by an opener for one page entry. The daemon canonicalizes a row id before granting it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum ReplyScope {
    #[default]
    None,
    Row { workspace_id: String },
    All,
}

/// Open a local page or a folder's index in the OS browser. Missing authority is always ordinary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocsOpenReq {
    pub path: String,
    #[serde(default)]
    pub direct: ReplyScope,
}

/// `docs.open` response — the loopback HTTP URL the frontend hands to the OS
/// browser-open. Shaped `http://127.0.0.1:<docsPort>/<rel>`; reaching a remote
/// backend requires an SSH `-L <docsPort>:127.0.0.1:<docsPort>` tunnel on the
/// launcher. ADR 0024.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocsOpenRes {
    pub url: String,
}

/// Render a Quarto/markdown doc and open it in the OS browser. Path is
/// absolute on the backend host. `execute = false` (the `o` key) renders
/// structure/formatting only — fast, quarto-only, no side effects.
/// `execute = true` (the `O` key) runs code chunks — needs the language
/// kernels on the backend host and is slower.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuartoOpenReq {
    pub path: String,
    pub execute: bool,
}

/// `quarto.open` response — the rendered self-contained HTML (Quarto's
/// `--embed-resources` inlines all CSS/JS/images, so one document is enough).
/// The frontend writes it to a temp `.html` and hands it to the OS browser,
/// reusing the `text/html` preview's open path — no HTTP server or
/// port-forward needed.
///
/// The HTML rides as the frame's **trailing blob**, not in this JSON.
/// `--embed-resources` output routinely exceeds `codec::MAX_ENVELOPE_BYTES`
/// (a 1.2 MiB render base64'd to 1.6 MiB), and an over-cap envelope used to
/// kill the whole connection mid-session. `blob` is the codec's trailing-blob
/// descriptor (`len` = the HTML byte count) — REQUIRED, or `read_frame` won't
/// consume the appended bytes and the next frame desyncs onto raw HTML. Same
/// shape as `MathRenderRes` / `FileChunk`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuartoOpenRes {
    pub blob: BlobDescriptor,
}

/// `proxy.connect` request (ADR 0035) — the frame behind a `handoff` hello on a dedicated
/// proxy connection. `port` is the loopback target on the backend host; the
/// frame carries no host on purpose (the daemon dials `127.0.0.1` only).
/// `token` mirrors `HelloReq::token`: ignored by the daemon, kept so older
/// clients still parse; filesystem permissions are the trust boundary (as
/// for every op).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConnectReq {
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// `proxy.connect` response. After `{ok: true}` the connection stops being
/// a frame stream: every subsequent byte in BOTH directions is piped
/// verbatim to/from the dialed backend service. Errors (`bad_port`,
/// `dial_failed`) ride the standard error payload instead and the
/// connection closes without entering pipe mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConnectRes {
    pub ok: bool,
}

#[cfg(test)]
#[path = "pages_tests.rs"]
mod tests;
