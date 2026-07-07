// http_serve.rs — a tiny loopback HTTP/1.1 static file server with byte-range
// support, used to stream video files to the OS browser's native HTML5 <video>
// player (ADR 0018, revised). The frontend's `o` key asks the backend for a
// `video.open` URL; the backend returns `http://127.0.0.1:<port><abs-path>`,
// which the launcher SSH-forwards to the local machine so the browser can
// reach it.
//
// Why hand-rolled rather than axum/tower-http: the backend otherwise has no
// HTTP stack, and the need is narrow — GET one file, honour a single
// `Range: bytes=` header so the browser can seek. ~200 lines on tokio beats
// pulling in the hyper/tower tree. Browsers send single-range requests for
// <video>; multipart ranges fall back to a full 200.
//
// Scope/security: binds 127.0.0.1 only (then SSH-forwarded, loopback on both
// ends). Serves only files whose extension is a known video type, and only
// real regular files. This port has no auth of its own, though — any local
// user on a shared host can reach it — so it must never accept a raw
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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Live video grants: `token -> absolute path`, plus insertion order so the
/// oldest grant can be evicted once `MAX_GRANTS` is exceeded (a session that
/// pops out many videos shouldn't grow this forever). `BTreeMap`/`VecDeque`
/// `::new()` are both const, so this initializes without lazy init — same
/// shape as `site_serve`'s `SITE_ROOTS`.
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

/// Generate an unguessable token (32 lowercase hex chars = 128 bits from the
/// OS CSPRNG), or `None` if `/dev/urandom` can't be read. Fails CLOSED
/// (security review): a predictable token defeats the whole point of this
/// scheme, so callers must refuse to mint a grant rather than fall back to
/// something merely "unpredictable-ish". `/dev/urandom` never blocks and is
/// present on every host this backend targets (Linux/macOS) — not worth a
/// `rand` crate dependency for this one call site, and this should never
/// actually fail.
fn random_token() -> Option<String> {
    use std::io::Read;
    let mut buf = [0u8; 16];
    if let Err(e) = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)) {
        tracing::error!(error = %e, "random_token: /dev/urandom read failed — refusing to mint a predictable token");
        return None;
    }
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Video extensions this server will serve. Mirrors `ShipToolsVideoFile`'s
/// `VIDEO_EXTENSIONS` and `files_mode::mime_for_path`'s video arm.
const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mov", "mkv", "m4v"];

const READ_CHUNK: usize = 64 * 1024;

/// Loopback port the video server listens on. Fixed (env-overridable) so the
/// launcher can SSH-forward it without negotiation. Resolved identically at
/// spawn time and when building `video.open` URLs.
pub fn video_port() -> u16 {
    std::env::var("SOT_VIDEO_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1235)
}

/// Spawn the video file server on `127.0.0.1:port`. Returns once the listener
/// is bound; the accept loop runs for the life of the process. Idempotent
/// callers should spawn this once at startup.
pub async fn spawn(port: u16) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("bind video http server on 127.0.0.1:{port}"))?;
    tracing::info!(port, "video http server listening");
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _peer)) => {
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(stream).await {
                            tracing::debug!(error = %e, "video http conn ended");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "video http accept failed");
                }
            }
        }
    });
    Ok(())
}

/// Content-Type for a video path by extension. Falls back to a generic stream
/// type so the browser still treats it as media.
fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("mkv") => "video/x-matroska",
        _ => "application/octet-stream",
    }
}

/// Whether this path is a video extension this server will serve. Public so
/// the `video.open` handler can reject non-video requests before building a URL.
pub fn is_servable_video(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some(e) if VIDEO_EXTS.contains(&e)
    )
}

async fn write_simple(stream: &mut TcpStream, status: &str, body: &str) -> Result<()> {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

async fn handle_conn(mut stream: TcpStream) -> Result<()> {
    // Read headers (up to the blank line). Bounded so a malformed client can't
    // grow this unbounded.
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(()); // client closed before sending a full request
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 16 * 1024 {
            return write_simple(&mut stream, "431 Request Header Fields Too Large", "headers too large").await;
        }
    }

    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method != "GET" && method != "HEAD" {
        return write_simple(&mut stream, "405 Method Not Allowed", "only GET/HEAD").await;
    }

    // Range header (case-insensitive name).
    let mut range_hdr: Option<String> = None;
    for line in lines {
        if let Some((name, val)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("range") {
                range_hdr = Some(val.trim().to_string());
            }
        }
    }

    // The request target's path is `/<token>` — an opaque grant id
    // `video.open` registered, NOT a filesystem path (that was the hole: any
    // local user could GET an arbitrary absolute path). Look up the one file
    // this token was granted for; anything else 404s.
    let raw_path = target.split('?').next().unwrap_or("");
    let token = raw_path.trim_start_matches('/');
    let fs_path = match path_for_token(token) {
        Some(p) => p,
        None => return write_simple(&mut stream, "404 Not Found", "no such grant").await,
    };

    if !is_servable_video(&fs_path) {
        return write_simple(&mut stream, "403 Forbidden", "not a video file").await;
    }
    let meta = match tokio::fs::metadata(&fs_path).await {
        Ok(m) if m.is_file() => m,
        _ => return write_simple(&mut stream, "404 Not Found", "no such file").await,
    };
    let total = meta.len();
    let ctype = content_type(&fs_path);

    // Parse a single `bytes=START-END` range (END optional). Multipart or
    // malformed ranges fall back to the full body.
    let mut start: u64 = 0;
    let mut end: u64 = total.saturating_sub(1);
    let mut partial = false;
    if let Some(r) = range_hdr.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
        if !r.contains(',') {
            let (s, e) = r.split_once('-').unwrap_or(("", ""));
            match (s.parse::<u64>().ok(), e.parse::<u64>().ok()) {
                (Some(s), Some(e)) => {
                    start = s;
                    end = e.min(total.saturating_sub(1));
                    partial = true;
                }
                (Some(s), None) => {
                    start = s;
                    partial = true;
                }
                (None, Some(suffix)) => {
                    // `bytes=-N` — last N bytes.
                    start = total.saturating_sub(suffix);
                    partial = true;
                }
                _ => {}
            }
        }
    }
    if partial && (start > end || start >= total) {
        let resp = format!(
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(resp.as_bytes()).await?;
        stream.flush().await?;
        return Ok(());
    }

    let len = end - start + 1;
    let status = if partial { "206 Partial Content" } else { "200 OK" };
    let mut header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nAccept-Ranges: bytes\r\nContent-Length: {len}\r\nConnection: close\r\n"
    );
    if partial {
        header.push_str(&format!("Content-Range: bytes {start}-{end}/{total}\r\n"));
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes()).await?;

    if method == "HEAD" {
        stream.flush().await?;
        return Ok(());
    }

    // Stream the requested slice.
    let mut file = tokio::fs::File::open(&fs_path).await?;
    if start > 0 {
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(start)).await?;
    }
    let mut remaining = len;
    let mut chunk = vec![0u8; READ_CHUNK];
    while remaining > 0 {
        let want = remaining.min(READ_CHUNK as u64) as usize;
        let n = file.read(&mut chunk[..want]).await?;
        if n == 0 {
            break;
        }
        // A broken pipe here just means the browser closed the connection
        // (seeked away / paused) — not an error worth surfacing.
        if stream.write_all(&chunk[..n]).await.is_err() {
            return Ok(());
        }
        remaining -= n as u64;
    }
    stream.flush().await.ok();
    Ok(())
}
