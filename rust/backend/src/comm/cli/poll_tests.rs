use super::*;
use crate::comm::test_home::TestHome;
use serde_json::json;

fn reader() -> TestHome {
    let t = TestHome::new();
    t.self_file(&t.nopane(), "me-box1");
    t.live("me-box1", &t.root());
    t.own_record();
    t
}

fn file(t: &TestHome, line: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(t.home.join("inbox/me-box1.jsonl")).unwrap();
    writeln!(f, "{line}").unwrap();
}

#[test]
fn a_poll_shows_each_line_once_and_moves_the_cursor_past_them() {
    let t = reader();
    assert_eq!(poll(&t.env, &[]).stdout, "No messages.\n");
    file(&t, r#"{"from":"a","to":"me-box1","repo":"r","msg":"one","ts":"2026-01-01T00:00:00Z"}"#);
    file(&t, "not json");
    file(&t, r#"{"from":"b","to":"","repo":"q","text":"two","ts":"t"}"#);
    let o = poll(&t.env, &[]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(o.stdout, "[2026-01-01T00:00:00Z] [a:r] one\n[t] [b:q] two\n");
    let cursor = std::fs::read_to_string(t.home.join("read/me-box1.cursor")).unwrap();
    assert!(cursor.starts_with("3 "), "an unparseable line is read too: {cursor}");
    assert_eq!(poll(&t.env, &[]).stdout, "No new messages.\n");
    file(&t, r#"{"from":"a","to":"me-box1","msg":"three"}"#);
    assert_eq!(poll(&t.env, &[]).stdout, "[] [a:] three\n");
}

/// A held inbox lock is "try again": nothing is read and the cursor stays.
#[test]
fn a_busy_inbox_moves_nothing_and_says_try_again() {
    let mut t = reader();
    file(&t, r#"{"from":"a","to":"me-box1","msg":"x"}"#);
    t.set(crate::comm::mail::inbox::INBOX_READ_WAIT_ENV, "1");
    let lock = std::fs::OpenOptions::new().create(true).read(true).append(true).open(t.home.join("inbox/me-box1.lock")).unwrap();
    lock.lock().unwrap();
    let o = poll(&t.env, &[]);
    assert_eq!(o.code, 75);
    assert!(o.stdout.contains("is being written"), "{}", o.stdout);
    assert!(!t.home.join("read/me-box1.cursor").exists());
    assert_eq!(unread(&t.env, &["me-box1".into()]).code, 75);
    lock.unlock().unwrap();
    t.unset(crate::comm::mail::inbox::INBOX_READ_WAIT_ENV);
    assert_eq!(unread(&t.env, &["me-box1".into()]).stdout, "1 1\n");
    assert_eq!(poll(&t.env, &[]).code, 0);
    assert_eq!(unread(&t.env, &["me-box1".into()]).stdout, "1 0\n");
}

#[test]
fn another_rows_handle_is_not_this_processs_inbox() {
    let t = reader();
    file(&t, r#"{"from":"a","to":"me-box1","msg":"x"}"#);
    t.put("me-box1", json!({"host": "box1", "workspace_id": "ws-b", "root": t.root()}));
    let o = poll(&t.env, &[]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("No handle for this process"), "the identity already discarded it: {}", o.stderr);
    assert!(!t.home.join("read/me-box1.cursor").exists(), "nothing was read");
}

/// The end-of-turn check counts directed mail past the cursor, by the one rule.
#[test]
fn unread_counts_only_directed_mail_from_another_handle() {
    let t = reader();
    file(&t, r#"{"from":"a","to":"me-box1","msg":"x"}"#);
    file(&t, r#"{"from":"me-box1","to":"me-box1","msg":"own"}"#);
    file(&t, r#"{"from":"a","to":"","msg":"broadcast"}"#);
    assert_eq!(unread(&t.env, &["me-box1".into()]).stdout, "3 1\n");
    assert_eq!(unread(&t.env, &["../x".into()]).code, 2, "a handle is an inbox file name and nothing else");
}
