// pages/video.rs — a tiny loopback HTTP/1.1 static file server with byte-range
// support, used to stream video files to the OS browser's native HTML5 <video>
// player (ADR 0018, revised). The frontend's `o` key asks the backend for a
// `video.open` URL.
//
// Why hand-rolled rather than axum/tower-http: the backend otherwise has no
// HTTP stack, and the need is narrow — GET one file, honour a single
// `Range: bytes=` header so the browser can seek. ~200 lines on tokio beats
// pulling in the hyper/tower tree. Browsers send single-range requests for
// <video>; multipart ranges fall back to a full 200. `content_type`,
// `parse_range`, `serve_file` and `write_simple` are shared with `pages::site`,
// so the two servers cannot disagree about Range or an empty file.
//
// Scope/security: binds 127.0.0.1 only and serves only connections from this
// OS account (`serve_own`, ADR 0049). Serves only files whose
// extension is a known video type, and only real regular files. It still must
// never accept a raw
// filesystem path from the request itself (that turned "video files the
// single user can already read" into "any file, video or not, anyone on the
// box can read"). Instead `video.open` REGISTERS the one file it's handing
// out under an unguessable token (`register_video`, below); this server only
// ever serves a path it was explicitly asked to grant, looked up by that
// token — never the request-supplied path.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Context, Result};
#[cfg(test)]
use tokio::io::AsyncReadExt;
#[cfg(test)]
use tokio::net::TcpListener;
use tokio::net::TcpStream;

use super::http::{content_type, read_request, serve_file, write_simple, Request};
use super::random_token;

/// Live video grants: `token -> absolute path`, plus insertion order so the
/// oldest grant can be evicted once `MAX_GRANTS` is exceeded (a session that
/// pops out many videos shouldn't grow this forever). `BTreeMap`/`VecDeque`
/// `::new()` are both const, so this initializes without lazy init — same
/// shape as `pages::site`'s `SITE_ROOTS`.
struct Grants {
    by_token: BTreeMap<String, PathBuf>,
    order: VecDeque<String>,
}
static GRANTS: RwLock<Grants> = RwLock::new(Grants {
    by_token: BTreeMap::new(),
    order: VecDeque::new(),
});

/// Cap on live grants. Generous for how many videos a session realistically
/// pops out at once; just a backstop against unbounded growth.
const MAX_GRANTS: usize = 64;

/// Register `path` under a fresh unguessable token, evicting the oldest grant
/// if the cap is exceeded. Returns the token to embed in the served URL, or
/// `None` if the CSPRNG couldn't be read — callers must refuse to serve
/// rather than fall back to a guessable token (security review). Called by
/// the `video.open` handler — never by anything driven off request input.
pub fn register_video(path: PathBuf) -> Option<String> {
    let token = random_token()?;
    let mut g = GRANTS.write().unwrap_or_else(|p| p.into_inner());
    g.by_token.insert(token.clone(), path);
    g.order.push_back(token.clone());
    while g.order.len() > MAX_GRANTS {
        if let Some(old) = g.order.pop_front() {
            g.by_token.remove(&old);
        }
    }
    Some(token)
}

fn path_for_token(token: &str) -> Option<PathBuf> {
    GRANTS
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_token
        .get(token)
        .cloned()
}

/// PREFERRED loopback port for the video server (env-overridable, default
/// 1235). This is a preference, not a promise: on a shared host another
/// user's daemon may already hold it (the 2026-07-23 shared-host collision — two
/// users' daemons both defaulting to 1235, loser's `o` broken with the
/// browser hitting the *winner's* server → "no such grant"). `spawn` falls
/// back to an OS-assigned ephemeral port; everything that needs the real
/// port (`video.open` URLs, the ADR-0035 proxy allowlist) must read
/// `bound_video_port()`, never this.
pub fn video_port() -> u16 {
    std::env::var("SOT_VIDEO_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1235)
}

/// The port the video server ACTUALLY bound (0 = not bound / never spawned).
/// URL builders and the proxy allowlist read this — advertising or dialing
/// the merely-preferred port when the bind lost the race would point the
/// browser (and the user's grant token) at another user's server.
static BOUND_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

pub fn bound_video_port() -> Option<u16> {
    match BOUND_PORT.load(std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        p => Some(p),
    }
}

/// Spawn the video file server on `127.0.0.1:preferred`, falling back to an
/// OS-assigned ephemeral port when the preferred one is taken (another
/// user's daemon on a shared host, typically). Returns once a listener is
/// bound; the accept loop runs for the life of the process. Idempotent
/// callers should spawn this once at startup. The actual port is recorded
/// for `bound_video_port()`.
pub async fn spawn(preferred: u16) -> Result<()> {
    let listener = match crate::pages::bind_page_listener(preferred) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(preferred, error = %e, "video preferred port taken — falling back to an ephemeral port (multi-user host?)");
            crate::pages::bind_page_listener(0)
                .context("bind video http server on an ephemeral 127.0.0.1 port")?
        }
    };
    let port = listener
        .local_addr()
        .context("video http server local_addr")?
        .port();
    BOUND_PORT.store(port, std::sync::atomic::Ordering::SeqCst);
    tracing::info!(port, "video http server listening");
    tokio::spawn(sot_platform::identity::peer_owner::serve_own(listener, "video", serve_conn));
    Ok(())
}

/// One accepted video connection: `handle_conn`, its error logged at debug.
async fn serve_conn(stream: TcpStream) {
    if let Err(e) = handle_conn(stream).await {
        tracing::debug!(error = %e, "video http conn ended");
    }
}

/// Rust video opens, preview bounds and HTTP MIME decisions use sot_protocol::video_path::video_mime; leading-dot filenames follow the existing Julia suffix contract.
/// Public so the `video.open` handler can reject non-video requests before building a URL; the preview's size-gate
/// exemption and bytes reader ask it too.
pub fn is_servable_video(path: &Path) -> bool {
    path.to_str()
        .and_then(sot_protocol::video_path::video_mime)
        .is_some()
}

/// One accepted connection: its requests in turn (`read_request`), while each reply leaves it open.
pub(super) async fn handle_conn(mut stream: TcpStream) -> Result<()> {
    let mut buf = Vec::new();
    while let Some(request) = read_request(&mut stream, &mut buf).await? {
        if !serve_request(&mut stream, &request).await? {
            break;
        }
    }
    Ok(())
}

/// One request: the file its token grants, or a plain refusal. Returns whether the connection stays open.
async fn serve_request(stream: &mut TcpStream, request: &Request) -> Result<bool> {
    if request.method != "GET" && request.method != "HEAD" {
        return write_simple(
            stream,
            &request.method,
            "405 Method Not Allowed",
            "only GET/HEAD",
            false,
        )
        .await;
    }
    let keep = request.keep;

    // The request target's path is `/<token>` — an opaque grant id
    // `video.open` registered, NOT a filesystem path (that was the hole: any
    // local user could GET an arbitrary absolute path). Look up the one file
    // this token was granted for; anything else 404s.
    let raw_path = request.target.split('?').next().unwrap_or("");
    let token = raw_path.trim_start_matches('/');
    let fs_path = match path_for_token(token) {
        Some(p) => p,
        None => {
            return write_simple(
                stream,
                &request.method,
                "404 Not Found",
                "no such grant",
                keep,
            )
            .await
        }
    };

    if !is_servable_video(&fs_path) {
        return write_simple(
            stream,
            &request.method,
            "403 Forbidden",
            "not a video file",
            keep,
        )
        .await;
    }
    let file = match tokio::fs::File::open(&fs_path).await {
        Ok(f) => f,
        Err(_) => {
            return write_simple(
                stream,
                &request.method,
                "404 Not Found",
                "no such file",
                keep,
            )
            .await
        }
    };
    serve_file(stream, request, file, content_type(&fs_path), "").await
}

#[cfg(test)]
#[path = "video_bind_fallback_tests.rs"]
mod bind_fallback_tests;

#[cfg(test)]
#[path = "video_grant_token_tests.rs"]
mod grant_token_tests;

#[cfg(test)]
#[path = "video_ext_tests.rs"]
mod video_ext_tests;

#[cfg(test)]
#[path = "video_serve_own_tests.rs"]
mod serve_own_tests;
