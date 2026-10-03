// http_serve.rs — a tiny loopback HTTP/1.1 static file server with byte-range
// support, used to stream video files to the OS browser's native HTML5 <video>
// player (ADR 0018, revised). The frontend's `o` key asks the backend for a
// `video.open` URL; the backend returns `http://127.0.0.1:<port><abs-path>`,
// which reaches a remote frontend through the daemon's page proxy (ADR 0035).
//
// Why hand-rolled rather than axum/tower-http: the backend otherwise has no
// HTTP stack, and the need is narrow — GET one file, honour a single
// `Range: bytes=` header so the browser can seek. ~200 lines on tokio beats
// pulling in the hyper/tower tree. Browsers send single-range requests for
// <video>; multipart ranges fall back to a full 200. `content_type`,
// `parse_range`, `serve_file` and `write_simple` are shared with `site_serve`,
// so the two servers cannot disagree about Range or an empty file.
//
// Scope/security: binds 127.0.0.1 only and serves only connections from this
// OS account (`spawn_accept_loop`, decision 0031). Serves only files whose
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
/// OS CSPRNG), or `None` if the OS CSPRNG can't be read. Fails CLOSED
/// (security review): a predictable token defeats the whole point of this
/// scheme, so callers must refuse to mint a grant rather than fall back to
/// something merely "unpredictable-ish".
fn random_token() -> Option<String> {
    let mut buf = [0u8; 16];
    if let Err(e) = getrandom::fill(&mut buf) {
        tracing::error!(error = %e, "random_token: OS CSPRNG read failed — refusing to mint a predictable token");
        return None;
    }
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Video extensions this server will serve. Mirrors `ShipToolsVideoFile`'s
/// `VIDEO_EXTENSIONS` and `files_mode::mime_for_path`'s video arm.
const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mov", "mkv", "m4v"];

const READ_CHUNK: usize = 64 * 1024;

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
    let listener = match TcpListener::bind(("127.0.0.1", preferred)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(preferred, error = %e, "video preferred port taken — falling back to an ephemeral port (multi-user host?)");
            TcpListener::bind(("127.0.0.1", 0))
                .await
                .context("bind video http server on an ephemeral 127.0.0.1 port")?
        }
    };
    let port = listener
        .local_addr()
        .context("video http server local_addr")?
        .port();
    BOUND_PORT.store(port, std::sync::atomic::Ordering::SeqCst);
    tracing::info!(port, "video http server listening");
    spawn_accept_loop(listener, "video", sot_log::peer_owner::admit, handle_conn);
    Ok(())
}

/// The accept loop every loopback page listener this daemon opens runs: the video server, the static-site server
/// and each docs-pool listener. Decision 0031: a connection whose far end is not this OS account is closed with no
/// byte written; `admit` is `sot_log::peer_owner::admit` outside tests. The check runs in the connection's own
/// task, on a blocking thread, never on the accept loop.
pub(crate) fn spawn_accept_loop<H, Fut>(listener: TcpListener, name: &'static str, admit: sot_log::peer_owner::Admit, handle: H)
where
    H: Fn(TcpStream) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let handle = std::sync::Arc::new(handle);
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let handle = std::sync::Arc::clone(&handle);
                    tokio::spawn(async move {
                        let Ok(local) = stream.local_addr() else { return };
                        if !tokio::task::spawn_blocking(move || admit(name, local, peer)).await.unwrap_or(false) {
                            return; // `stream` drops here: closed with no byte written
                        }
                        if let Err(e) = handle(stream).await {
                            tracing::debug!(listener = name, port, error = %e, "page connection ended");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(listener = name, port, error = %e, "page accept failed");
                }
            }
        }
    });
}

/// Content-Type by extension: the static-site table plus the video rows.
/// Shared with `site_serve`; falls back to a generic stream type so the
/// browser still treats an unknown file as opaque bytes. Serving a video type
/// is still gated by `is_servable_video`, so this table serves nothing new.
pub(crate) fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") | Some("map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("eot") => "application/vnd.ms-fontobject",
        Some("wasm") => "application/wasm",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml; charset=utf-8",
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

pub(crate) async fn write_simple(stream: &mut TcpStream, status: &str, body: &str) -> Result<()> {
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
    let file = match tokio::fs::File::open(&fs_path).await {
        Ok(f) => f,
        Err(_) => return write_simple(&mut stream, "404 Not Found", "no such file").await,
    };
    serve_file(
        &mut stream,
        method == "HEAD",
        file,
        content_type(&fs_path),
        range_hdr.as_deref(),
        "",
    )
    .await
}

/// A parsed single-range request against a body of known length.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RangeReq {
    /// No usable range: send the whole body.
    Full,
    /// Send bytes `start..=end`, both inside the body.
    Part { start: u64, end: u64 },
    /// The range starts past the end, or is inverted: 416.
    Unsatisfiable,
}

/// Parse a single `bytes=START-END` range (END optional, `bytes=-N` = last N).
/// Multipart or malformed ranges fall back to the full body.
pub(crate) fn parse_range(hdr: Option<&str>, total: u64) -> RangeReq {
    let last = total.saturating_sub(1);
    let mut start: u64 = 0;
    let mut end: u64 = last;
    let mut partial = false;
    if let Some(r) = hdr.and_then(|r| r.strip_prefix("bytes=")) {
        if !r.contains(',') {
            let (s, e) = r.split_once('-').unwrap_or(("", ""));
            match (s.parse::<u64>().ok(), e.parse::<u64>().ok()) {
                (Some(s), Some(e)) => {
                    start = s;
                    end = e.min(last);
                    partial = true;
                }
                (Some(s), None) => {
                    start = s;
                    partial = true;
                }
                (None, Some(suffix)) => {
                    start = total.saturating_sub(suffix);
                    partial = true;
                }
                _ => {}
            }
        }
    }
    if !partial {
        RangeReq::Full
    } else if start > end || start >= total {
        RangeReq::Unsatisfiable
    } else {
        RangeReq::Part { start, end }
    }
}

/// Send `file` on `stream`: length and regular-file check come from fstat on
/// the open fd (never a second path lookup), a single `Range` is honoured, and
/// `Accept-Ranges: bytes` is always sent. `extra_headers` is zero or more
/// complete `Name: value\r\n` lines. A 0-byte file sends `Content-Length: 0`.
pub(crate) async fn serve_file(
    stream: &mut TcpStream,
    head_only: bool,
    mut file: tokio::fs::File,
    ctype: &str,
    range_hdr: Option<&str>,
    extra_headers: &str,
) -> Result<()> {
    let total = match file.metadata().await {
        Ok(m) if m.is_file() => m.len(),
        _ => return write_simple(stream, "404 Not Found", "no such file").await,
    };
    let (start, len, partial) = match parse_range(range_hdr, total) {
        RangeReq::Full => (0, total, None),
        RangeReq::Part { start, end } => (start, end - start + 1, Some(end)),
        RangeReq::Unsatisfiable => {
            let resp = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(resp.as_bytes()).await?;
            stream.flush().await?;
            return Ok(());
        }
    };
    let status = if partial.is_some() { "206 Partial Content" } else { "200 OK" };
    let mut header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nAccept-Ranges: bytes\r\nContent-Length: {len}\r\n{extra_headers}Connection: close\r\n"
    );
    if let Some(end) = partial {
        header.push_str(&format!("Content-Range: bytes {start}-{end}/{total}\r\n"));
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes()).await?;

    if head_only {
        stream.flush().await?;
        return Ok(());
    }

    // Stream the requested slice.
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

#[cfg(test)]
mod range_tests {
    use super::*;

    #[test]
    fn parse_range_forms() {
        let p = |h: &str, t| parse_range(Some(h), t);
        assert_eq!(p("bytes=100-199", 1000), RangeReq::Part { start: 100, end: 199 });
        assert_eq!(p("bytes=100-", 1000), RangeReq::Part { start: 100, end: 999 });
        assert_eq!(p("bytes=-10", 1000), RangeReq::Part { start: 990, end: 999 });
        assert_eq!(p("bytes=900-5000", 1000), RangeReq::Part { start: 900, end: 999 });
        assert_eq!(p("bytes=5-2", 1000), RangeReq::Unsatisfiable);
        assert_eq!(p("bytes=1000-", 1000), RangeReq::Unsatisfiable);
        assert_eq!(p("bytes=0-", 0), RangeReq::Unsatisfiable);
        assert_eq!(parse_range(None, 0), RangeReq::Full);
        assert_eq!(p("bytes=0-1,5-6", 1000), RangeReq::Full);
    }

    /// The video server used to send `Content-Length: 1` for an empty file
    /// (`end - start + 1` with end clamped to 0). Through the real listener.
    #[tokio::test]
    async fn empty_file_sends_content_length_zero() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("empty.mp4");
        std::fs::write(&f, b"").unwrap();
        let token = register_video(f).expect("register_video");
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let _ = handle_conn(stream).await;
            }
        });
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(format!("GET /{token} HTTP/1.1\r\nConnection: close\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut raw = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), s.read_to_end(&mut raw))
            .await
            .expect("server did not close the connection")
            .unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        assert!(text.starts_with("HTTP/1.1 200 OK"), "got: {text}");
        assert!(text.contains("Content-Length: 0\r\n"), "got: {text}");
        assert!(text.ends_with("\r\n\r\n"), "body must be empty, got: {text}");
    }
}

#[cfg(test)]
mod bind_fallback_tests {
    use super::*;

    /// Preferred port taken (an ephemeral squatter standing in for another
    /// user's daemon on a shared host) → `spawn` falls back to an
    /// OS-assigned port, records it for `bound_video_port()`, and actually
    /// serves on it (unknown grant → 404, proving it's OUR server).
    #[tokio::test]
    async fn spawn_falls_back_to_ephemeral_when_preferred_taken() {
        let squatter = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let taken = squatter.local_addr().unwrap().port();
        spawn(taken).await.expect("fallback bind should succeed");
        let bound = bound_video_port().expect("actual port recorded");
        assert_ne!(bound, taken, "must not claim the squatted port");
        let mut s = TcpStream::connect(("127.0.0.1", bound)).await.unwrap();
        s.write_all(b"GET /nosuchtoken HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut buf = [0u8; 128];
        let n = s.read(&mut buf).await.unwrap();
        let head = String::from_utf8_lossy(&buf[..n]);
        assert!(head.contains("404"), "unknown grant must 404, got: {head}");
    }
}

#[cfg(test)]
mod accept_loop_tests {
    use super::*;

    /// Decision 0031: a connection the owner check refuses gets no byte, though the handler would have answered.
    #[tokio::test]
    async fn a_connection_the_owner_check_refuses_gets_no_byte() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        spawn_accept_loop(listener, "test", |_, _, _| false, |mut s: TcpStream| async move {
            s.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await?;
            Ok(())
        });
        let mut c = TcpStream::connect(addr).await.unwrap();
        let _ = c.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await;
        let mut raw = Vec::new();
        // An Err from a reset counts as the end.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), c.read_to_end(&mut raw))
            .await
            .expect("the refused connection must close");
        assert!(raw.is_empty(), "got: {}", String::from_utf8_lossy(&raw));
    }

    #[tokio::test]
    async fn this_accounts_connection_is_served_through_the_real_check() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        spawn_accept_loop(listener, "test", sot_log::peer_owner::admit, handle_conn);
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(b"GET /nosuchtoken HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        let mut buf = [0u8; 128];
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), c.read(&mut buf))
            .await
            .expect("this account's connection must be answered")
            .unwrap();
        let head = String::from_utf8_lossy(&buf[..n]);
        assert!(head.starts_with("HTTP/1.1 404"), "got: {head}");
    }
}
