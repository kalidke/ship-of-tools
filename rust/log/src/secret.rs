//! Masking of page secrets in log output (ADR 0049, User isolation): `redact` and the writer that applies it.

use std::borrow::Cow;

/// `bytes` with the value after every `secret=` (up to the first `&`, `#`, whitespace, `"`, `'`, `<`, `>`, `)`, `\`,
/// or the end) and every maximal run of exactly 32 characters in `[0-9a-f]` replaced by `<redacted>`; every other
/// byte unchanged; borrowed when nothing matched.
pub fn redact(bytes: &[u8]) -> Cow<'_, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    let mut done = 0; // bytes[..done] has been copied to `out`
    let mut i = 0;
    while i < bytes.len() {
        let (masked, next) = if bytes[i..].starts_with(MARKER) {
            let start = i + MARKER.len();
            let end = bytes[start..].iter().position(|&b| ends_value(b)).map_or(bytes.len(), |n| start + n);
            (start..end, end)
        } else if is_hex(bytes[i]) {
            let end = bytes[i..].iter().position(|&b| !is_hex(b)).map_or(bytes.len(), |n| i + n);
            (if end - i == RUN { i..end } else { 0..0 }, end)
        } else {
            i += 1;
            continue;
        };
        if !masked.is_empty() {
            let out = out.get_or_insert_with(|| Vec::with_capacity(bytes.len()));
            out.extend_from_slice(&bytes[done..masked.start]);
            out.extend_from_slice(MASK);
            done = masked.end;
        }
        i = next;
    }
    match out {
        None => Cow::Borrowed(bytes),
        Some(mut out) => {
            out.extend_from_slice(&bytes[done..]);
            Cow::Owned(out)
        }
    }
}

const MARKER: &[u8] = b"secret=";
const MASK: &[u8] = b"<redacted>";
/// The length of a token `random_token` mints: 128 bits as lowercase hex.
const RUN: usize = 32;

fn is_hex(b: u8) -> bool {
    matches!(b, b'0'..=b'9' | b'a'..=b'f')
}

fn ends_value(b: u8) -> bool {
    matches!(b, b'&' | b'#' | b'"' | b'\'' | b'<' | b'>' | b')' | b'\\') || b.is_ascii_whitespace()
}

/// A log writer that hands `redact(buf)` to the inner writer whole and reports all of `buf` written, so one call
/// masks one event.
pub struct RedactingWriter<W>(pub W);

impl<W: std::io::Write> std::io::Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write_all(&redact(buf))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
#[path = "secret_tests.rs"]
mod tests;
