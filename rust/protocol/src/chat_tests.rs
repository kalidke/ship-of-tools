use super::*;

fn record() -> ChatRecord {
    ChatRecord {
        workspace: "W1".into(),
        channel: "C1".into(),
        ts: Ts::parse("1700000000.000100").unwrap(),
        thread_ts: Ts::parse("1700000000.000100").unwrap(),
        author: ChatAuthor {
            id: "U1".into(),
            name: "Ada".into(),
        },
        origin: Origin::Drawer,
        text: "hi".into(),
        truncated: false,
        reply_count: 0,
        files: vec![],
        latest_reply: None,
        limited: false,
        omitted: false,
        files_omitted: 0,
    }
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
        r#"{"workspace":"W1","channel":"C1","ts":"1700000000.000100","thread_ts":"1700000000.000100","author":{"id":"U1","name":"Ada"},"origin":"drawer","text":"hi","truncated":true,"reply_count":0,"files":[{"id":"F1","name":"a.png","mime":"image/png","bytes":7,"thumbnail":true}],"latest_reply":"1700000001.000000","limited":true,"omitted":true,"files_omitted":2}"#
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
