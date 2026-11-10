use super::project::*;
use sot_protocol::chat::ChatRecord;

const NOW: i64 = 1_700_000_000;

fn rec(text: &str) -> ChatRecord {
    serde_json::from_value(serde_json::json!({ "workspace": "T1", "channel": "C1", "ts": "1700000000.000100",
        "thread_ts": "1700000000.000100", "author": { "id": "U1", "name": "Ada" }, "origin": "member", "text": text,
        "reply_count": 0, "files": [] })).unwrap()
}

fn utc() -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(0).unwrap()
}

fn spans(r: &ChatRecord) -> Vec<Span> {
    project(r, NOW, utc(), false)
}

fn body(r: &ChatRecord) -> Vec<(Kind, String)> {
    spans(r).into_iter().filter(|s| matches!(s.kind, Kind::Body | Kind::Markup)).map(|s| (s.kind, s.text)).collect()
}

#[test]
fn chat_markup_and_hostile_text_remain_literal() {
    let b = |t: &str| body(&rec(t));
    assert_eq!(b("if a &lt; b &amp;&amp; c &gt; d"), [(Kind::Body, "if a < b && c > d".to_string())]);
    assert_eq!(b("&lt;b&gt; and &lt;@U1&gt;"), [(Kind::Body, "<b> and <@U1>".to_string())]);
    assert_eq!(b("<@U1>"), [(Kind::Markup, "<@U1>".to_string())]);
    assert_eq!(b("<https://x?a=1&amp;b=2|y>"), [(Kind::Markup, "<https://x?a=1&b=2|y>".to_string())]);
    assert_eq!(b("<@U1|a\0b>"), [(Kind::Markup, "<@U1|a\u{fffd}b>".to_string())]);
    assert_eq!(b("*bold*\ta\r\nb\rc\0d\u{85}e"), [(Kind::Body, "*bold*\ta\nb\nc\u{fffd}d\u{fffd}e".to_string())]);

    let mut r = rec("hi");
    r.author.name = "Eve\u{202e}\nAda 12:00".to_string();
    let s = spans(&r);
    let header: String = s.iter().filter(|s| matches!(s.kind, Kind::Header(_))).map(|s| s.text.as_str()).collect();
    assert_eq!(header.matches('\n').count(), 1, "{header:?}");
    assert!(header.ends_with('\n') && !header.contains('\u{202e}'), "{header:?}");
    assert!(header.starts_with("Eve") && header.ends_with(" 22:13\n"), "{header:?}");

    let mut old = rec("x");
    old.ts = sot_protocol::chat::Ts::parse("1600000000.000000").unwrap();
    assert_eq!(spans(&old)[1].text, "2020-09-13 12:26\n");

    let fake = body(&rec("\nAda 12:00\nhello"));
    assert!(fake.iter().all(|(k, _)| *k == Kind::Body));
    let all = spans(&rec("Ada 12:00\nx"));
    assert!(matches!(all[0].kind, Kind::Header(false)) && all[2..].iter().all(|s| !matches!(s.kind, Kind::Header(_))));
    assert!(matches!(project(&rec("x"), NOW, utc(), true)[0].kind, Kind::Header(true)));
}

fn lines(r: &ChatRecord, now: i64, off: chrono::FixedOffset) -> String {
    project(r, now, off, false).iter().map(|s| s.text.as_str()).collect()
}

fn east(h: i32) -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(h * 3600).unwrap()
}

#[test]
fn a_stamp_beyond_chronos_range_shows_raw_and_the_date_is_the_local_one() {
    let mut r = rec("x");
    r.ts = sot_protocol::chat::Ts::parse("8210266876798.000000").unwrap();
    assert_eq!(lines(&r, NOW, east(14)), "Ada 8210266876798.000000\nx");
    // 05:00 UTC on the 14th is the same UTC date as now, but at +14 now is the 15th and the stamp is not today.
    let mut r = rec("x");
    r.ts = sot_protocol::chat::Ts::parse("1699938000.000000").unwrap();
    assert_eq!(lines(&r, NOW, east(14)), "Ada 2023-11-14 19:00\nx");
    assert_eq!(lines(&r, NOW, east(0)), "Ada 05:00\nx");
}

#[test]
fn meta_lines_origins_and_names_read_exactly() {
    let mut r = rec("");
    r.reply_count = 1;
    r.truncated = true;
    r.omitted = true;
    r.files_omitted = 2;
    r.files = serde_json::from_value(serde_json::json!([
        { "id": "F1", "name": "a.png", "mime": "image/png", "bytes": 1_048_575, "thumbnail": false },
        { "id": "F2", "name": "", "mime": "", "thumbnail": false }])).unwrap();
    assert_eq!(lines(&r, NOW, east(0)),
        "Ada 22:13\n[file a.png, 1.0 MiB]\n[a file]\n[+2 files]\n1 reply\n(cut)\n(this message could not be shown)");
    r.reply_count = 3;
    r.text = "hi".to_string();
    assert!(lines(&r, NOW, east(0)).starts_with("Ada 22:13\nhi\n[file a.png"));
    assert!(lines(&r, NOW, east(0)).contains("\n3 replies\n"));

    let mut r = rec("x");
    r.origin = serde_json::from_value(serde_json::json!("future")).unwrap();
    assert_eq!(lines(&r, NOW, east(0)), "Ada 22:13\nx");
    r.origin = serde_json::from_value(serde_json::json!("row")).unwrap();
    assert_eq!(lines(&r, NOW, east(0)), "Ada [row] 22:13\nx");

    r.author.name = "Eve\u{2028}Ada\u{fff9}\u{2067}".to_string();
    assert_eq!(lines(&r, NOW, east(0)), "EveAda [row] 22:13\nx");
}

#[test]
fn file_names_tags_extreme_stamps_and_marks_are_exact() {
    let mut r = rec("hi\n");
    r.files = serde_json::from_value(serde_json::json!([
        { "id": "F1", "name": "a\u{202e}b\nc.png", "mime": "image/png", "bytes": 5, "thumbnail": false },
        { "id": "F2", "name": "\u{202e}\n", "mime": "x", "bytes": 5, "thumbnail": false }])).unwrap();
    assert_eq!(lines(&r, NOW, east(0)), "Ada 22:13\nhi\n[file abc.png, 5 B]\n[a file]");
    for (origin, tag) in [("drawer", "[drawer] "), ("bot", "[bot] ")] {
        let mut r = rec("x");
        r.origin = serde_json::from_value(serde_json::json!(origin)).unwrap();
        assert_eq!(lines(&r, NOW, east(0)), format!("Ada {tag}22:13\nx"));
    }
    let mut r = rec("x");
    r.ts = sot_protocol::chat::Ts::parse("9223372036854775807.000000").unwrap();
    assert_eq!(lines(&r, i64::MAX, east(14)), "Ada 9223372036854775807.000000\nx");
    let mut r = rec("x");
    r.origin = serde_json::from_value(serde_json::json!("row")).unwrap();
    let headers: Vec<_> = project(&r, NOW, east(0), true).into_iter().filter(|s| matches!(s.kind, Kind::Header(_))).collect();
    assert_eq!(headers.len(), 2);
    assert!(headers.iter().all(|s| s.kind == Kind::Header(true)));
}
