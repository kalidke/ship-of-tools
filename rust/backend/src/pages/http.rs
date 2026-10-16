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
            write_simple(stream, "431 Request Header Fields Too Large", "headers too large", false).await?;
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

/// A plain-text reply. Returns `keep`: whether the connection carries the next request.
pub(crate) async fn write_simple<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: &str,
    body: &str,
    keep: bool,
) -> Result<bool> {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n{}\r\n{body}",
        body.len(),
        connection_line(keep)
    );
    send(stream, resp.as_bytes()).await?;
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
        _ => return write_simple(stream, "404 Not Found", "no such file", keep).await,
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
mod range_tests {
    use super::super::video::{handle_conn, register_video};
    use super::*;
    use tokio::net::{TcpListener, TcpStream};

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
mod keep_tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::duplex;

    /// The request heads that decide whether a connection is kept, and what each decides.
    #[tokio::test]
    async fn a_request_keeps_its_connection_only_when_it_may() {
        let cases = [
            ("GET / HTTP/1.1\r\n\r\n", true),
            ("GET / HTTP/1.1\r\nContent-Length: 0\r\n\r\n", true),
            ("GET / HTTP/1.0\r\n\r\n", false),
            ("GET / HTTP/1.1\r\nConnection: close\r\n\r\n", false),
            ("GET / HTTP/1.1\r\nconnection: Keep-Alive, Close\r\n\r\n", false),
            ("GET / HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello", false),
            ("GET / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n", false),
        ];
        for (head, keep) in cases {
            let (mut client, mut server) = duplex(4096);
            client.write_all(head.as_bytes()).await.unwrap();
            let request = read_request(&mut server, &mut Vec::new()).await.unwrap().expect("a request");
            assert_eq!(request.keep, keep, "{head:?}");
        }
    }

    /// Two requests sent at once are read one after the other from the same buffer.
    #[tokio::test]
    async fn pipelined_requests_are_read_in_turn() {
        let (mut client, mut server) = duplex(4096);
        client.write_all(b"GET /a HTTP/1.1\r\n\r\nGET /b HTTP/1.1\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        let first = read_request(&mut server, &mut buf).await.unwrap().unwrap();
        let second = read_request(&mut server, &mut buf).await.unwrap().unwrap();
        assert_eq!((first.target.as_str(), first.keep), ("/a", true));
        assert_eq!((second.target.as_str(), second.keep), ("/b", false));
    }

    fn get(keep: bool) -> Request {
        Request { method: "GET".into(), target: "/".into(), range: None, cookie: None, keep }
    }

    /// A file of a megabyte served through a 1 KiB pipe: the file is read at most a chunk ahead of the client.
    async fn big_file() -> (std::path::PathBuf, tokio::fs::File) {
        let path = std::env::temp_dir().join(format!("sot-keep-test-{}", sot_updater::unique::suffix()));
        std::fs::write(&path, vec![7u8; 1 << 20]).unwrap();
        let file = tokio::fs::File::open(&path).await.unwrap();
        (path, file)
    }

    /// Read from `client` up to the end of the reply's head.
    async fn read_head(client: &mut tokio::io::DuplexStream) {
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(client.read(&mut b).await.unwrap(), 1);
            head.push(b[0]);
        }
    }

    /// A reply kept open is whole; one that could not be sent whole (the file shrank, the client left) ends the
    /// connection, since the client would read the next reply as the rest of its body.
    #[tokio::test]
    async fn a_reply_keeps_its_connection_only_when_sent_whole() {
        let timeout = Duration::from_secs(10);
        // Whole.
        let (path, file) = big_file().await;
        let (mut client, mut server) = duplex(1024);
        let sent = tokio::spawn(async move { serve_file(&mut server, &get(true), file, "text/plain", "").await.unwrap() });
        let mut raw = Vec::new();
        tokio::time::timeout(timeout, async {
            read_head(&mut client).await;
            let mut body = vec![0; 1 << 20];
            client.read_exact(&mut body).await.unwrap();
            raw = body;
        })
        .await
        .unwrap();
        assert!(raw.iter().all(|&b| b == 7));
        assert!(tokio::time::timeout(timeout, sent).await.unwrap().unwrap(), "a whole reply keeps the connection");
        // Short: the file shrinks after its head is sent.
        let (shrinking, file) = big_file().await;
        let (mut client, mut server) = duplex(1024);
        let sent = tokio::spawn(async move { serve_file(&mut server, &get(true), file, "text/plain", "").await.unwrap() });
        read_head(&mut client).await;
        std::fs::write(&shrinking, b"").unwrap();
        let mut rest = Vec::new();
        tokio::time::timeout(timeout, client.read_to_end(&mut rest)).await.unwrap().unwrap();
        assert!(!tokio::time::timeout(timeout, sent).await.unwrap().unwrap(), "a short reply closes the connection");
        // Failed: the client leaves after the head.
        let (failing, file) = big_file().await;
        let (mut client, mut server) = duplex(1024);
        let sent = tokio::spawn(async move { serve_file(&mut server, &get(true), file, "text/plain", "").await.unwrap() });
        read_head(&mut client).await;
        drop(client);
        assert!(!tokio::time::timeout(timeout, sent).await.unwrap().unwrap(), "a failed reply closes the connection");
        for path in [path, shrinking, failing] {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The bound is one deadline for a whole head: one sent a line every 10 s is closed 30 s after the wait began,
    /// not 30 s after its last byte.
    #[tokio::test(start_paused = true)]
    async fn a_head_dripped_past_the_bound_is_closed_at_the_bound() {
        let (mut client, mut server) = duplex(64 * 1024);
        tokio::spawn(async move {
            let _ = client.write_all(b"GET / HTTP/1.1\r\n").await;
            for i in 0u32.. {
                tokio::time::sleep(Duration::from_secs(10)).await;
                if client.write_all(format!("X-Drip-{i}: y\r\n").as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let start = tokio::time::Instant::now();
        assert!(read_request(&mut server, &mut Vec::new()).await.unwrap().is_none());
        let waited = start.elapsed();
        assert!(waited >= IDLE_BOUND && waited < IDLE_BOUND + Duration::from_secs(5), "closed after {waited:?}");
    }

    /// A client that stops reading mid-reply is cut after `WRITE_BOUND`, and the connection is not kept; one that
    /// keeps taking a chunk every 20 s gets the whole reply.
    #[tokio::test(start_paused = true)]
    async fn a_client_that_stops_reading_is_cut_at_the_write_bound() {
        let (path, file) = big_file().await;
        let (mut client, mut server) = duplex(1024);
        let sent = tokio::spawn(async move { serve_file(&mut server, &get(true), file, "text/plain", "").await.unwrap() });
        read_head(&mut client).await;
        let start = tokio::time::Instant::now();
        let kept = tokio::time::timeout(WRITE_BOUND + Duration::from_secs(5), sent).await.expect("cut at the bound").unwrap();
        assert!(!kept);
        assert!(start.elapsed() >= WRITE_BOUND, "cut after {:?}", start.elapsed());
        drop(client);
        let (_, file) = big_file().await;
        let (mut client, mut server) = duplex(READ_CHUNK);
        let sent = tokio::spawn(async move { serve_file(&mut server, &get(true), file, "text/plain", "").await.unwrap() });
        read_head(&mut client).await;
        let mut got = 0;
        let mut buf = vec![0u8; 64 * 1024];
        while got < 1 << 20 {
            tokio::time::sleep(Duration::from_secs(20)).await;
            got += client.read(&mut buf).await.unwrap();
        }
        assert!(sent.await.unwrap(), "a slow client that keeps reading gets the whole reply");
        let _ = std::fs::remove_file(path);
    }
}
