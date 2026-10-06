// codec.rs — async NDJSON + length-prefixed-blob framing.
//
// `read_frame` consumes one `\n`-terminated JSON envelope and, if its
// `payload.blob` field is present, the next `len` bytes — returned together
// so callers never have to reason about the binary tail separately.
//
// `read_envelope` reads the envelope alone, for a reader that must not read a
// blob it has not admitted.
//
// `write_frame` does the inverse: serialize the envelope, append `\n`,
// optionally append the blob bytes, flush.
//
// We cap envelopes at 1 MiB because the Frame payload is meant for control
// data; bulk content rides through the blob path.

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::Frame;

pub const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;

/// A frame whose serialized envelope exceeded [`MAX_ENVELOPE_BYTES`].
///
/// Carried as a distinct error type — not just an `anyhow!` string — so callers
/// can `downcast_ref` and tell "this one frame was too big" apart from "the
/// socket is broken". That distinction matters because [`write_frame`] checks
/// the size **before** writing any bytes: on rejection nothing reached the wire
/// and the stream is still perfectly consistent, so the right response is an
/// error frame on the same connection, never a teardown. A genuine mid-write
/// failure has no such guarantee and must stay fatal.
///
/// Only the write path produces this. An over-cap *inbound* envelope stays
/// fatal: `read_frame` stops one byte past the cap, partway through the line
/// and before any blob behind it, so the read stream's position is not
/// trustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeTooLarge {
    pub len: usize,
    pub cap: usize,
}

impl std::fmt::Display for EnvelopeTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "frame envelope is {} bytes; cap is {}", self.len, self.cap)
    }
}

impl std::error::Error for EnvelopeTooLarge {}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    frame: &Frame,
    blob: Option<&[u8]>,
) -> Result<()> {
    let mut line = serde_json::to_vec(frame).context("frame serialize failed")?;
    if line.len() > MAX_ENVELOPE_BYTES {
        // Nothing has been written yet — see `EnvelopeTooLarge`.
        return Err(anyhow::Error::new(EnvelopeTooLarge {
            len: line.len(),
            cap: MAX_ENVELOPE_BYTES,
        }));
    }
    line.push(b'\n');
    w.write_all(&line).await.context("write envelope")?;
    if let Some(b) = blob {
        w.write_all(b).await.context("write blob")?;
    }
    w.flush().await.context("flush")?;
    Ok(())
}

/// What may be said of a line that does not parse as JSON: serde's error category and position and the line's
/// length, never its bytes. A line can carry a page's secret (a `pluto.open` reply cut off by a dying link, a REPL
/// announcement cut off after a `wglshow` URL), and these descriptions reach logs and the frontend's status line
/// (ADR 0049, User isolation). serde's own message can quote input, so only its category and position are kept.
pub fn unparsed(e: &serde_json::Error, len: usize) -> String {
    format!("{:?} error at line {} column {} | len={len}", e.classify(), e.line(), e.column())
}

/// One envelope, read with the cap checked while it is read, then parsed; no blob tail is read. The daemon reads a
/// connection's first frame this way (a hello carries no blob); every other reader uses `read_frame`.
pub async fn read_envelope<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Frame> {
    // At most the cap and one byte more are read, room for the newline: the cap counts an envelope without its
    // newline, as the writers do, and a peer that sends no newline is refused there, never read to its end.
    let mut line = Vec::with_capacity(256);
    let n = AsyncReadExt::take(&mut *r, MAX_ENVELOPE_BYTES as u64 + 1)
        .read_until(b'\n', &mut line)
        .await
        .context("read envelope")?;
    if n == 0 {
        return Err(anyhow!("eof"));
    }
    if line.ends_with(b"\n") {
        line.pop();
    }
    if line.len() > MAX_ENVELOPE_BYTES {
        return Err(anyhow!("envelope exceeds {} bytes", MAX_ENVELOPE_BYTES));
    }
    serde_json::from_slice(&line).map_err(|e| anyhow!("frame parse failed: {}", unparsed(&e, line.len())))
}

/// The blob a frame's payload declares (`payload.blob.len`): that many raw bytes follow its envelope on the wire.
pub fn declared_blob_len(frame: &Frame) -> Option<u64> {
    frame.payload.as_object().and_then(|m| m.get("blob")).and_then(|b| b.get("len")).and_then(serde_json::Value::as_u64)
}

pub async fn read_frame<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<(Frame, Option<Vec<u8>>)> {
    let frame = read_envelope(r).await?;
    let Some(len) = declared_blob_len(&frame) else {
        return Ok((frame, None));
    };
    // The buffer grows only with bytes that arrived, from at most one envelope's size: a declared length is never
    // memory reserved on the peer's word.
    let mut blob = Vec::with_capacity(len.min(MAX_ENVELOPE_BYTES as u64) as usize);
    AsyncReadExt::take(&mut *r, len).read_to_end(&mut blob).await.context("read blob")?;
    if (blob.len() as u64) < len {
        return Err(anyhow!("blob ended after {} of {} bytes", blob.len(), len));
    }
    Ok((frame, Some(blob)))
}

/// Convenience: feed an `AsyncRead` (e.g. one half of a tokio Unix socket
/// split) into `read_frame` without the caller wrapping a `BufReader` each
/// time.
pub fn buffered<R: AsyncRead + Unpin>(r: R) -> tokio::io::BufReader<R> {
    tokio::io::BufReader::new(r)
}

/// The blocking twin of [`write_frame`], for a connection with no Tokio
/// runtime behind it — ADR 0045 decision 3's lane-bridge dial
/// (`sot-protocol::topology::lane_client`) runs on a plain blocking thread, exactly
/// like the rest of `sot-log`'s own client machinery it composes with.
/// One `\n`-terminated JSON envelope, same [`MAX_ENVELOPE_BYTES`] cap as
/// the async path — NO blob support: `lane.connect`'s request/response
/// pair never carries one (see [`crate::ops::LaneConnectReq`]'s own
/// doc), so this never looks for a `payload.blob` descriptor the way
/// [`read_frame`] does.
pub fn write_frame_blocking<W: std::io::Write>(w: &mut W, frame: &Frame) -> Result<()> {
    let mut line = serde_json::to_vec(frame).context("frame serialize failed")?;
    if line.len() > MAX_ENVELOPE_BYTES {
        // Nothing has been written yet — see `EnvelopeTooLarge`.
        return Err(anyhow::Error::new(EnvelopeTooLarge {
            len: line.len(),
            cap: MAX_ENVELOPE_BYTES,
        }));
    }
    line.push(b'\n');
    w.write_all(&line).context("write envelope")?;
    w.flush().context("flush")?;
    Ok(())
}

/// The blocking twin of [`read_frame`] — see [`write_frame_blocking`]'s
/// own doc for why this exists and why it never reads a blob tail.
pub fn read_frame_blocking<R: std::io::BufRead>(r: &mut R) -> Result<Frame> {
    // Capped DURING the read, not after: `read_until` itself has no
    // limit, so accumulating a whole `\n`-terminated line first and only
    // THEN checking its length would let a peer that never sends `\n`
    // grow `line` without bound before this function ever gets to
    // refuse it (ADR 0045 lane B4a Codex review blocker). `fill_buf`/
    // `consume` reads in the underlying `BufRead`'s own chunk sizes,
    // scanning each chunk for the terminator and checking the running
    // total against the cap before ever asking for more.
    let mut line = Vec::with_capacity(256);
    loop {
        let chunk = r.fill_buf().context("read envelope")?;
        if chunk.is_empty() {
            return Err(anyhow!(
                "eof after {} byte(s) with no terminating newline",
                line.len()
            ));
        }
        if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
            line.extend_from_slice(&chunk[..=pos]);
            r.consume(pos + 1);
            break;
        }
        line.extend_from_slice(chunk);
        let consumed = chunk.len();
        r.consume(consumed);
        if line.len() > MAX_ENVELOPE_BYTES {
            return Err(anyhow!(
                "envelope exceeds {} bytes before a terminating newline arrived",
                MAX_ENVELOPE_BYTES
            ));
        }
    }
    if line.ends_with(b"\n") {
        line.pop();
    }
    // The cap counts an envelope without its newline, as the writers do.
    if line.len() > MAX_ENVELOPE_BYTES {
        return Err(anyhow!(
            "envelope is {} bytes; cap is {}",
            line.len(),
            MAX_ENVELOPE_BYTES
        ));
    }
    let frame: Frame = match serde_json::from_slice(&line) {
        Ok(f) => f,
        Err(e) => return Err(anyhow!("frame parse failed: {}", unparsed(&e, line.len()))),
    };
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::{read_frame, write_frame, EnvelopeTooLarge, MAX_ENVELOPE_BYTES};
    use crate::ops::FileChunk;
    use crate::ir::BlobDescriptor;
    use crate::Frame;

    // Regression: a streamed file.download FileChunk MUST carry a `blob`
    // descriptor, or read_frame won't consume the appended bytes and the next
    // frame desyncs onto raw file data (the 2026-05-28 download bug). This
    // round-trips two frames where the first has a trailing blob whose bytes
    // happen to look like JSON garbage, and asserts the second frame still
    // parses cleanly + the blob came back intact.
    #[tokio::test]
    async fn file_chunk_blob_is_consumed_no_desync() {
        let mut wire: Vec<u8> = Vec::new();
        let payload = b"ftypisom....mdat raw bytes that are NOT json {{{"; // would break a JSON parse
        let chunk = FileChunk {
            offset: 0,
            total: payload.len() as u64,
            eof: true,
            blob: BlobDescriptor { len: payload.len() as u64, mime: "application/octet-stream".into() },
        };
        let f1 = Frame::res(7, "file.download", serde_json::to_value(&chunk).unwrap());
        write_frame(&mut wire, &f1, Some(payload)).await.unwrap();
        // A second, ordinary frame right after — this is what desynced before.
        let f2 = Frame::res(8, "file.upload", serde_json::json!({"offset": 0, "done": true}));
        write_frame(&mut wire, &f2, None).await.unwrap();

        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(wire));
        let (g1, blob1) = read_frame(&mut r).await.unwrap();
        assert_eq!(g1.id, 7);
        assert_eq!(blob1.as_deref(), Some(&payload[..]), "blob bytes must round-trip");
        let (g2, blob2) = read_frame(&mut r).await.unwrap();
        assert_eq!(g2.id, 8, "second frame must parse — no desync onto raw bytes");
        assert!(blob2.is_none());
    }

    // Regression: quarto.open's `--embed-resources` HTML must ride the blob
    // path. It used to be base64'd into the envelope, which blew the 1 MiB cap
    // and killed the connection. This uses HTML larger than the cap — which is
    // the whole point, it could not travel in the envelope at all — and asserts
    // it round-trips and that a following frame still parses.
    #[tokio::test]
    async fn quarto_html_rides_blob_path_over_envelope_cap() {
        let mut wire: Vec<u8> = Vec::new();
        // Deliberately > MAX_ENVELOPE_BYTES, and full of JSON-hostile bytes so a
        // desync would corrupt the next parse rather than silently pass.
        let html = format!(
            "<!doctype html><html><body>{}</body></html>",
            "{\"not\":json}\n".repeat(90_000)
        )
        .into_bytes();
        assert!(
            html.len() > MAX_ENVELOPE_BYTES,
            "fixture must exceed the envelope cap to be meaningful"
        );
        let res = crate::ops::QuartoOpenRes {
            blob: BlobDescriptor {
                len: html.len() as u64,
                mime: "text/html".into(),
            },
        };
        let payload = serde_json::to_value(&res).unwrap();
        // No `html_base64` on the wire: receivers still carrying the legacy
        // raw-JSON arm gate on its presence and must fall through to the blob.
        assert!(
            payload.get("html_base64").is_none(),
            "legacy html_base64 must not appear in the blob-path payload"
        );
        let f1 = Frame::res(3, "quarto.open", payload);
        write_frame(&mut wire, &f1, Some(&html))
            .await
            .expect("envelope is small — only the blob is large");
        let f2 = Frame::res(4, "tree.root", serde_json::json!({"ok": true}));
        write_frame(&mut wire, &f2, None).await.unwrap();

        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(wire));
        let (g1, blob1) = read_frame(&mut r).await.unwrap();
        assert_eq!(g1.id, 3);
        assert_eq!(blob1.as_deref(), Some(&html[..]), "HTML must round-trip");
        let (g2, _) = read_frame(&mut r).await.unwrap();
        assert_eq!(g2.id, 4, "next frame must parse — no desync onto raw HTML");
    }

    // An over-cap envelope must be reported as `EnvelopeTooLarge` (so callers
    // can contain it instead of dropping the connection) AND must leave the
    // sink completely untouched — that untouched-sink guarantee is exactly what
    // makes containment safe.
    #[tokio::test]
    async fn oversize_envelope_is_typed_and_writes_nothing() {
        let mut wire: Vec<u8> = Vec::new();
        let huge = "x".repeat(MAX_ENVELOPE_BYTES + 1);
        let f = Frame::res(9, "quarto.open", serde_json::json!({ "html": huge }));
        let err = write_frame(&mut wire, &f, None)
            .await
            .expect_err("envelope exceeds the cap");
        let typed = err
            .downcast_ref::<EnvelopeTooLarge>()
            .expect("must be downcastable, not a bare string — server.rs matches on the type");
        assert!(typed.len > typed.cap);
        assert_eq!(typed.cap, MAX_ENVELOPE_BYTES);
        assert!(
            wire.is_empty(),
            "nothing may reach the wire — containment depends on it"
        );
    }

    // Version-skew seam (backend/frontend paired across a release boundary,
    // e.g. new frontends against an old daemon or vice versa): `rev` is
    // `Option<u64>` with `#[serde(default, skip_serializing_if =
    // "Option::is_none")]` (see `Frame`'s own doc) — omitted from the wire
    // when absent, and deserializes to `None` when the key is missing at
    // all. That combination is what lets EITHER side omit `rev` (a NEW
    // daemon's `preview.changed`, now deliberately outside the session
    // ring — see `watcher.rs`) without the OTHER side choking: nothing
    // about `rev`'s shape changed for this fix, only whether a given event
    // ever gets one stamped.
    #[tokio::test]
    async fn rev_omitted_round_trips_to_none() {
        let mut wire: Vec<u8> = Vec::new();
        // `Frame::evt` leaves `rev: None`; `write_frame` then OMITS the key
        // entirely (skip_serializing_if) — this is exactly what a
        // `preview.changed` frame looks like on the wire post-fix.
        let f = Frame::evt("preview.changed", serde_json::json!({"path": "/a/b"}));
        write_frame(&mut wire, &f, None).await.unwrap();
        assert!(
            !String::from_utf8_lossy(&wire).contains("\"rev\""),
            "rev must be OMITTED, not sent as null, when absent"
        );
        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(wire));
        let (parsed, _) = read_frame(&mut r).await.unwrap();
        assert_eq!(
            parsed.rev, None,
            "a frame with no `rev` key must deserialize to None, not error \
             (an OLD frontend parsing a NEW daemon's un-stamped preview.changed \
             depends on exactly this)"
        );
    }

    // The reverse pairing: an OLD daemon still stamps `rev` on every
    // preview.changed (it never adopted this fix). A NEW frontend must
    // parse that unchanged and simply carry the value — nothing downstream
    // treats an unexpectedly-present `rev` as an error.
    #[tokio::test]
    async fn rev_present_round_trips_and_is_not_rejected() {
        let mut wire: Vec<u8> = Vec::new();
        let f = Frame::evt("preview.changed", serde_json::json!({"path": "/a/b"})).with_rev(42);
        write_frame(&mut wire, &f, None).await.unwrap();
        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(wire));
        let (parsed, _) = read_frame(&mut r).await.unwrap();
        assert_eq!(parsed.rev, Some(42));
    }

    /// ADR 0049, User isolation: a frame cut off mid-envelope (an ssh link dying during a `pluto.open` or `video.open`
    /// reply) must not put any of the page's secret into the parse error, which reaches the frontend's log and
    /// status line; this holds at every cut, a token's first characters included.
    #[tokio::test]
    async fn a_truncated_frame_error_carries_no_frame_bytes() {
        let whole: &[u8] = br#"{"v":2,"id":1,"kind":"res","op":"video.open","rev":1,"payload":{"url":"http://127.0.0.1:41234/0123456789abcdef0123456789abcdef"}}"#;
        let token_at = whole.windows(4).position(|w| w == b"0123").unwrap();
        for cut in [token_at + 5, token_at + 12, token_at + 31] {
            let mut cut = whole[..cut].to_vec();
            cut.push(b'\n'); // a relay that cuts an envelope short and ends the line there
            let mut r = tokio::io::BufReader::new(std::io::Cursor::new(cut.clone()));
            let e = read_frame(&mut r).await.unwrap_err().to_string();
            assert!(!e.contains("0123") && !e.contains("video.open") && !e.contains("head="), "{e}");
            let e = super::read_frame_blocking(&mut std::io::Cursor::new(cut)).unwrap_err().to_string();
            assert!(!e.contains("0123") && !e.contains("video.open") && !e.contains("head="), "{e}");
            assert!(e.contains("len="), "{e}");
        }
    }

    /// A peer that sends no newline is refused one byte past the cap, never read to its end: the daemon reads a
    /// connection's first frame before it admits anything (ADR 0049 `## User isolation`), and `read_until` used to take
    /// the whole line before the check (review round 2, OLD).
    #[tokio::test]
    async fn an_envelope_with_no_newline_is_refused_at_the_cap() {
        struct Counted(std::io::Cursor<Vec<u8>>, std::sync::Arc<std::sync::atomic::AtomicUsize>);
        impl tokio::io::AsyncRead for Counted {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                let before = buf.filled().len();
                let polled = tokio::io::AsyncRead::poll_read(std::pin::Pin::new(&mut self.0), cx, buf);
                self.1.fetch_add(buf.filled().len() - before, std::sync::atomic::Ordering::SeqCst);
                polled
            }
        }
        let taken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let source = Counted(std::io::Cursor::new(vec![b'x'; 3 * MAX_ENVELOPE_BYTES]), taken.clone());
        let mut r = tokio::io::BufReader::with_capacity(8192, source);
        let err = read_frame(&mut r).await.expect_err("an envelope with no newline is refused");
        let taken = taken.load(std::sync::atomic::Ordering::SeqCst);
        assert!(taken <= MAX_ENVELOPE_BYTES + 1 + 8192, "read {taken} bytes of an envelope with no newline: {err:#}");
    }

    /// A blob's length is what the peer declares, not what it sends: the buffer grows only with bytes that arrived, so
    /// a frame that declares a pebibyte and sends ten bytes is refused at its end instead of reserving the pebibyte
    /// (review round 2, OLD: `vec![0u8; len]` aborted the process on an allocation no machine has).
    #[tokio::test]
    async fn a_declared_blob_length_reserves_nothing() {
        let frame = Frame::req(1, "x", serde_json::json!({ "blob": { "len": 1u64 << 50 } }));
        let mut wire = serde_json::to_vec(&frame).unwrap();
        wire.push(b'\n');
        wire.extend_from_slice(b"ten bytes!");
        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(wire));
        let err = read_frame(&mut r).await.expect_err("a blob that ends early is refused");
        assert!(format!("{err:#}").contains("blob ended after 10 of 1125899906842624 bytes"), "{err:#}");
    }

    /// The cap counts an envelope without its newline, in the writers and the readers alike: an envelope whose JSON is
    /// exactly the cap is written and read by both pairs (review round 2, NOTE D: the readers counted the newline and
    /// refused what the writers had written).
    #[tokio::test]
    async fn an_envelope_at_the_cap_is_written_and_read_by_both_pairs() {
        let mut frame = Frame::req(1, "x", serde_json::json!({ "pad": "" }));
        let base = serde_json::to_vec(&frame).unwrap().len();
        frame.payload = serde_json::json!({ "pad": "x".repeat(MAX_ENVELOPE_BYTES - base) });
        assert_eq!(serde_json::to_vec(&frame).unwrap().len(), MAX_ENVELOPE_BYTES);
        let mut wire = Vec::new();
        write_frame(&mut wire, &frame, None).await.expect("the writer takes an envelope at the cap");
        let mut r = tokio::io::BufReader::new(std::io::Cursor::new(wire));
        read_frame(&mut r).await.expect("read_frame reads what write_frame wrote");
        let mut blocking = Vec::new();
        super::write_frame_blocking(&mut blocking, &frame).expect("the blocking writer takes it too");
        super::read_frame_blocking(&mut std::io::Cursor::new(blocking)).expect("read_frame_blocking reads it");
    }
}
