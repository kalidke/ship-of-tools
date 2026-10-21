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

/// Every cursor form 0.6.6 writes or ever wrote reads as it read there (the values its `sot_cursor_offset` gave).
#[test]
fn a_cursor_of_every_form_reads_as_0_6_6_read_it() {
    let ib = MINE.repeat(3);
    assert_eq!(off(&ib, Some(&hashed(2, &ib))), 2);
    assert_eq!(off(&ib, Some("2 1-1")), 1, "a hash the line no longer has steps back one");
    assert_eq!(off(&ib, Some(&format!("4 {}", line_hash(b"x")))), 3, "hashed one past the end: the last line was cut back");
    assert_eq!(off(&ib, Some("4")), 0);
    assert_eq!(off(&ib, Some("5 1-1")), 0);
    assert_eq!(off(&ib, Some("9")), 0);
    assert_eq!(off(&ib, Some("2")), 2);
    assert_eq!(off(&ib, Some("0")), 0);
    assert_eq!(off(&ib, Some("3 ")), 3);
    assert_eq!(off(&ib, Some("oops")), 3, "any other text is the timestamp form: no line is stamped after it");
    assert_eq!(off(&ib, Some("")), 0);
    assert_eq!(off(&ib, None), 0);
    assert_eq!(off("", Some("2")), 0);
    let nul = format!("{{\"to\":\"a\",\"m\":\"x\0y\"}}\n{MINE}");
    assert_eq!(off(&nul, Some(&hashed(1, &nul))), 1, "a NUL is dropped from the hash on both sides");
    let d = home(&ib, Some("2 1-1"));
    assert_eq!(offset(d.path(), "a"), Offset { lines: 1, stepped_back: true });
}

#[test]
fn a_timestamp_cursor_counts_the_lines_before_the_first_later_one() {
    let ib = ts_inbox(&["1T00:00:01", "1T00:00:02", "1T00:00:03"]);
    assert_eq!(off(&ib, Some("2026-01-02T00:00:00Z")), 3);
    assert_eq!(off(&ib, Some("2026-01-01T00:00:01Z")), 1);
    let torn = format!("{}not json\n{}", ts_inbox(&["1T00:00:01"]), ts_inbox(&["1T00:00:09"]));
    assert_eq!(off(&torn, Some("2026-01-01T00:00:05Z")), 2, "an unparseable line is counted, never a boundary");
    let skew = ts_inbox(&["1T00:00:09", "1T00:00:01", "1T00:00:02"]);
    assert_eq!(off(&skew, Some("2026-01-01T00:00:05Z")), 0, "the first later line wins");
    let tail = format!("{}{{\"ts\":", ts_inbox(&["1T00:00:01"]));
    assert_eq!(off(&tail, Some("2026-01-02T00:00:00Z")), 0);
    let arr = "{\"ts\":[1]}\n{\"ts\":\"2026-01-01T00:00:09Z\"}\n";
    assert_eq!(off(arr, Some("2026-01-01T00:00:05Z")), 0);
}

#[test]
fn only_directed_mail_from_another_handle_counts() {
    assert!(counts(MINE.trim_end().as_bytes(), "a"));
    for line in [
        "{\"from\":\"a\",\"to\":\"a\",\"msg\":\"own\"}",
        "{\"from\":\"b\",\"to\":\"c\"}",
        "{\"from\":\"b\",\"to\":5}",
        "{\"from\":\"b\",\"msg\":\"no to\"}",
        "{\"from\":\"b\",\"to\":\"\",\"msg\":\"broadcast\"}",
        "not json",
        "[1]",
        "\"a\"",
    ] {
        assert!(!counts(line.as_bytes(), "a"), "{line}");
    }
    assert!(counts(b"{\"to\":\"a\"}", "a"), "a line with no from is another's");
}

#[test]
fn a_written_cursor_reads_back_and_its_hash_is_the_line_as_read() {
    let ib = MINE.repeat(2);
    let d = home(&ib, None);
    write(d.path(), "a", 2, MINE.trim_end().as_bytes()).unwrap();
    let text = std::fs::read_to_string(d.path().join("read/a.cursor")).unwrap();
    assert_eq!(text, format!("2 {}", line_hash(MINE.trim_end().as_bytes())), "no newline, as 0.6.6 wrote it");
    assert_eq!(off(&ib, Some(&text)), 2);
    write(d.path(), "a", 0, b"").unwrap();
    assert_eq!(std::fs::read_to_string(d.path().join("read/a.cursor")).unwrap(), "0");
    assert_eq!(complete(b"a\nb"), b"a\n");
    assert_eq!(complete(b"ab"), b"");
}
