use super::*;

fn record() -> ChatRecord {
    ChatRecord { workspace: "W1".into(), origin: Origin::Drawer, ..ChatRecord::fixture() }
}

#[test]
fn a_timestamp_has_exactly_six_fraction_digits() {
    assert!(Ts::parse("1700000000.123456").is_ok());
    assert!(Ts::parse("0.000000").is_ok());
    for bad in [
        "",
        ".",
        "1700000000",
        "1700000000.",
        "1700000000.12345",
        "1700000000.1234567",
        "1700000000.7309489999",
        ".123456",
        "17x0.123456",
        "1700000000.12345a",
        "-1.123456",
        "1.2.123456",
        " 1.123456",
        "1.123456\n",
        "123456789012345678901.123456",
        "1７.123456",
    ] {
        assert!(Ts::parse(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn a_bad_timestamp_in_json_is_refused() {
    assert!(serde_json::from_str::<Ts>("\"1700000000.1234567\"").is_err());
    assert!(serde_json::from_str::<Ts>("1700000000.123456").is_err());
    assert_eq!(
        serde_json::from_str::<Ts>("\"1700000000.123456\"")
            .unwrap()
            .as_str(),
        "1700000000.123456"
    );
}

#[test]
fn the_record_wire_shape_is_stable() {
    let mut r = record();
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        r#"{"workspace":"W1","channel":"C1","ts":"1700000000.000100","thread_ts":"1700000000.000100","author":{"id":"U1","name":"Ada"},"origin":"drawer","text":"hi","reply_count":0,"files":[]}"#
    );
    r.truncated = true;
    r.limited = true;
    r.omitted = true;
    r.files_omitted = 2;
    r.edited = true;
    r.latest_reply = Some(Ts::parse("1700000001.000000").unwrap());
    r.files.push(ChatFile {
        id: "F1".into(),
        name: "a.png".into(),
        mime: "image/png".into(),
        bytes: Some(7),
        thumbnail: true,
    });
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        r#"{"workspace":"W1","channel":"C1","ts":"1700000000.000100","thread_ts":"1700000000.000100","author":{"id":"U1","name":"Ada"},"origin":"drawer","text":"hi","truncated":true,"reply_count":0,"files":[{"id":"F1","name":"a.png","mime":"image/png","bytes":7,"thumbnail":true}],"latest_reply":"1700000001.000000","limited":true,"omitted":true,"files_omitted":2,"edited":true}"#
    );
}

#[test]
fn a_record_round_trips_and_an_older_shape_without_the_optional_fields_parses() {
    let mut r = record();
    r.truncated = true;
    r.latest_reply = Some(Ts::parse("1700000001.000000").unwrap());
    let back: ChatRecord = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back, r);
    let bare = r#"{"workspace":"W1","channel":"C1","ts":"1.000001","thread_ts":"1.000001","author":{"id":"U1","name":""},"origin":"bot","text":"","reply_count":3,"files":[]}"#;
    let parsed: ChatRecord = serde_json::from_str(bare).unwrap();
    assert!(!parsed.truncated && !parsed.limited && !parsed.omitted);
    assert_eq!((parsed.files_omitted, parsed.latest_reply), (0, None));
    assert_eq!(parsed.origin, Origin::Bot);
}

fn decode(r: &ChatRecord) -> Result<ChatRecord, serde_json::Error> {
    serde_json::from_str(&serde_json::to_string(r).unwrap())
}

fn a_file() -> ChatFile {
    ChatFile { id: "F1".into(), name: "a".into(), mime: "image/png".into(), bytes: None, thumbnail: false }
}

#[test]
fn a_record_is_bounded_at_decode() {
    let ok = |f: &dyn Fn(&mut ChatRecord)| {
        let mut r = record();
        f(&mut r);
        decode(&r).is_ok()
    };
    assert!(ok(&|r| r.text = "t".repeat(MAX_TEXT)));
    assert!(!ok(&|r| r.text = "t".repeat(MAX_TEXT + 1)));
    assert!(ok(&|r| r.channel = "c".repeat(MAX_ID)));
    assert!(!ok(&|r| r.channel = "c".repeat(MAX_ID + 1)));
    assert!(!ok(&|r| r.workspace = "w".repeat(MAX_ID + 1)));
    assert!(!ok(&|r| r.author.id = "u".repeat(MAX_ID + 1)));
    assert!(!ok(&|r| r.author.name = "n".repeat(MAX_NAME + 1)));
    assert!(ok(&|r| r.files = vec![a_file(); MAX_FILES]));
    assert!(!ok(&|r| r.files = vec![a_file(); MAX_FILES + 1]));
    assert!(!ok(&|r| r.files = vec![ChatFile { id: "i".repeat(MAX_ID + 1), ..a_file() }]));
    assert!(!ok(&|r| r.files = vec![ChatFile { name: "n".repeat(MAX_FILENAME + 1), ..a_file() }]));
    assert!(!ok(&|r| r.files = vec![ChatFile { mime: "m".repeat(MAX_MIME + 1), ..a_file() }]));
}

fn text(s: &str) -> TextSegment {
    TextSegment::Text(s.into())
}

fn markup(s: &str) -> TextSegment {
    TextSegment::Markup(s.into())
}

#[test]
fn slack_text_decodes_entities_once_delimited_and_escape_inverts_it() {
    assert_eq!(text_segments("if a &lt; b &amp;&amp; c &gt; d"), [text("if a < b && c > d")]);
    assert_eq!(text_segments("&lt;@U1&gt; <@U1>"), [text("<@U1> "), markup("<@U1>")]);
    assert_eq!(
        text_segments("<https://x?a=1&amp;b=2|a &amp; b>"),
        [markup("<https://x?a=1&b=2|a & b>")]
    );
    assert_eq!(text_segments("hi <@U12"), [text("hi "), markup("<@U12")]);
    assert_eq!(text_segments("&amp;lt;"), [text("&lt;")]);
    assert_eq!(text_segments("&nbsp; &"), [text("&nbsp; &")]);
    assert_eq!(text_segments("<@U1><@U2>x"), [markup("<@U1>"), markup("<@U2>"), text("x")]);
    assert!(text_segments("").is_empty());

    assert_eq!(escape_text("<&>"), "&lt;&amp;&gt;");
    for s in ["&lt;", "&gt;", "<&>", "<@U1>", "", "a & b < c > d", "&amp;lt;"] {
        let segments = text_segments(&escape_text(s));
        assert!(segments.iter().all(|g| matches!(g, TextSegment::Text(_))), "{s:?}");
        let joined: String = segments
            .iter()
            .map(|g| match g {
                TextSegment::Text(t) | TextSegment::Markup(t) => t.as_str(),
            })
            .collect();
        assert_eq!(joined, s);
    }

    assert!(post_text_fits(&"&".repeat(1638)));
    assert!(!post_text_fits(&"&".repeat(1639)));
    assert!(post_text_fits(&"a".repeat(8192)));
    assert!(!post_text_fits(&"a".repeat(8193)));
    assert!(!post_text_fits(&format!("{}a", "\u{e9}".repeat(4096))));
}

#[test]
fn an_edited_flag_is_additive() {
    let r = record();
    let key = |r: &ChatRecord| serde_json::to_value(r).unwrap().get("edited").cloned();
    assert_eq!(key(&r), None);
    assert!(!r.edited);
    let mut v = serde_json::to_value(&r).unwrap();
    v["edited"] = true.into();
    let parsed: ChatRecord = serde_json::from_value(v).unwrap();
    assert_eq!(key(&parsed), Some(true.into()));
    assert!(parsed.edited);
    // An older peer's record has no key and reads as not edited; a key of another type is refused.
    assert_eq!(key(&decode(&r).unwrap()), None);
    let mut v = serde_json::to_value(&r).unwrap();
    v["edited"] = "yes".into();
    assert!(serde_json::from_value::<ChatRecord>(v).is_err());
}
