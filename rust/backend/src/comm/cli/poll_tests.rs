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

fn long(t: &TestHome, tag: &str, n: usize) {
    file(t, &format!(r#"{{"from":"a","to":"me-box1","repo":"r","msg":"{tag}{}","ts":"t"}}"#, "x".repeat(n)));
}

/// Twenty 1,000-byte lines: one poll prints within the budget, says how many remain and stops the cursor at the last
/// line it printed; the next poll prints the rest.
#[test]
fn a_poll_prints_a_bounded_batch_and_moves_the_cursor_only_over_what_it_printed() {
    let t = reader();
    for i in 0..20 {
        long(&t, &format!("{i:02}"), 987);
    }
    let first = poll(&t.env, &[]);
    let (body, tail) = first.stdout.rsplit_once("4 more").map(|(b, t)| (b, t)).expect("a remaining count");
    assert!(body.len() <= 16_000, "{} bytes", body.len());
    assert_eq!(tail, " messages: run comm-poll.sh again\n");
    let printed = body.lines().count() as u64;
    assert!(printed < 20 && printed > 10);
    let cursor = std::fs::read_to_string(t.home.join("read/me-box1.cursor")).unwrap();
    assert!(cursor.starts_with(&format!("{printed} ")), "{cursor}");
    assert_eq!(unread(&t.env, &["me-box1".into()]).stdout, format!("20 {}\n", 20 - printed));
    let rest = poll(&t.env, &[]).stdout;
    assert_eq!(rest.lines().count() as u64, 20 - printed);
    assert!(!rest.contains("more messages"));
    assert!(rest.contains(&format!("{:02}x", 19)) && !rest.contains(&format!("{:02}x", printed - 1)));
    assert_eq!(unread(&t.env, &["me-box1".into()]).stdout, "20 0\n");
}

/// A line over the budget is printed alone and whole; the line after it waits.
#[test]
fn a_line_over_the_budget_is_printed_alone_and_whole() {
    let t = reader();
    long(&t, "big", 20_000);
    long(&t, "next", 10);
    let o = poll(&t.env, &[]);
    assert!(o.stdout.contains(&"x".repeat(20_000)), "whole");
    assert!(o.stdout.ends_with("1 more messages: run comm-poll.sh again\n"), "{}", &o.stdout[o.stdout.len() - 80..]);
    assert!(!o.stdout.contains("next"));
    assert!(std::fs::read_to_string(t.home.join("read/me-box1.cursor")).unwrap().starts_with("1 "));
    assert!(poll(&t.env, &[]).stdout.contains("next"));
}

/// Lines the poll cannot show cost nothing: one before the edge is passed with the batch, one after it is read at the
/// next poll, which prints no more than the line that is left.
#[test]
fn unshowable_lines_cost_nothing_and_are_passed() {
    let t = reader();
    file(&t, "not json");
    long(&t, "a", 15_000);
    file(&t, "also not json");
    long(&t, "b", 2_000);
    file(&t, "last not json");
    let o = poll(&t.env, &[]);
    assert!(o.stdout.contains("ax") && !o.stdout.contains("bx"));
    assert!(o.stdout.ends_with("1 more messages: run comm-poll.sh again\n"));
    assert!(std::fs::read_to_string(t.home.join("read/me-box1.cursor")).unwrap().starts_with("2 "));
    let o = poll(&t.env, &[]);
    assert!(o.stdout.starts_with("[t] [a:r] b") && o.stdout.lines().count() == 1, "{}", o.stdout);
    assert!(std::fs::read_to_string(t.home.join("read/me-box1.cursor")).unwrap().starts_with("5 "));
}

/// A line deferred for its size holds back every line after it, a small one included: none is marked read unprinted.
#[test]
fn a_small_line_never_passes_a_deferred_one() {
    let t = reader();
    long(&t, "a", 15_000);
    long(&t, "BIG", 9_000);
    long(&t, "small", 10);
    let o = poll(&t.env, &[]);
    assert!(o.stdout.contains("ax") && !o.stdout.contains("BIG") && !o.stdout.contains("small"));
    assert!(o.stdout.ends_with("2 more messages: run comm-poll.sh again\n"));
    let rest = poll(&t.env, &[]).stdout;
    assert!(rest.contains("BIGx") && rest.contains("small"), "{rest}");
}

/// The budget counts bytes: ten lines of 3,000 bytes each (1,000 three-byte characters) do not all print.
#[test]
fn the_budget_counts_bytes_not_characters() {
    let t = reader();
    for i in 0..10 {
        file(&t, &format!(r#"{{"from":"a","to":"me-box1","repo":"r","msg":"{i}{}","ts":"t"}}"#, "\u{20ac}".repeat(1000)));
    }
    let o = poll(&t.env, &[]);
    let body = o.stdout.rsplit_once("more messages").expect("lines remain").0;
    assert!(body.len() <= 16_000 + 10, "{} bytes", body.len());
}
