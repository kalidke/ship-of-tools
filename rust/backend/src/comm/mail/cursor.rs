//! The read cursor and the unread rule. `read/<h>.cursor` holds how many inbox lines its reader has been shown and the
//! hash of the last one, `<count> <crc>-<len>`; only `sotd comm poll` writes it. A count cannot lose a line two
//! senders filed in the same second, as the old timestamp cursor did, and the hash catches a line a failed append cut
//! back after a reader counted it. Every reader counts newline-terminated lines only.

use std::path::Path;

/// POSIX `cksum` of `bytes`: the CRC-32 (0x04C11DB7, MSB first) over the bytes then the length's bytes low first,
/// complemented, with the length.
fn cksum(bytes: &[u8]) -> (u32, u64) {
    let mut crc: u32 = 0;
    let mut feed = |b: u8| {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    };
    bytes.iter().for_each(|b| feed(*b));
    let mut n = bytes.len() as u64;
    while n != 0 {
        feed((n & 0xff) as u8);
        n >>= 8;
    }
    (!crc, bytes.len() as u64)
}

/// A line's hash in the cursor, `<crc>-<len>`, over the line without its newlines and NULs (the 0.6.6 scripts' reader
/// dropped a NUL from its copy, so both sides hash alike).
pub(crate) fn line_hash(line: &[u8]) -> String {
    let kept: Vec<u8> = line.iter().copied().filter(|b| *b != b'\n' && *b != 0).collect();
    let (crc, len) = cksum(&kept);
    format!("{crc}-{len}")
}

/// jq's `. > $cur` for a segment's `(fromjson? // {}) | (.ts // "")` against a string: whether a line of the old
/// timestamp form's inbox is past the cursor. Only an object's `ts` can be.
fn ts_is_greater(segment: &str, cur: &str) -> bool {
    use serde_json::Value;
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(segment) else {
        return false;
    };
    match o.get("ts") {
        Some(Value::String(t)) => t.as_str() > cur,
        // null and false read as "" (never greater than a non-empty cursor); true and numbers sort below every string,
        // arrays and objects above.
        Some(Value::Array(_) | Value::Object(_)) => true,
        _ => false,
    }
}

/// The inbox bytes up to and with the last newline: the lines a reader may count.
pub(crate) fn complete(bytes: &[u8]) -> &[u8] {
    bytes.iter().rposition(|b| *b == b'\n').map_or(&bytes[..0], |end| &bytes[..=end])
}

/// Where a reader of `inbox/<h>.jsonl` starts: the lines already shown, and whether the cursor's last line was cut
/// back (the reader then steps back one, and says so).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Offset {
    pub lines: u64,
    pub stepped_back: bool,
}

/// `read/<h>.cursor` against `inbox/<h>.jsonl`. A count with a hash its line no longer has is one back; a hashed count
/// one past the end is the last line read, cut back with nothing filed since (the total); any other count past the end
/// is 0 (the inbox was cleared or restored by hand, and a cursor left past it would never see mail again); the old
/// timestamp form counts the lines before the first one stamped later; anything unreadable is 0. In doubt it reads low:
/// a line shown twice is tolerable, a line dropped is not.
pub(crate) fn offset(comm_home: &Path, handle: &str) -> Offset {
    let at = |lines| Offset { lines, stepped_back: false };
    let cur = std::fs::read(comm_home.join("read").join(format!("{handle}.cursor"))).unwrap_or_default();
    let cur = String::from_utf8_lossy(&cur);
    let cur = cur.trim_end_matches('\n');
    if cur.is_empty() {
        return at(0);
    }
    let inbox = std::fs::read(comm_home.join("inbox").join(format!("{handle}.jsonl"))).unwrap_or_default();
    let total = inbox.iter().filter(|b| **b == b'\n').count() as u64;
    let (cnt, hash) = match cur.split_once(' ') {
        Some((c, h)) => (c, Some(h)),
        None => (cur, None),
    };
    if !cnt.is_empty() && cnt.bytes().all(|b| b.is_ascii_digit()) {
        let cnt: u64 = cnt.parse().unwrap_or(u64::MAX);
        if cnt > total {
            return if cnt == total + 1 && hash.is_some_and(|h| !h.is_empty()) {
                Offset { lines: total, stepped_back: true }
            } else {
                at(0)
            };
        }
        if cnt > 0 {
            if let Some(h) = hash.filter(|h| !h.is_empty()) {
                let line = inbox.split(|b| *b == b'\n').nth(cnt as usize - 1).unwrap_or_default();
                if line_hash(line) != h {
                    return Offset { lines: cnt - 1, stepped_back: true };
                }
            }
        }
        return at(cnt);
    }
    let text = String::from_utf8_lossy(&inbox);
    let mut n = 0u64;
    for seg in text.split('\n').filter(|s| !s.is_empty()) {
        if ts_is_greater(seg, cur) {
            break;
        }
        n += 1;
    }
    at(if n > total { 0 } else { n })
}

/// [`offset`]'s line count.
pub(crate) fn cursor_offset(comm_home: &Path, handle: &str) -> u64 {
    offset(comm_home, handle).lines
}

/// The one unread rule: a JSON object whose `to` is a string equal to `handle` and whose `from` is not `handle`. A
/// broadcast copy (`to` empty), another handle's line, one with no string `to`, and a line that is not JSON never count.
pub(crate) fn counts(line: &[u8], handle: &str) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
        return false;
    };
    v.get("to").and_then(|t| t.as_str()) == Some(handle) && v.get("from").and_then(|f| f.as_str()) != Some(handle)
}

/// Writes `read/<h>.cursor`: `<count> <hash of last>`, or `0`. `last` is the line as the reader read it, never read
/// from the file again, so a line shown and then cut back keeps its own hash. Through a temp file and a rename, so a
/// reader never sees an empty cursor.
pub(crate) fn write(comm_home: &Path, handle: &str, count: u64, last: &[u8]) -> std::io::Result<()> {
    let dir = comm_home.join("read");
    std::fs::create_dir_all(&dir)?;
    let text = if count > 0 { format!("{count} {}", line_hash(last)) } else { "0".to_string() };
    let tmp = dir.join(format!(".{handle}.cursor.{}", std::process::id()));
    std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, dir.join(format!("{handle}.cursor")))).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
#[path = "cursor_tests.rs"]
mod tests;
