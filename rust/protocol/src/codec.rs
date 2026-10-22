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
    // The payload's declared blob and the bytes given must agree, or a reader takes the next envelope for blob bytes.
    let declared = declared_blob_len(frame);
    let given = blob.map(|b| b.len() as u64);
    if declared != given {
        return Err(anyhow!("frame declares a blob of {declared:?} bytes but was given {given:?}; nothing written"));
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

/// The most a reader reserves up front for a blob on the peer's declared length; the buffer grows with bytes that arrive.
pub(crate) const BLOB_RESERVE_BOUND: usize = 64 * 1024;

pub(crate) fn blob_reserve(declared: u64) -> usize {
    declared.min(BLOB_RESERVE_BOUND as u64) as usize
}

pub async fn read_frame<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<(Frame, Option<Vec<u8>>)> {
    let frame = read_envelope(r).await?;
    let Some(len) = declared_blob_len(&frame) else {
        return Ok((frame, None));
    };
    // The buffer grows only with bytes that arrived, from at most `BLOB_RESERVE_BOUND`: a declared length is never
    // memory reserved on the peer's word.
    let mut blob = Vec::with_capacity(blob_reserve(len));
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
#[path = "codec_tests.rs"]
mod tests;
