//! The request and response code both loopback servers share: request heads and persistent connections, content
//! types, single ranges, file bodies, plain replies. A connection is bounded on both sides: no whole request head within
//! `IDLE_BOUND`, or a reply write the client takes none of within `WRITE_BOUND`, closes it.

use std::path::Path;

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const READ_CHUNK: usize = 64 * 1024;

/// How long a connection waits for a request's whole head, from the wait's start: a new connection's first and a kept
/// connection's next. A head sent a byte at a time gets no longer than one sent whole. A remote
/// window reaches these servers through one ssh login per connection (ADR 0035, `pipe_one`), so a connection carries a
/// page's requests one after another, and one left idle ends here rather than when the browser drops it.
const IDLE_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// How long one write of a reply (its head, or up to `READ_CHUNK` bytes of its body) may take. It bounds a client that
/// stops reading, which would otherwise hold the task and a remote window's ssh login for as long as it keeps the
/// socket open. A client taking a chunk in 30 s (about 2 KiB/s) is never cut, however long the whole reply takes.
const WRITE_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// `bytes` on `stream`, ending in `TimedOut` when the client takes none of it within `WRITE_BOUND`.
async fn send<S: AsyncWrite + Unpin>(stream: &mut S, bytes: &[u8]) -> std::io::Result<()> {
    tokio::time::timeout(WRITE_BOUND, stream.write_all(bytes))
        .await
        .unwrap_or_else(|_| Err(std::io::ErrorKind::TimedOut.into()))
}

/// One request's head, as both servers read it.
pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) target: String,
    pub(crate) range: Option<String>,
    pub(crate) cookie: Option<String>,
    /// Whether the connection carries the next request after this one's reply: an HTTP/1.1 request that neither says
    /// `Connection: close` nor has a body, which neither server reads.
    pub(crate) keep: bool,
}

/// The next request head on `stream`. `buf` holds what was read past the previous head and keeps what is read past
/// this one. `None` when the client closes, sends no whole head within `IDLE_BOUND`, or sends a head past 16 KiB (answered 431).
pub(crate) async fn read_request<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    buf: &mut Vec<u8>,
) -> Result<Option<Request>> {
    let deadline = tokio::time::Instant::now() + IDLE_BOUND;
    let end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if buf.len() > 16 * 1024 {
            // The head is never parsed, so its method is not known; the connection closes after this reply.
            write_simple(
                stream,
                "GET",
                "431 Request Header Fields Too Large",
                "headers too large",
                false,
            )
            .await?;
            return Ok(None);
        }
        let mut tmp = [0u8; 1024];
        let Ok(read) = tokio::time::timeout_at(deadline, stream.read(&mut tmp)).await else {
            return Ok(None);
        };
        let n = read?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    buf.drain(..end);
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or("").split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let mut request = Request { method, target, range: None, cookie: None, keep: parts.next() == Some("HTTP/1.1") };
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        match name.as_str() {
            "range" => request.range = Some(value.to_string()),
            "cookie" => request.cookie = Some(value.to_string()),
            "connection" if value.split(',').any(|t| t.trim().eq_ignore_ascii_case("close")) => request.keep = false,
            "transfer-encoding" => request.keep = false,
            "content-length" if value != "0" => request.keep = false,
            _ => {}
        }
    }
    Ok(Some(request))
}

/// The `Connection` line of a reply: none while the connection stays open (HTTP/1.1's default), else `close`.
fn connection_line(keep: bool) -> &'static str {
    if keep {
        ""
    } else {
        "Connection: close\r\n"
    }
}

/// Content-Type by extension: the static-site table, with a video named as `video_mime` decides.
/// Shared with `pages::site`; falls back to a generic stream type so the
/// browser still treats an unknown file as opaque bytes. Serving a video type
/// is still gated by `is_servable_video`, so this table serves nothing new.
pub(crate) fn content_type(path: &Path) -> &'static str {
    if let Some(mime) = path.to_str().and_then(sot_protocol::video_path::video_mime) {
        return mime;
    }
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
        _ => "application/octet-stream",
    }
}

/// A plain-text reply. Returns `keep`: whether the connection carries the next request. A HEAD reply is the head
/// alone, with the Content-Length its body would have.
pub(crate) async fn write_simple<S: AsyncWrite + Unpin>(
    stream: &mut S,
    method: &str,
    status: &str,
    body: &str,
    keep: bool,
) -> Result<bool> {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n{}\r\n",
        body.len(),
        connection_line(keep)
    );
    send(stream, resp.as_bytes()).await?;
    if method != "HEAD" {
        send(stream, body.as_bytes()).await?;
    }
    stream.flush().await?;
    Ok(keep)
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

/// Send `file` on `stream` in answer to `request`: length and regular-file check come from fstat on
/// the open fd (never a second path lookup), a single `Range` is honoured, and
/// `Accept-Ranges: bytes` is always sent. `extra_headers` is zero or more
/// complete `Name: value\r\n` lines. A 0-byte file sends `Content-Length: 0`.
/// Returns whether the connection carries the next request: not when the request
/// says so, nor when the body could not be sent whole (the file shrank, the write failed).
pub(crate) async fn serve_file<S: AsyncWrite + Unpin>(
    stream: &mut S,
    request: &Request,
    mut file: tokio::fs::File,
    ctype: &str,
    extra_headers: &str,
) -> Result<bool> {
    let keep = request.keep;
    let total = match file.metadata().await {
        Ok(m) if m.is_file() => m.len(),
        _ => {
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
    let (start, len, partial) = match parse_range(request.range.as_deref(), total) {
        RangeReq::Full => (0, total, None),
        RangeReq::Part { start, end } => (start, end - start + 1, Some(end)),
        RangeReq::Unsatisfiable => {
            let resp = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\n{}\r\n",
                connection_line(keep)
            );
            send(stream, resp.as_bytes()).await?;
            stream.flush().await?;
            return Ok(keep);
        }
    };
    let status = if partial.is_some() { "206 Partial Content" } else { "200 OK" };
    let mut header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nAccept-Ranges: bytes\r\nContent-Length: {len}\r\n{extra_headers}{}",
        connection_line(keep)
    );
    if let Some(end) = partial {
        header.push_str(&format!("Content-Range: bytes {start}-{end}/{total}\r\n"));
    }
    header.push_str("\r\n");
    send(stream, header.as_bytes()).await?;

    if request.method == "HEAD" {
        stream.flush().await?;
        return Ok(keep);
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
        // (seeked away / paused), and a timeout that it stopped reading — not errors worth surfacing.
        if send(stream, &chunk[..n]).await.is_err() {
            return Ok(false);
        }
        remaining -= n as u64;
    }
    stream.flush().await.ok();
    Ok(keep && remaining == 0)
}

#[cfg(test)]
#[path = "http_range_tests.rs"]
mod range_tests;

#[cfg(test)]
#[path = "http_keep_tests.rs"]
mod keep_tests;
