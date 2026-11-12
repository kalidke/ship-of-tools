use super::mrkdwn::{BOLD, CODE, ITALIC};
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

/// The spans of a record with no divider: these tests are about the header, the body and the meta lines.
fn spans(r: &ChatRecord) -> Vec<Span> {
    project(r, None, NOW, utc(), false).into_iter().filter(|s| s.kind != Kind::Day).collect()
}

fn body(r: &ChatRecord) -> Vec<(Kind, String)> {
    spans(r).into_iter().filter(|s| matches!(s.kind, Kind::Body | Kind::Markup | Kind::Link | Kind::Mention)).map(|s| (s.kind, s.text)).collect()
}

#[test]
fn chat_markup_and_hostile_text_remain_literal() {
    let b = |t: &str| body(&rec(t));
    assert_eq!(b("if a &lt; b &amp;&amp; c &gt; d"), [(Kind::Body, "if a < b && c > d".to_string())]);
    assert_eq!(b("&lt;b&gt; and &lt;@U1&gt;"), [(Kind::Body, "<b> and <@U1>".to_string())]);
    assert_eq!(b("<@U1>"), [(Kind::Mention, "@U1".to_string())]);
    assert_eq!(b("<https://x?a=1&amp;b=2|y>"), [(Kind::Link, "y".to_string())]);
    assert_eq!(b("<@U1|a\0b>"), [(Kind::Mention, "@a\u{fffd}b".to_string())]);
    assert_eq!(b("<!foo\0>"), [(Kind::Markup, "<!foo\u{fffd}>".to_string())]);
    // Styled text loses its markers; the controls and line ends around it are as ever.
    assert_eq!(b("*bold*\ta\r\nb\rc\0d\u{85}e"), [(Kind::Body, "bold".to_string()), (Kind::Body, "\ta\nb\nc\u{fffd}d\u{fffd}e".to_string())]);
    assert_eq!(b("*bold\ta\r\nb*"), [(Kind::Body, "*bold\ta\nb*".to_string())]);
    assert_eq!(b("`<@U1|a\0b>` &lt;@U1&gt;"), [(Kind::Body, "<@U1|a\u{fffd}b>".to_string()), (Kind::Body, " <@U1>".to_string())]);

    let mut r = rec("hi");
    r.author.name = "Eve\u{202e}\nAda 12:00".to_string();
    let s = spans(&r);
    let header: String = s.iter().filter(|s| is_header(s.kind)).map(|s| s.text.as_str()).collect();
    assert_eq!(header.matches('\n').count(), 1, "{header:?}");
    assert!(header.ends_with('\n') && !header.contains('\u{202e}'), "{header:?}");
    assert!(header.starts_with("Eve") && header.ends_with("  10:13 PM\n"), "{header:?}");

    let mut old = rec("x");
    old.ts = sot_protocol::chat::Ts::parse("1600000000.000000").unwrap();
    assert_eq!(spans(&old)[1].text, "12:26 PM\n");

    let fake = body(&rec("\nAda 12:00\nhello"));
    assert!(fake.iter().all(|(k, _)| *k == Kind::Body));
    let all = spans(&rec("Ada 12:00\nx"));
    assert!(matches!(all[0].kind, Kind::Name) && all[2..].iter().all(|s| !is_header(s.kind)));
}

/// Slack's tokens as the app shows them: a link its label (or its URL), a mention `@` and its label (or its id), a channel `#`
/// and its label (or its id), the specials by name, a date its fallback; a token the drawer does not read stays as typed.
#[test]
fn links_mentions_and_specials_show_as_slack_shows_them() {
    let b = |t: &str| body(&rec(t));
    let s = |k, t: &str| (k, t.to_string());
    assert_eq!(b("<https://x?a=1&amp;b=2|notes>"), [s(Kind::Link, "notes")]);
    assert_eq!(b("<https://x.test/p>"), [s(Kind::Link, "https://x.test/p")]);
    assert_eq!(b("<http://x.test|a &amp; b>"), [s(Kind::Link, "a & b")]);
    assert_eq!(b("<mailto:a@x.test|mail me>"), [s(Kind::Link, "mail me")]);
    assert_eq!(b("<https://x.test|>"), [s(Kind::Link, "https://x.test")], "an empty label shows the URL");
    assert_eq!(b("<@U1>"), [s(Kind::Mention, "@U1")]);
    assert_eq!(b("<@W2|ada>"), [s(Kind::Mention, "@ada")]);
    assert_eq!(b("<@U1|>"), [s(Kind::Mention, "@U1")]);
    assert_eq!(b("<#C1|dev>"), [s(Kind::Mention, "#dev")]);
    assert_eq!(b("<#C1>"), [s(Kind::Mention, "#C1")]);
    assert_eq!(b("<#C1|>"), [s(Kind::Mention, "#C1")]);
    for t in ["<!here>", "<!here|here>", "<!here|anything>"] {
        assert_eq!(b(t), [s(Kind::Mention, "@here")], "{t}");
    }
    assert_eq!(b("<!channel> <!everyone|x>"), [s(Kind::Mention, "@channel"), s(Kind::Body, " "), s(Kind::Mention, "@everyone")]);
    assert_eq!(b("<!subteam^S1|@team>"), [s(Kind::Mention, "@team")]);
    // A date shows its fallback as the text around it, and joins it.
    assert_eq!(b("on <!date^1392734382^{date_num}|Jan 1> ok"), [s(Kind::Body, "on Jan 1 ok")]);
    // Whatever else is a token stays as today, brackets and all, in the markup's style.
    for t in ["<!foo>", "<x>", "<@u1>", "<@U1 x>", "<@Z1>", "<#D1|x>", "<http://>", "<ftp://x.test|y>", "<!subteam^S1>", "<!date^1^x>", "<!here", "<https://x.test|y"] {
        assert_eq!(b(t), [s(Kind::Markup, &t.replace("&amp;", "&"))], "{t}");
    }
    // In a line: the words around a token are body text, and a style reaches a token's run.
    assert_eq!(b("see <https://x.test|notes> and <@U1|ada> in <#C1|dev>, <!here>"), [
        s(Kind::Body, "see "), s(Kind::Link, "notes"), s(Kind::Body, " and "), s(Kind::Mention, "@ada"), s(Kind::Body, " in "),
        s(Kind::Mention, "#dev"), s(Kind::Body, ", "), s(Kind::Mention, "@here")]);
    let styled = project(&rec("*<@U1|ada> x*"), None, NOW, utc(), false).into_iter().filter(|x| x.kind == Kind::Mention).map(|x| (x.text, x.style)).collect::<Vec<_>>();
    assert_eq!(styled, [("@ada".to_string(), BOLD)]);
    // Typed `&lt;@U1&gt;` is text, not a token, and a token in code is its decoded text.
    assert_eq!(b("&lt;@U1&gt; `<@U1|ada>`"), [s(Kind::Body, "<@U1> "), s(Kind::Body, "<@U1|ada>")]);
    // No token makes a header or a badge: a mention named like one is a mention.
    let spans = project(&rec("<@U1|ROW> <https://x.test|Ada 12:00>"), Some(&at("1700000000.000000")), NOW, utc(), false);
    assert!(spans.iter().all(|s| !is_header(s.kind)), "{spans:?}");
}

/// A label is one line and holds no control: a line end becomes a space, any other control U+FFFD; a mention's or channel's
/// name also loses the invisible and direction characters a header's name loses. A token with a label of nothing shown shows
/// its id.
#[test]
fn a_label_is_one_line_and_holds_no_control() {
    let b = |t: &str| body(&rec(t));
    let s = |k, t: &str| (k, t.to_string());
    assert_eq!(b("<https://x.test|a\nb\r\nc\rd\0e\tf\u{85}g>"), [s(Kind::Link, "a b c d\u{fffd}e\u{fffd}f\u{fffd}g")]);
    assert_eq!(b("<https://x.test/a\nb>"), [s(Kind::Link, "https://x.test/a b")], "a URL shown as the label is one line too");
    assert_eq!(b("<@U1|a\nAda 12:00\0b>"), [s(Kind::Mention, "@a Ada 12:00\u{fffd}b")]);
    assert_eq!(b("<#C1|a\u{202e}b\u{200b}c\u{2028}d>"), [s(Kind::Mention, "#abcd")]);
    assert_eq!(b("<!subteam^S1|@te\u{2067}am>"), [s(Kind::Mention, "@team")]);
    assert_eq!(b("<@U1|\u{200b}\u{202e}>"), [s(Kind::Mention, "@U1")], "nothing left shows the id");
    assert_eq!(b("<!date^1^x|a\nb>"), [s(Kind::Body, "a b")]);
    // No span a token makes holds a line end or any control.
    let spans = project(&rec("<https://x.test|a\nb> <@U1|c\nd> <#C1|e\0f> <!subteam^S1|g\th> <!date^1^x|i\nj>"), None, NOW, utc(), false);
    for x in spans.iter().filter(|x| matches!(x.kind, Kind::Body | Kind::Link | Kind::Mention)) {
        assert!(x.text.chars().all(|c| !c.is_control()), "{x:?}");
    }
}

/// No markup makes a header: a body that styles its first words like a header's has the body's kind, never the name's or
/// the time's, and its styles are bits on body spans.
#[test]
fn no_body_markup_makes_a_header() {
    let first = at("1700000000.000000");
    let mut next = at("1700000060.000000");
    next.text = "*Ada* _2:14 PM_ `ROW`".to_string();
    let spans = project(&next, Some(&first), NOW, utc(), false);
    assert!(spans.iter().all(|s| s.kind == Kind::Body && !is_header(s.kind)), "{spans:?}");
    let shown: Vec<_> = spans.iter().map(|s| (s.text.as_str(), s.style)).collect();
    assert_eq!(shown, [("Ada", BOLD), (" ", 0), ("2:14 PM", ITALIC), (" ", 0), ("ROW", CODE)]);
    // Alone, the record has one header, and the styled body follows it.
    let all = project(&next, None, NOW, utc(), false);
    assert_eq!(all.iter().filter(|s| is_header(s.kind)).count(), 2);
}

/// The kinds only a header has.
fn is_header(kind: Kind) -> bool {
    matches!(kind, Kind::Name | Kind::Time | Kind::Badge)
}

fn lines(r: &ChatRecord, now: i64, off: chrono::FixedOffset) -> String {
    project(r, None, now, off, false).iter().filter(|s| s.kind != Kind::Day).map(|s| s.text.as_str()).collect()
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
    assert_eq!(lines(&r, NOW, east(14)), "Ada  7:00 PM\nx");
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
    assert_eq!(kinds, [Kind::Day, Kind::Name, Kind::Badge, Kind::Time, Kind::Body]);
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
    let shown: String = project(&r, None, NOW, utc(), true).iter().filter(|s| s.kind != Kind::Day).map(|s| s.text.as_str()).collect();
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
    assert_eq!(kinds(&counted, Some(&first), false), [(Kind::Replies, "2 replies".to_string())]);
    assert_eq!(kinds(&counted, None, false)[1].0, Kind::Name);
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

/// The stamp of a UTC date and time.
fn stamp(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> String {
    format!("{}.000000", chrono::NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap().and_utc().timestamp())
}

/// A divider's label without its padding and line end.
fn label(r: &ChatRecord, prev: Option<&ChatRecord>, now: i64, off: chrono::FixedOffset) -> Option<String> {
    let first = project(r, prev, now, off, false).into_iter().next()?;
    (first.kind == Kind::Day).then(|| first.text.trim_matches(|c| c == '\u{a0}' || c == '\n').to_string())
}

/// A record that starts a local day carries a divider before its header, as does the oldest record shown; the header shows the
/// time alone, whatever the day; a continuation and a body that imitates a divider carry none.
#[test]
fn a_day_starts_with_its_divider_and_the_header_shows_the_time_alone() {
    let (late, early) = (at("1699919880.000000"), at("1699920060.000000"));
    let shown = |r: &ChatRecord, prev: Option<&ChatRecord>, off| project(r, prev, NOW, off, false).into_iter().map(|s| (s.kind, s.text)).collect::<Vec<_>>();
    let s = shown(&early, Some(&late), utc());
    assert_eq!(s[0], (Kind::Day, "\u{a0}\u{a0}\u{a0}Today\u{a0}\u{a0}\u{a0}\n".to_string()), "{s:?}");
    assert_eq!(s[1], (Kind::Name, "Ada  ".to_string()));
    assert_eq!(s[2], (Kind::Time, "12:01 AM\n".to_string()), "the time alone");
    assert_eq!(label(&early, None, NOW, utc()).as_deref(), Some("Today"), "the oldest shown");
    // An hour west the two are one local day: a continuation, so no divider and no header.
    assert_eq!(shown(&early, Some(&late), east(-1)), [(Kind::Body, "x".to_string())]);
    // A record on a day that is not today shows its time alone under its day's divider.
    let old = at("1699833600.000000");
    assert_eq!(label(&old, Some(&at("1699700000.000000")), NOW, utc()).as_deref(), Some("Yesterday"));
    assert_eq!(shown(&old, None, utc())[2], (Kind::Time, "12:00 AM\n".to_string()));
    // The same local day, a different author: a new header and no divider.
    let mut other = early.clone();
    other.author.name = "Eve".to_string();
    assert_eq!(shown(&other, Some(&early), utc())[0].0, Kind::Name);
    // A stamp that is not a time next to one that is: the day is unknown, and it has no divider unless it is the oldest.
    let mut raw = at("8210266876798.000000");
    assert_eq!(label(&raw, Some(&early), NOW, east(14)).as_deref(), Some("8210266876798.000000"));
    raw.ts = sot_protocol::chat::Ts::parse("8210266876799.000000").unwrap();
    let mut next = raw.clone();
    next.ts = sot_protocol::chat::Ts::parse("8210266876800.000000").unwrap();
    assert_eq!(label(&next, Some(&raw), NOW, east(14)), None);
    // A body of a divider's words is body text.
    let mut fake = at("1700000060.000000");
    fake.text = "Today\nYesterday".to_string();
    assert!(project(&fake, Some(&at("1700000000.000000")), NOW, utc(), false).iter().all(|s| s.kind == Kind::Body));
}

/// `Today` and `Yesterday` by the local date, else the weekday, month and ordinal day, and the year when it is not this one.
#[test]
fn the_divider_says_today_yesterday_or_the_date() {
    let at = |y, mo, d| label(&at(&stamp(y, mo, d, 12, 0)), None, NOW, utc()).unwrap();
    assert_eq!(at(2023, 11, 14), "Today");
    assert_eq!(at(2023, 11, 13), "Yesterday");
    assert_eq!(at(2023, 11, 9), "Thursday, November 9th");
    for (d, want) in [(1, "Sunday, October 1st"), (2, "Monday, October 2nd"), (3, "Tuesday, October 3rd"), (4, "Wednesday, October 4th"),
        (11, "Wednesday, October 11th"), (12, "Thursday, October 12th"), (13, "Friday, October 13th"), (21, "Saturday, October 21st"),
        (22, "Sunday, October 22nd"), (23, "Monday, October 23rd"), (31, "Tuesday, October 31st")] {
        assert_eq!(at(2023, 10, d), want);
    }
    assert_eq!(at(2020, 9, 13), "Sunday, September 13th, 2020");
    assert_eq!(at(2022, 11, 13), "Sunday, November 13th, 2022", "the same day a year before is not yesterday");
    // The local date decides: at +14 now is the 15th, so the 14th is yesterday; a stamp out of range is its own text.
    let r = self::at("1699938000.000000");
    assert_eq!(label(&r, None, NOW, east(14)).as_deref(), Some("Yesterday"));
    assert_eq!(label(&r, None, NOW, utc()).as_deref(), Some("Today"));
    assert_eq!(label(&self::at("8210266876798.000000"), None, NOW, east(14)).as_deref(), Some("8210266876798.000000"));
    // A `now` out of range cannot say today: the date and its year.
    assert_eq!(label(&r, None, i64::MAX, utc()).as_deref(), Some("Tuesday, November 14th, 2023"));
}

/// `2 replies` (`1 reply`) in the accent, then `Last reply` and the day as the divider says it, in the muted color.
#[test]
fn the_thread_summary_reads_as_slacks() {
    let summary = |count, last: Option<String>| {
        let mut r = at("1700000000.000000");
        r.text = String::new();
        r.reply_count = count;
        r.latest_reply = last.map(|t| sot_protocol::chat::Ts::parse(&t).unwrap());
        project(&r, Some(&at("1699999990.000000")), NOW, utc(), false).into_iter().map(|s| (s.kind, s.text)).collect::<Vec<_>>()
    };
    let last = |y, mo, d, h, mi| summary(2, Some(stamp(y, mo, d, h, mi)));
    assert_eq!(last(2023, 11, 14, 14, 44), [(Kind::Replies, "2 replies".to_string()), (Kind::Last, "   Last reply today at 2:44 PM".to_string())]);
    assert_eq!(last(2023, 11, 13, 9, 5)[1].1, "   Last reply yesterday at 9:05 AM");
    assert_eq!(last(2023, 11, 9, 0, 0)[1].1, "   Last reply Nov 9 at 12:00 AM");
    assert_eq!(last(2020, 9, 13, 12, 26)[1].1, "   Last reply Sep 13, 2020 at 12:26 PM");
    assert_eq!(summary(1, None), [(Kind::Replies, "1 reply".to_string())]);
    assert_eq!(summary(0, Some(stamp(2023, 11, 14, 14, 44))), []);
    let mut r = at("1700000000.000000");
    r.reply_count = 3;
    r.latest_reply = Some(sot_protocol::chat::Ts::parse("8210266876798.000000").unwrap());
    assert_eq!(project(&r, Some(&at("1699999990.000000")), NOW, east(14), false)[2].text, "   Last reply 8210266876798.000000");
    // Its line starts after the body, and the next meta item starts its own line.
    let mut r = at("1700000000.000000");
    r.reply_count = 2;
    r.truncated = true;
    let s: Vec<_> = project(&r, Some(&at("1699999990.000000")), NOW, utc(), false).into_iter().map(|s| (s.kind, s.text)).collect();
    assert_eq!(s, [(Kind::Body, "x".to_string()), (Kind::Replies, "\n2 replies".to_string()), (Kind::Meta, "\n(cut)".to_string())]);
}

/// `(edited)` ends the body of a message Slack marked edited, on the line of its last word, or on its own line after a
/// block or a quote; never on a deleted message, and never from what a poster typed.
#[test]
fn an_edited_message_ends_with_edited() {
    let edited = |text: &str| {
        let mut v = serde_json::to_value(rec(text)).unwrap();
        v["edited"] = true.into();
        serde_json::from_value::<ChatRecord>(v).unwrap()
    };
    let all = |r: &ChatRecord| spans(r).into_iter().map(|s| s.text).collect::<String>();
    assert_eq!(all(&edited("hello")), "Ada  10:13 PM\nhello (edited)");
    assert_eq!(spans(&edited("hello")).last().map(|s| s.kind), Some(Kind::Edited));
    assert_eq!(all(&rec("hello (edited)")), "Ada  10:13 PM\nhello (edited)");
    assert!(spans(&rec("hello (edited)")).iter().all(|s| s.kind != Kind::Edited));
    assert_eq!(all(&edited("")), "Ada  10:13 PM\n(edited)");
    assert_eq!(all(&edited("two\nlines\n")), "Ada  10:13 PM\ntwo\nlines\n(edited)");
    assert_eq!(all(&edited("```code```")), "Ada  10:13 PM\ncode\n(edited)");
    assert_eq!(all(&edited("&gt; quoted")), "Ada  10:13 PM\nquoted\n(edited)");
    let mut cut = edited("hello");
    cut.truncated = true;
    assert_eq!(all(&cut), "Ada  10:13 PM\nhello (edited)\n(cut)");
    let tomb = project(&edited("hello"), None, NOW, utc(), true).into_iter().map(|s| s.text).collect::<String>();
    assert!(!tomb.contains("(edited)"), "{tomb}");
}

/// A person's mention shows the name the record carries for its id, else the token's label, else the id; the name loses the
/// invisible and direction characters a header's name loses, and nothing a poster types in a label or a text makes a name.
#[test]
fn a_mention_shows_its_name_or_its_label_or_its_id() {
    let mut r = rec("");
    let name = |id: &str, name: &str| sot_protocol::chat::ChatName { id: id.into(), name: name.into() };
    r.names = vec![name("U1", "Ada Lovelace"), name("W2", "Grace"), name("U5", "\u{200b}\u{202e}"), name("U6", "Gr\u{202e}ace"), name("C1", "Chan")];
    let at = |text: &str| { let mut r = r.clone(); r.text = text.into(); body(&r) };
    let shown = |t: &str| [(Kind::Mention, t.to_string())];
    assert_eq!(at("<@U1>"), shown("@Ada Lovelace"));
    assert_eq!(at("<@U1|label>"), shown("@Ada Lovelace"), "the name beats the label");
    assert_eq!(at("<@W2>"), shown("@Grace"));
    assert_eq!(at("<@U3|label>"), shown("@label"));
    assert_eq!(at("<@U4>"), shown("@U4"));
    assert_eq!(at("<@U5|label>"), shown("@label"), "a name nothing visible is left of is no name");
    assert_eq!(at("<@U6>"), shown("@Grace"));
    // A name belongs to its id alone, and only to a person's token: a channel, a group or typed text is never renamed.
    assert_eq!(at("<#C1>"), shown("#C1"));
    assert_eq!(at("<!here>"), shown("@here"));
    assert_eq!(at("&lt;@U1&gt;"), [(Kind::Body, "<@U1>".to_string())]);
    assert_eq!(at("hi <@U1>, <@U1|x>!"), [
        (Kind::Body, "hi ".to_string()), (Kind::Mention, "@Ada Lovelace".to_string()), (Kind::Body, ", ".to_string()),
        (Kind::Mention, "@Ada Lovelace".to_string()), (Kind::Body, "!".to_string()),
    ]);
}
