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
