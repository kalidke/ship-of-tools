// codec.rs — async NDJSON + length-prefixed-blob framing.
//
// `read_frame` consumes one `\n`-terminated JSON envelope and, if its
// `payload.blob` field is present, the next `len` bytes — returned together
// so callers never have to reason about the binary tail separately.
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
/// fatal: `read_frame` has already consumed the line but cannot know whether a
/// blob it failed to parse a descriptor for is still queued behind it, so the
/// read stream's position is not trustworthy.
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

pub async fn read_frame<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<(Frame, Option<Vec<u8>>)> {
    let mut line = Vec::with_capacity(256);
    let n = r
        .read_until(b'\n', &mut line)
        .await
        .context("read envelope")?;
    if n == 0 {
        return Err(anyhow!("eof"));
    }
    if line.len() > MAX_ENVELOPE_BYTES {
        return Err(anyhow!(
            "envelope is {} bytes; cap is {}",
            line.len(),
            MAX_ENVELOPE_BYTES
        ));
    }
    if line.ends_with(b"\n") {
        line.pop();
    }
    let frame: Frame = match serde_json::from_slice(&line) {
        Ok(f) => f,
        Err(e) => {
            // Diagnostic: include the head of the bytes we choked on so
            // codec desyncs (the classic "blob shadowed envelope" failure)
            // are debuggable from the log instead of by guessing. Cap the
            // preview so a 1 MiB envelope doesn't blow up tracing.
            let preview_len = line.len().min(160);
            let preview = String::from_utf8_lossy(&line[..preview_len]);
            return Err(anyhow!(
                "frame parse failed: {e} | len={} head={:?}",
                line.len(),
                preview
            ));
        }
    };

    // Codec inspects the payload for a blob descriptor so callers don't need
    // to special-case ops that carry blobs.
    let blob_len = frame
        .payload
        .as_object()
        .and_then(|m| m.get("blob"))
        .and_then(|b| b.get("len"))
        .and_then(|l| l.as_u64());

    let blob = if let Some(len) = blob_len {
        let mut buf = vec![0u8; len as usize];
        AsyncReadExt::read_exact(r, &mut buf)
            .await
            .context("read blob")?;
        Some(buf)
    } else {
        None
    };

    Ok((frame, blob))
}

/// Convenience: feed an `AsyncRead` (e.g. one half of a tokio Unix socket
/// split) into `read_frame` without the caller wrapping a `BufReader` each
/// time.
pub fn buffered<R: AsyncRead + Unpin>(r: R) -> tokio::io::BufReader<R> {
    tokio::io::BufReader::new(r)
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
}
