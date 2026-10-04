//! How much mail is unread: the inbox and cursor read the way comm-lib.sh reads them (cksum, cursor forms, `scan`).

use super::*;

/// POSIX `cksum` of `bytes`: the CRC-32 (0x04C11DB7, MSB first) over the bytes
/// then the length's bytes low first, complemented, with the length.
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

/// The cursor's line hash, `<crc>-<len>`: `comm-lib.sh`'s `_sot_hash_stdin`,
/// over the line without its newlines and NULs.
fn line_hash(line: &[u8]) -> String {
    let kept: Vec<u8> = line.iter().copied().filter(|b| *b != b'\n' && *b != 0).collect();
    let (crc, len) = cksum(&kept);
    format!("{crc}-{len}")
}

/// jq's `. > $cur` for a segment's `(fromjson? // {}) | (.ts // "")` against a
/// string. `None` where the segment emits nothing at all (a non-object JSON
/// value: `.ts` errors and jq's `//` yields no element), so it is neither
/// counted nor a boundary.
fn ts_is_greater(segment: &str, cur: &str) -> bool {
    use serde_json::Value;
    // `(fromjson? | objects) // {}`: only an object's `.ts` can be a boundary.
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(segment) else {
        return false;
    };
    match o.get("ts") {
        Some(Value::String(t)) => t.as_str() > cur,
        // null and false read as "" (never greater than a non-empty cursor);
        // true and numbers sort below every string, arrays and objects above.
        Some(Value::Array(_) | Value::Object(_)) => true,
        _ => false,
    }
}

/// How many inbox lines `read/<h>.cursor` says were shown: a port of
/// `comm-lib.sh`'s `sot_cursor_offset`, the spec, and answers what it answers
/// for every input (the cross-check test runs both).
pub(crate) fn cursor_offset(comm_home: &Path, handle: &str) -> u64 {
    let cur = std::fs::read(comm_home.join("read").join(format!("{handle}.cursor"))).unwrap_or_default();
    let cur = String::from_utf8_lossy(&cur);
    let cur = cur.trim_end_matches('\n');
    if cur.is_empty() {
        return 0;
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
            return if cnt == total + 1 && hash.is_some_and(|h| !h.is_empty()) { total } else { 0 };
        }
        if cnt > 0 {
            if let Some(h) = hash.filter(|h| !h.is_empty()) {
                let line = inbox.split(|b| *b == b'\n').nth(cnt as usize - 1).unwrap_or_default();
                if line_hash(line) != h {
                    return cnt - 1;
                }
            }
        }
        return cnt;
    }
    let text = String::from_utf8_lossy(&inbox);
    let mut n = 0u64;
    for seg in text.split('\n').filter(|s| !s.is_empty()) {
        if ts_is_greater(seg, cur) {
            break;
        }
        n += 1;
    }
    if n > total {
        0
    } else {
        n
    }
}

/// Opens the inbox fresh (a cached size can lag another box's append on a
/// shared home), reads up to the last `\n` only, and counts the lines at
/// index >= max(cursor, `woken_line`) that are to `handle` and not from it.
/// A `woken_line` past the inbox's end (it shrank) counts as never woken.
pub(super) fn scan(comm_home: &Path, handle: &str, woken_line: u64) -> Scan {
    let cursor = cursor_offset(comm_home, handle);
    let Ok(bytes) = std::fs::read(comm_home.join("inbox").join(format!("{handle}.jsonl"))) else {
        return Scan::default();
    };
    let Some(end) = bytes.iter().rposition(|b| *b == b'\n') else {
        return Scan::default();
    };
    let total = bytes[..=end].iter().filter(|b| **b == b'\n').count() as u64;
    let woken_line = if woken_line > total { 0 } else { woken_line };
    let mut out = Scan { total, ..Scan::default() };
    for (i, line) in bytes[..end].split(|b| *b == b'\n').enumerate() {
        let i = i as u64;
        if i < cursor || !counts(line, handle) {
            continue;
        }
        out.unread += 1;
        if i >= woken_line {
            out.fresh += 1;
        }
    }
    out
}

fn counts(line: &[u8], handle: &str) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
        return false;
    };
    v.get("to").and_then(|t| t.as_str()) == Some(handle)
        && v.get("from").and_then(|f| f.as_str()) != Some(handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(inbox: &str, cursor: Option<&str>) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("inbox")).unwrap();
        std::fs::create_dir_all(d.path().join("read")).unwrap();
        std::fs::write(d.path().join("inbox/a.jsonl"), inbox).unwrap();
        if let Some(c) = cursor {
            std::fs::write(d.path().join("read/a.cursor"), c).unwrap();
        }
        d
    }

    const MINE: &str = "{\"from\":\"b\",\"to\":\"a\",\"msg\":\"x\"}\n";

    fn hashed(n: usize, inbox: &str) -> String {
        format!("{n} {}\n", line_hash(inbox.split('\n').nth(n - 1).unwrap().as_bytes()))
    }

    #[test]
    fn scan_counts_past_the_cursor() {
        let inbox = MINE.repeat(3);
        let d = home(&inbox, Some(&hashed(1, &inbox)));
        assert_eq!(scan(d.path(), "a", 0), Scan { total: 3, unread: 2, fresh: 2 });
        // A wake at line 2 leaves one fresh line.
        assert_eq!(scan(d.path(), "a", 2), Scan { total: 3, unread: 2, fresh: 1 });
        // The inbox shrank past the last wake: every unread line is fresh.
        assert_eq!(scan(d.path(), "a", 9), Scan { total: 3, unread: 2, fresh: 2 });
    }

    #[test]
    fn cksum_matches_the_real_one() {
        // Values from the system `cksum`.
        assert_eq!(cksum(b""), (4294967295, 0));
        assert_eq!(cksum(b"a"), (1220704766, 1));
        assert_eq!(cksum(b"ab"), (2072780115, 2));
        assert_eq!(cksum(MINE.trim_end().as_bytes()), (3902411541, 31));
        assert_eq!(line_hash(b"a\0\n"), "1220704766-1");
    }

    fn off(inbox: &str, cursor: Option<&str>) -> u64 {
        cursor_offset(home(inbox, cursor).path(), "a")
    }

    const TS: &str = "{\"to\":\"a\",\"ts\":\"2026-01-0";

    fn ts_inbox(stamps: &[&str]) -> String {
        stamps.iter().map(|t| format!("{TS}{t}Z\"}}\n")).collect()
    }

    #[test]
    fn offsets_by_form() {
        let ib = MINE.repeat(3);
        // Count and hash matching; mismatching (one back); one past the end
        // with a hash (the total); further past (0); bare count past, in range.
        assert_eq!(off(&ib, Some(&hashed(2, &ib))), 2);
        assert_eq!(off(&ib, Some("2 1-1")), 1);
        assert_eq!(off(&ib, Some(&format!("4 {}", line_hash(b"x")))), 3);
        assert_eq!(off(&ib, Some("4")), 0);
        assert_eq!(off(&ib, Some("5 1-1")), 0);
        assert_eq!(off(&ib, Some("9")), 0);
        assert_eq!(off(&ib, Some("2")), 2);
        assert_eq!(off(&ib, Some("0")), 0);
        // Empty or absent.
        assert_eq!(off(&ib, Some("")), 0);
        assert_eq!(off(&ib, None), 0);
        assert_eq!(off("", Some("2")), 0);
    }

    #[test]
    fn timestamp_cursors() {
        let ib = ts_inbox(&["1T00:00:01", "1T00:00:02", "1T00:00:03"]);
        // Nothing newer: the total, so no unread and no wake storm.
        assert_eq!(off(&ib, Some("2026-01-02T00:00:00Z")), 3);
        // A newer line in the middle.
        assert_eq!(off(&ib, Some("2026-01-01T00:00:01Z")), 1);
        // An unparseable line before the boundary is counted, never a boundary.
        let torn = format!("{}not json\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
        assert_eq!(off(&torn, Some("2026-01-01T00:00:05Z")), 2);
        // A skewed earlier line: the FIRST greater line wins.
        let skew = ts_inbox(&["1T00:00:09", "1T00:00:01", "1T00:00:02"]);
        assert_eq!(off(&skew, Some("2026-01-01T00:00:05Z")), 0);
        // A torn tail is a segment too, but a count past the total is 0.
        let tail = format!("{}{{\"ts\":", ts_inbox(&["1T00:00:01"]));
        assert_eq!(off(&tail, Some("2026-01-02T00:00:00Z")), 0);
    }

    #[test]
    fn the_wake_reads_a_timestamp_cursor_as_no_mail() {
        let ib = ts_inbox(&["1T00:00:01", "1T00:00:02"]);
        let d = home(&ib, Some("2026-01-02T00:00:00Z\n"));
        assert_eq!(scan(d.path(), "a", 0).unread, 0);
    }

    /// The shell is the spec: every fixture is run through the real
    /// `sot_cursor_offset` and must give the same number. Linux only, like the
    /// other tests that run the shell: it needs bash and jq on PATH.
    #[cfg(target_os = "linux")]
    #[test]
    fn agrees_with_the_shell() {
        let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../comm/lib/comm-lib.sh");
        let ib = MINE.repeat(3);
        let ts = ts_inbox(&["1T00:00:01", "1T00:00:02", "1T00:00:03"]);
        let torn = format!("{}not json\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
        let skew = ts_inbox(&["1T00:00:09", "1T00:00:01", "1T00:00:02"]);
        let tail = format!("{}{{\"ts\":", ts_inbox(&["1T00:00:01"]));
        let nonobj = format!("{}5\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
        let arr = "{\"ts\":[1]}\n{\"ts\":\"2026-01-01T00:00:09Z\"}\n".to_string();
        let nul = format!("{{\"to\":\"a\",\"m\":\"x\0y\"}}\n{MINE}");
        let strs = format!("{}\"x\"\ntrue\n[1]\nnull\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
        let h2 = hashed(2, &ib);
        let h4 = format!("4 {}", line_hash(b"x"));
        let hn = hashed(1, &nul);
        let cases: Vec<(&str, Option<&str>)> = vec![
            (&ib, Some(&h2)),
            (&ib, Some("2 1-1")),
            (&ib, Some(&h4)),
            (&ib, Some("4")),
            (&ib, Some("5 1-1")),
            (&ib, Some("2")),
            (&ib, Some("0")),
            (&ib, Some("3 ")),
            (&ib, Some("")),
            (&ib, None),
            ("", Some("2")),
            (&nul, Some(&hn)),
            (&ts, Some("2026-01-02T00:00:00Z")),
            (&ts, Some("2026-01-01T00:00:01Z")),
            (&torn, Some("2026-01-01T00:00:05Z")),
            (&skew, Some("2026-01-01T00:00:05Z")),
            (&tail, Some("2026-01-02T00:00:00Z")),
            (&nonobj, Some("2026-01-01T00:00:05Z")),
            (&arr, Some("2026-01-01T00:00:05Z")),
            (&ib, Some("oops")),
            (&strs, Some("2026-01-01T00:00:05Z")),
        ];
        for (inbox, cursor) in cases {
            let d = home(inbox, cursor);
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(r#"source "$1"; sot_cursor_offset a 2>/dev/null"#)
                .arg("bash")
                .arg(&lib)
                .env("COMM_HOME", d.path())
                .env("SOT_COMM_HOME", d.path())
                .output()
                .expect("run bash");
            let shell: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().expect("shell offset");
            assert_eq!(cursor_offset(d.path(), "a"), shell, "inbox {inbox:?} cursor {cursor:?}");
        }
    }

    #[test]
    fn scan_counts_a_missing_cursor_from_zero() {
        let d = home(&MINE.repeat(2), None);
        assert_eq!(scan(d.path(), "a", 0).unread, 2);
    }

    #[test]
    fn an_unterminated_tail_is_not_counted() {
        let d = home(&format!("{MINE}{}", MINE.trim_end()), None);
        assert_eq!(scan(d.path(), "a", 0), Scan { total: 1, unread: 1, fresh: 1 });
    }

    #[test]
    fn own_and_foreign_lines_are_not_counted() {
        let inbox = format!(
            "{MINE}{{\"from\":\"a\",\"to\":\"a\",\"msg\":\"me\"}}\n{{\"from\":\"b\",\"to\":\"c\",\"msg\":\"no\"}}\nnot json\n"
        );
        let d = home(&inbox, None);
        assert_eq!(scan(d.path(), "a", 0), Scan { total: 4, unread: 1, fresh: 1 });
    }
}
