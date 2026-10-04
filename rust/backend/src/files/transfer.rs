//! File transfer: `file.download` streamed in chunks to the connection, and `file.upload`.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::BlobDescriptor;
use sot_protocol::FileChunk;
use sot_protocol::FileDownloadReq;
use sot_protocol::FileUploadAck;
use sot_protocol::FileUploadReq;
use sot_protocol::Frame;
use crate::handlers::HandlerOutput;

/// `file.download` — stream a backend-host file to the frontend in <=1 MiB
/// `FileChunk` frames (bytes as each frame's trailing blob), all sharing
/// `req_id`; the `eof` frame carries the last chunk. Reads any path the backend
/// can read (matches `preview.get` reach — files outside the project root are
/// fine). Writes straight to the connection's outbound `tx`, so memory stays
/// bounded to one chunk regardless of file size. On open/read failure, sends a
/// single `{error, code}` frame instead.
pub async fn stream_file_download<W>(
    tx: &mut W,
    req_id: u64,
    payload_json: serde_json::Value,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncReadExt;
    const CHUNK: usize = 1024 * 1024;

    // Parse failure answers with an error frame (no chunks written yet, so
    // the response shape is unambiguous) — connection containment happens
    // before streaming starts. Mid-stream read errors below stay
    // connection-fatal on purpose: chunks are already on the wire and a
    // shape-switch mid-stream would leave the frontend's downloader hanging.
    let req: FileDownloadReq = match serde_json::from_value(payload_json) {
        Ok(r) => r,
        Err(e) => {
            let f = Frame::res(
                req_id,
                op::FILE_DOWNLOAD,
                json!({ "error": format!("file.download payload: {e}"), "code": "bad_request" }),
            );
            sot_protocol::write_frame(tx, &f, None).await?;
            return Ok(());
        }
    };
    tracing::info!(path = %req.path, "file.download");

    let path = std::path::Path::new(&req.path);
    let total = match tokio::fs::metadata(path).await {
        Ok(m) if m.is_file() => m.len(),
        _ => {
            let f = Frame::res(
                req_id,
                op::FILE_DOWNLOAD,
                json!({ "error": format!("no such file: {}", req.path), "code": "io_error" }),
            );
            sot_protocol::write_frame(tx, &f, None).await?;
            return Ok(());
        }
    };
    let mut file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            let f = Frame::res(
                req_id,
                op::FILE_DOWNLOAD,
                json!({ "error": format!("open failed: {e}"), "code": "io_error" }),
            );
            sot_protocol::write_frame(tx, &f, None).await?;
            return Ok(());
        }
    };

    let mut offset: u64 = 0;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buf).await.context("file.download read")?;
        let eof = n == 0 || offset + n as u64 >= total;
        // The `blob` descriptor is REQUIRED: codec::read_frame only consumes
        // the appended bytes when `payload.blob.len` is present. Without it the
        // frontend skips this chunk's bytes and parses raw file data as the
        // next envelope → desync → reconnect loop. (Mirrors preview.get.)
        let chunk = FileChunk {
            offset,
            total,
            eof,
            blob: BlobDescriptor {
                len: n as u64,
                mime: "application/octet-stream".to_string(),
            },
        };
        let frame = Frame::res(req_id, op::FILE_DOWNLOAD, serde_json::to_value(&chunk)?);
        sot_protocol::write_frame(tx, &frame, Some(&buf[..n])).await?;
        offset += n as u64;
        if eof {
            break;
        }
    }
    Ok(())
}

/// `file.upload` — write one uploaded chunk into the cursored backend directory.
/// Stateless per chunk: on `offset == 0` it sanitizes `name` to a plain
/// basename (rejecting `/`, `\`, `.`, `..` so it can't escape `dir`),
/// de-duplicates against existing files with a ` (1)` suffix, and creates +
/// truncates the file; later chunks (which carry the resolved name back from
/// the ack) open it and write at `offset`. Acks each chunk, returning the
/// resolved `final_name` on the first and final chunks.
pub async fn handle_file_upload(
    req_id: u64,
    payload_json: serde_json::Value,
) -> Result<HandlerOutput> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    let req: FileUploadReq = serde_json::from_value(payload_json).context("file.upload payload")?;

    let err = |msg: String, code: &str| -> Result<HandlerOutput> {
        Ok(vec![(
            Frame::res(
                req_id,
                op::FILE_UPLOAD,
                json!({ "error": msg, "code": code }),
            ),
            None,
        )])
    };

    let name = req.name.trim();
    if !is_safe_upload_name(name) {
        return err(format!("unsafe upload name: {:?}", req.name), "bad_name");
    }
    let dir = std::path::Path::new(&req.dir);
    match tokio::fs::metadata(dir).await {
        Ok(m) if m.is_dir() => {}
        _ => return err(format!("upload dir not found: {}", req.dir), "no_dir"),
    }
    let bytes = match STANDARD.decode(req.data_b64.as_bytes()) {
        Ok(b) => b,
        Err(e) => return err(format!("chunk base64 decode: {e}"), "bad_chunk"),
    };

    // First chunk resolves the (de-duplicated) destination name; later chunks
    // carry that resolved name back, so they just write at offset.
    let final_name = if req.offset == 0 {
        dedup_upload_name(dir, name)
    } else {
        name.to_string()
    };
    let path = dir.join(&final_name);

    let write_res = async {
        let mut f = if req.offset == 0 {
            tokio::fs::File::create(&path).await?
        } else {
            tokio::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .await?
        };
        f.seek(std::io::SeekFrom::Start(req.offset)).await?;
        f.write_all(&bytes).await?;
        f.flush().await?;
        Ok::<(), std::io::Error>(())
    }
    .await;
    if let Err(e) = write_res {
        return err(format!("write {} failed: {e}", path.display()), "io_error");
    }

    let ack = FileUploadAck {
        offset: req.offset,
        done: req.eof,
        final_name: (req.offset == 0 || req.eof).then(|| final_name.clone()),
    };
    Ok(vec![(
        Frame::res(req_id, op::FILE_UPLOAD, serde_json::to_value(ack)?),
        None,
    )])
}

/// A safe upload basename: non-empty, a single path component (no `/` or `\`),
/// and not `.`/`..` — so a chunk's `name` can never escape its target `dir`.
fn is_safe_upload_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && !n.contains('/') && !n.contains('\\') && n != "." && n != ".."
}

/// De-duplicate `name` within `dir`: returns `name` if free, else inserts
/// ` (1)`, ` (2)`, … before the extension until a free name is found.
fn dedup_upload_name(dir: &std::path::Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for n in 1..100_000 {
        let cand = format!("{stem} ({n}){ext}");
        if !dir.join(&cand).exists() {
            return cand;
        }
    }
    format!("{stem}-{}{ext}", std::process::id())
}

#[cfg(test)]
mod file_transfer_tests {
    use super::{dedup_upload_name, is_safe_upload_name};

    #[test]
    fn upload_name_safety_rejects_traversal() {
        // Accept plain basenames (incl. spaces + the de-dup suffix shape).
        assert!(is_safe_upload_name("data.csv"));
        assert!(is_safe_upload_name("my report (1).txt"));
        // Reject anything that could escape the target dir.
        assert!(!is_safe_upload_name(""));
        assert!(!is_safe_upload_name("   "));
        assert!(!is_safe_upload_name("../etc/passwd"));
        assert!(!is_safe_upload_name("a/b.txt"));
        assert!(!is_safe_upload_name("a\\b.txt"));
        assert!(!is_safe_upload_name("."));
        assert!(!is_safe_upload_name(".."));
    }

    #[test]
    fn dedup_suffixes_on_collision() {
        let dir = std::env::temp_dir().join(format!("sot-ul-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Free name → unchanged.
        assert_eq!(dedup_upload_name(&dir, "x.txt"), "x.txt");
        // Collisions insert ` (n)` before the extension.
        std::fs::write(dir.join("x.txt"), b"").unwrap();
        assert_eq!(dedup_upload_name(&dir, "x.txt"), "x (1).txt");
        std::fs::write(dir.join("x (1).txt"), b"").unwrap();
        assert_eq!(dedup_upload_name(&dir, "x.txt"), "x (2).txt");
        // Extensionless names get the suffix at the end.
        assert_eq!(dedup_upload_name(&dir, "data"), "data");
        std::fs::write(dir.join("data"), b"").unwrap();
        assert_eq!(dedup_upload_name(&dir, "data"), "data (1)");

        std::fs::remove_dir_all(&dir).ok();
    }
}
