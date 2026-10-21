//! Streaming file hashing. Archives are tens of MB and growing — hash in
//! chunks, never by slurping the file (Codex review, SHOULD-FIX 3). `sha2`
//! replaces the backend's hand-rolled FIPS-180-4 implementation; the NIST
//! known-answer vectors move with it.

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

/// Lowercase hex sha256 of a file, streamed in 1 MiB chunks.
pub async fn sha256_file(path: &Path) -> Result<String> {
    let mut f = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Lowercase hex sha256 of an in-memory buffer (small inputs, tests).
pub fn sha256_bytes(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
#[path = "hash_tests.rs"]
mod tests;
