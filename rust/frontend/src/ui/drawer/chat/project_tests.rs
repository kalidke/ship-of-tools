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
    project(r, None, NOW, utc(), false)
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
    let header: String = s.iter().filter(|s| is_header(s.kind)).map(|s| s.text.as_str()).collect();
    assert_eq!(header.matches('\n').count(), 1, "{header:?}");
    assert!(header.ends_with('\n') && !header.contains('\u{202e}'), "{header:?}");
    assert!(header.starts_with("Eve") && header.ends_with("  10:13 PM\n"), "{header:?}");

    let mut old = rec("x");
    old.ts = sot_protocol::chat::Ts::parse("1600000000.000000").unwrap();
    assert_eq!(spans(&old)[1].text, "Sep 13 2020 12:26 PM\n");

    let fake = body(&rec("\nAda 12:00\nhello"));
    assert!(fake.iter().all(|(k, _)| *k == Kind::Body));
    let all = spans(&rec("Ada 12:00\nx"));
    assert!(matches!(all[0].kind, Kind::Name) && all[2..].iter().all(|s| !is_header(s.kind)));
}

/// The kinds only a header has.
fn is_header(kind: Kind) -> bool {
    matches!(kind, Kind::Name | Kind::Time | Kind::Badge)
}

fn lines(r: &ChatRecord, now: i64, off: chrono::FixedOffset) -> String {
    project(r, None, now, off, false).iter().map(|s| s.text.as_str()).collect()
}

fn east(h: i32) -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(h * 3600).unwrap()
}

#[test]
fn a_stamp_beyond_chronos_range_shows_raw_and_the_date_is_the_local_one() {
    let mut r = rec("x");
    r.ts = sot_protocol::chat::Ts::parse("8210266876798.000000").unwrap();
    assert_eq!(lines(&r, NOW, east(14)), "Ada  8210266876798.000000\nx");
    // 05:00 UTC on the 14th is the same UTC date as now, but at +14 now is the 15th and the stamp is not today.
    let mut r = rec("x");
    r.ts = sot_protocol::chat::Ts::parse("1699938000.000000").unwrap();
    assert_eq!(lines(&r, NOW, east(14)), "Ada  Nov 14 7:00 PM\nx");
    assert_eq!(lines(&r, NOW, east(0)), "Ada  5:00 AM\nx");
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
        "Ada  10:13 PM\n[file a.png, 1.0 MiB]\n[a file]\n[+2 files]\n1 reply\n(cut)\n(this message could not be shown)");
    r.reply_count = 3;
    r.text = "hi".to_string();
    assert!(lines(&r, NOW, east(0)).starts_with("Ada  10:13 PM\nhi\n[file a.png"));
    assert!(lines(&r, NOW, east(0)).contains("\n3 replies\n"));

    let mut r = rec("x");
    r.origin = serde_json::from_value(serde_json::json!("future")).unwrap();
    assert_eq!(lines(&r, NOW, east(0)), "Ada  10:13 PM\nx");
    r.origin = serde_json::from_value(serde_json::json!("row")).unwrap();
    assert_eq!(lines(&r, NOW, east(0)), "Ada  \u{a0}\u{a0}ROW\u{a0}\u{a0}  10:13 PM\nx");

    r.author.name = "Eve\u{2028}Ada\u{fff9}\u{2067}".to_string();
    assert_eq!(lines(&r, NOW, east(0)), "EveAda  \u{a0}\u{a0}ROW\u{a0}\u{a0}  10:13 PM\nx");
}

#[test]
fn file_names_tags_extreme_stamps_and_marks_are_exact() {
    let mut r = rec("hi\n");
    r.files = serde_json::from_value(serde_json::json!([
        { "id": "F1", "name": "a\u{202e}b\nc.png", "mime": "image/png", "bytes": 5, "thumbnail": false },
        { "id": "F2", "name": "\u{202e}\n", "mime": "x", "bytes": 5, "thumbnail": false }])).unwrap();
    assert_eq!(lines(&r, NOW, east(0)), "Ada  10:13 PM\nhi\n[file abc.png, 5 B]\n[a file]");
    for (origin, tag) in [("drawer", "DRAWER"), ("bot", "APP")] {
        let mut r = rec("x");
        r.origin = serde_json::from_value(serde_json::json!(origin)).unwrap();
        assert_eq!(lines(&r, NOW, east(0)), format!("Ada  \u{a0}\u{a0}{tag}\u{a0}\u{a0}  10:13 PM\nx"));
    }
    let mut r = rec("x");
    r.ts = sot_protocol::chat::Ts::parse("9223372036854775807.000000").unwrap();
    assert_eq!(lines(&r, i64::MAX, east(14)), "Ada  9223372036854775807.000000\nx");
    let mut r = rec("x");
    r.origin = serde_json::from_value(serde_json::json!("row")).unwrap();
    let kinds: Vec<_> = project(&r, None, NOW, east(0), false).into_iter().map(|s| s.kind).collect();
    assert_eq!(kinds, [Kind::Name, Kind::Badge, Kind::Time, Kind::Body]);
}

/// A deleted message that had text, files and replies is its header and the notice and nothing more; the daemon's record of a
/// message it could not read says so and is not shown as deleted.
#[test]
fn a_deleted_message_is_its_header_and_the_notice_alone() {
    let mut r = rec("secret");
    r.reply_count = 2;
    r.files_omitted = 1;
    r.truncated = true;
    r.omitted = true;
    r.latest_reply = Some(sot_protocol::chat::Ts::parse("1700000001.000000").unwrap());
    r.files = serde_json::from_value(serde_json::json!([{ "id": "F1", "name": "a.png", "mime": "image/png", "bytes": 10, "thumbnail": false }])).unwrap();
    let shown: String = project(&r, None, NOW, utc(), true).iter().map(|s| s.text.as_str()).collect();
    assert_eq!(shown, "Ada  10:13 PM\n(message deleted)");
    let unreadable = lines(&r, NOW, utc());
    assert!(unreadable.contains("(this message could not be shown)") && !unreadable.contains("(message deleted)"), "{unreadable:?}");
}

/// Slack's desktop app shows `12:47 AM` and `1:05 PM`: a 12-hour clock with no leading zero, midnight and noon as 12.
#[test]
fn a_time_is_twelve_hour_as_slack_shows_it() {
    // 1699920000 is midnight on the day of `NOW`, in UTC.
    let at = |secs: i64, now: i64, off| when(&format!("{secs}.000000"), now, off);
    for (secs, want) in [(1_699_922_820, "12:47 AM"), (1_699_967_100, "1:05 PM"), (1_699_920_000, "12:00 AM"), (1_699_963_200, "12:00 PM"), (1_699_999_999, "10:13 PM")] {
        assert_eq!(at(secs, NOW, utc()), want);
    }
    // Another day shows its date first, and another year its year.
    assert_eq!(at(1_699_833_600, NOW, utc()), "Nov 13 12:00 AM");
    assert_eq!(at(1_600_000_000, NOW, utc()), "Sep 13 2020 12:26 PM");
    // The fixed offset moves the day as well as the hour.
    assert_eq!(at(1_699_922_820, NOW, east(-1)), "Nov 13 11:47 PM");
}

/// `rec`'s record at `secs` seconds.
fn at(secs: &str) -> ChatRecord {
    let mut r = rec("x");
    r.ts = sot_protocol::chat::Ts::parse(secs).unwrap();
    r
}

/// A record continues its author's group within `GROUP_GAP` (300 s) on one local day, and in no other case.
#[test]
fn a_message_continues_its_authors_group_within_five_minutes_on_the_same_day() {
    let first = at("1700000000.000000");
    let after = |secs: &str| continues(&at(secs), Some(&first), utc());
    assert!(after("1700000299.000000"), "299 s groups");
    assert!(after("1700000299.900000"), "299.9 s groups");
    assert!(!after("1700000300.000000"), "300 s does not");
    assert!(!continues(&first, None, utc()), "no record before");
    let next = at("1700000060.000000");
    let mut other = next.clone();
    other.author.name = "Eve".to_string();
    assert!(!continues(&other, Some(&first), utc()), "another name with the same id");
    let mut other = next.clone();
    other.author.id = "U2".to_string();
    assert!(!continues(&other, Some(&first), utc()), "another id with the same name");
    let mut other = next.clone();
    other.origin = serde_json::from_value(serde_json::json!("drawer")).unwrap();
    assert!(!continues(&other, Some(&first), utc()), "another origin");
    assert!(continues(&next, Some(&first), utc()));
    // 23:58 and 00:01 UTC are two days; an hour west they are one.
    let (late, early) = (at("1699919880.000000"), at("1699920060.000000"));
    assert!(!continues(&early, Some(&late), utc()), "another local day");
    assert!(continues(&early, Some(&late), east(-1)), "the same local day an hour west");
    // A continuation has no header, a tombstone groups like any record, and no meta item starts with a blank line.
    let kinds = |r: &ChatRecord, prev: Option<&ChatRecord>, deleted| project(r, prev, NOW, utc(), deleted).into_iter().map(|s| (s.kind, s.text)).collect::<Vec<_>>();
    assert_eq!(kinds(&next, Some(&first), false), [(Kind::Body, "x".to_string())]);
    assert_eq!(kinds(&next, Some(&first), true), [(Kind::Meta, "(message deleted)".to_string())]);
    let mut counted = next.clone();
    counted.text = String::new();
    counted.reply_count = 2;
    assert_eq!(kinds(&counted, Some(&first), false), [(Kind::Meta, "2 replies".to_string())]);
    assert_eq!(kinds(&counted, None, false)[0].0, Kind::Name);
    // The gutter's time is the clock alone.
    assert_eq!(clock("1699967100.000000", utc()).as_deref(), Some("1:05"));
}

/// A continuation whose text is a header's words is body text only: no span has the name's, the time's or the badge's kind.
#[test]
fn a_body_that_imitates_a_header_draws_no_tile_and_no_time() {
    let first = at("1700000000.000000");
    let mut next = at("1700000060.000000");
    next.text = "Ada  \u{a0}\u{a0}ROW\u{a0}\u{a0}  10:13 PM\nAda 12:00".to_string();
    let spans = project(&next, Some(&first), NOW, utc(), false);
    assert!(spans.iter().all(|s| s.kind == Kind::Body), "{spans:?}");
}
