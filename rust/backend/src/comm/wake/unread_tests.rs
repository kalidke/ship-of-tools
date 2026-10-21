use super::*;
use crate::comm::mail::cursor::line_hash;

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

const TS: &str = "{\"to\":\"a\",\"ts\":\"2026-01-0";

fn ts_inbox(stamps: &[&str]) -> String {
    stamps.iter().map(|t| format!("{TS}{t}Z\"}}\n")).collect()
}

#[test]
fn the_wake_reads_a_timestamp_cursor_as_no_mail_past_it() {
    let ib = ts_inbox(&["1T00:00:01", "1T00:00:02"]);
    let d = home(&ib, Some("2026-01-02T00:00:00Z\n"));
    assert_eq!(scan(d.path(), "a", 0).unread, 0);
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
