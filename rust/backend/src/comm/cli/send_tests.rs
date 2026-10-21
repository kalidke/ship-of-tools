use super::*;
use crate::comm::test_home::TestHome;
use serde_json::json;

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// A joined sender, `me-box1`, outside any row, with the folder's lock its own (it appends itself).
fn sender() -> TestHome {
    let t = TestHome::new();
    t.self_file(&t.nopane(), "me-box1");
    t.live("me-box1", &t.root());
    t.own_record();
    t
}

#[test]
fn the_recipient_is_the_first_at_word_and_a_message_may_start_with_one() {
    let a = parse_send(&args(&["@b", "@c", "hi"])).unwrap();
    assert_eq!((a.target.as_str(), a.text.as_str()), ("b", "@c hi"));
    assert_eq!(parse_send(&args(&["@b", "-"])).unwrap().file.as_deref(), Some("-"), "a lone - is standard input");
    assert_eq!(parse_send(&args(&["@b", "--file", "p"])).unwrap().file.as_deref(), Some("p"));
    assert!(parse_send(&args(&["@b", "--file", "p", "text"])).unwrap_err().contains("not both"));
    assert!(parse_send(&args(&["@b"])).unwrap_err().starts_with("usage:"));
    assert!(parse_send(&args(&["--broadcast", "x"])).unwrap().broadcast);
    let r = parse_relay(&args(&["send", "hi", "@b", "@c"])).unwrap();
    assert_eq!((r.target.as_str(), r.text.as_str()), ("b", "hi @c"));
    assert!(parse_relay(&args(&["bridge"])).unwrap_err().contains("the one verb is send"));
}

/// `filed` only after the line is in the receiver's inbox, said by the appender.
#[test]
fn a_send_to_a_live_handle_is_filed_whole_by_its_appender() {
    let t = sender();
    t.live("you-box1", "/elsewhere");
    let o = deliver_all(parse_send(&args(&["@you-box1", "hello", "there"])).unwrap(), |_| unreachable!());
    assert_eq!((o.code, o.stdout.as_str()), (0, "  filed -> @you-box1\n"), "{}", o.stderr);
    let lines = t.inbox("you-box1");
    assert_eq!(lines.len(), 1);
    assert_eq!((lines[0]["from"].as_str(), lines[0]["to"].as_str(), lines[0]["msg"].as_str()), (Some("me-box1"), Some("you-box1"), Some("hello there")));
    assert_eq!(lines[0]["repo"].as_str(), Some(t.repo().as_str()));
}

#[test]
fn a_send_to_a_handle_no_live_session_holds_appends_nothing() {
    let t = sender();
    t.put("old-box1", json!({"host": "box1", "last_seen": "2020-01-01T00:00:00Z"}));
    let o = deliver_all(parse_send(&args(&["@old-box1", "x"])).unwrap(), |_| unreachable!());
    assert_eq!(o.code, 1);
    assert_eq!(o.stderr, "FAILED -> @old-box1: no live session holds @old-box1\n");
    assert!(t.inbox("old-box1").is_empty());
}

#[test]
fn a_sender_with_no_routable_identity_sends_nothing() {
    let t = TestHome::new();
    t.live("you-box1", "/x");
    let o = deliver_all(parse_send(&args(&["@you-box1", "x"])).unwrap(), |_| unreachable!());
    assert_eq!(o.code, 1);
    assert!(o.stderr.starts_with("FAILED -> @you-box1: your sot-comm identity did not resolve"), "{}", o.stderr);
    t.self_file(&t.nopane(), "me-box1");
    let o = deliver_all(parse_send(&args(&["@you-box1", "x"])).unwrap(), |_| unreachable!());
    assert!(o.stderr.contains("your identity @me-box1 has no registry row; join again:"), "{}", o.stderr);
    t.put("me-box1", json!({"host": "box1", "root": "/another/project"}));
    let o = deliver_all(parse_send(&args(&["@you-box1", "x"])).unwrap(), |_| unreachable!());
    assert!(o.stderr.contains("registered to a DIFFERENT project's root"), "{}", o.stderr);
    assert!(t.inbox("you-box1").is_empty());
}

/// No answer is FAILED, and nothing is queued or tried another way: a handle this folder does not list goes to the
/// hub alone, and a box that cannot take the folder's lock goes to its folder's daemon alone.
#[test]
fn silence_is_failure_and_there_is_no_second_route() {
    let t = sender();
    let at = if cfg!(windows) { "pipe:" } else { "unix:" };
    let o = deliver_all(parse_send(&args(&["@nobody", "x"])).unwrap(), |_| unreachable!());
    assert_eq!(o.code, 1);
    assert!(o.stderr.starts_with(&format!("FAILED -> @nobody: the daemon did not answer at {at}")), "{}", o.stderr);
    std::fs::remove_file(t.home.join(crate::comm::mail::inbox::LOCK_RECORD)).unwrap();
    t.live("you-box1", "/x");
    let o = deliver_all(parse_send(&args(&["@you-box1", "x"])).unwrap(), |_| unreachable!());
    assert!(o.stderr.starts_with(&format!("FAILED -> @you-box1: the daemon did not answer at {at}")), "{}", o.stderr);
    assert!(t.inbox("you-box1").is_empty(), "no lock record of its own: this box never appends itself");
}

#[test]
fn a_file_or_stdin_body_is_sent_as_its_bytes_and_an_empty_one_is_refused() {
    let t = sender();
    t.live("you-box1", "/x");
    let body = "it's `$(not run)` \"quoted\"\n";
    let o = deliver_all(parse_send(&args(&["@you-box1", "-"])).unwrap(), |f| {
        assert_eq!(f, "-");
        Ok(body.to_string())
    });
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(t.inbox("you-box1")[0]["msg"].as_str(), Some(body));
    let o = deliver_all(parse_send(&args(&["@you-box1", "--file", "/p"])).unwrap(), |_| Ok(String::new()));
    assert_eq!(o.stderr, "FAILED -> @you-box1: the message file '/p' is empty; nothing was sent\n");
}

#[test]
fn a_broadcast_files_a_copy_for_each_live_handle_and_counts_them() {
    let t = sender();
    t.live("a-box1", "/a");
    t.put("b-box1", json!({"host": "box1", "last_seen": "2020-01-01T00:00:00Z"}));
    let o = deliver_all(parse_send(&args(&["--broadcast", "all"])).unwrap(), |_| unreachable!());
    assert_eq!(o.code, 1);
    assert!(o.stdout.ends_with("Broadcast to 1 of 2 agent(s).\n"), "{}", o.stdout);
    assert_eq!(t.inbox("a-box1")[0]["to"].as_str(), Some(""), "a broadcast copy is never directed mail");
    assert!(t.inbox("me-box1").is_empty(), "never to the sender");
}
