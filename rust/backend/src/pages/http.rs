//! The response code both loopback servers share: content types, single ranges, file bodies, plain replies.

use std::path::Path;

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const READ_CHUNK: usize = 64 * 1024;

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

pub(crate) async fn write_simple(stream: &mut TcpStream, status: &str, body: &str) -> Result<()> {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
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
    use super::super::video::{handle_conn, register_video};
    use super::*;
    use tokio::net::TcpListener;

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
