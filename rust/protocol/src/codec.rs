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

const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;

pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    frame: &Frame,
    blob: Option<&[u8]>,
) -> Result<()> {
    let mut line = serde_json::to_vec(frame).context("frame serialize failed")?;
    if line.len() > MAX_ENVELOPE_BYTES {
        return Err(anyhow!(
            "frame envelope is {} bytes; cap is {}",
            line.len(),
            MAX_ENVELOPE_BYTES
        ));
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
    use super::{read_frame, write_frame};
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
}
