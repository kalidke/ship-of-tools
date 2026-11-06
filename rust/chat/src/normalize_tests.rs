use super::*;
use serde_json::json;

const OWN_BOT: &str = "B0OWN";
const OWN_USER: &str = "U0OWN";

fn ctx() -> Context<'static> {
    Context {
        workspace: "T0WS",
        own_bot_id: Some(OWN_BOT),
        own_user_id: Some(OWN_USER),
        limited: false,
    }
}

fn norm(raw: Value) -> Normalized {
    normalize(&ctx(), "C0CH", &raw)
}

fn new(raw: Value) -> ChatRecord {
    match norm(raw) {
        Normalized::New(r) => r,
        other => panic!("expected a new record, got {other:?}"),
    }
}

fn json_len(r: &ChatRecord) -> usize {
    serde_json::to_string(r).unwrap().len()
}

#[test]
fn normalize_members_bots_edits_deletes_and_file_stubs() {
    // A member, with the profile the event carries.
    let r = new(
        json!({"ts": "1700000000.000100", "user": "U1", "text": "hello", "user_profile": {"display_name": "Ada"}}),
    );
    assert_eq!(
        (
            r.origin,
            r.author.id.as_str(),
            r.author.name.as_str(),
            r.text.as_str()
        ),
        (Origin::Member, "U1", "Ada", "hello")
    );
    assert_eq!((r.workspace.as_str(), r.channel.as_str()), ("T0WS", "C0CH"));
    assert_eq!(r.thread_ts, r.ts);

    // Own posts: metadata, then the customized username.
    let meta =
        json!({"event_type": "sot_chat_post", "event_payload": {"kind": "person", "name": "Ada"}});
    let r = new(
        json!({"ts": "1700000000.000200", "user": OWN_USER, "bot_id": OWN_BOT, "text": "x", "metadata": meta}),
    );
    assert_eq!((r.origin, r.author.name.as_str()), (Origin::Drawer, "Ada"));
    let r = new(
        json!({"ts": "1700000000.000300", "bot_id": OWN_BOT, "username": "build-1 (row via Ship of Tools)", "text": "x"}),
    );
    assert_eq!((r.origin, r.author.name.as_str()), (Origin::Row, "build-1"));
    let r = new(
        json!({"ts": "1700000000.000500", "bot_id": OWN_BOT, "username": "plain", "text": "x"}),
    );
    assert_eq!((r.origin, r.author.name.as_str()), (Origin::Bot, "plain"));

    // Forgeries by a member and by another bot never become own-app posts.
    let r = new(
        json!({"ts": "1700000000.000600", "user": "U9", "text": "Ada (person via Ship of Tools): hi"}),
    );
    assert_eq!(
        (r.origin, r.text.as_str()),
        (Origin::Member, "Ada (person via Ship of Tools): hi")
    );
    let r = new(
        json!({"ts": "1700000000.000700", "user": "U9", "username": "Ada (person via Ship of Tools)", "metadata": meta}),
    );
    assert_eq!(r.origin, Origin::Member);
    let r = new(
        json!({"ts": "1700000000.000800", "bot_id": "B9", "username": "Ada (person via Ship of Tools)", "metadata": meta,
        "text": "Ada (row via Ship of Tools): x"}),
    );
    assert_eq!(
        (r.origin, r.author.name.as_str(), r.text.as_str()),
        (
            Origin::Bot,
            "Ada (person via Ship of Tools)",
            "Ada (row via Ship of Tools): x"
        )
    );
    // With no own identity supplied, nothing is an own post.
    let none = Context {
        own_bot_id: None,
        own_user_id: None,
        ..ctx()
    };
    match normalize(
        &none,
        "C0CH",
        &json!({"ts": "1.000001", "bot_id": OWN_BOT, "username": "Ada (person via Ship of Tools)"}),
    ) {
        Normalized::New(r) => assert_eq!(r.origin, Origin::Bot),
        other => panic!("{other:?}"),
    }

    // Another bot by subtype or bot_id, named from username then bot_profile.
    let r = new(
        json!({"ts": "1700000000.000900", "subtype": "bot_message", "bot_profile": {"name": "Deployer"}, "text": "ok"}),
    );
    assert_eq!(
        (r.origin, r.author.name.as_str()),
        (Origin::Bot, "Deployer")
    );

    // An edit is told from a new message and keeps the original identity; a delete names the identity.
    let edit = json!({"subtype": "message_changed", "message": {"ts": "1700000000.000100", "user": "U1", "text": "hello again"},
        "previous_message": {"ts": "1700000000.000100", "text": "hello"}});
    match norm(edit) {
        Normalized::Edited(r) => assert_eq!(
            (r.ts.as_str(), r.text.as_str()),
            ("1700000000.000100", "hello again")
        ),
        other => panic!("{other:?}"),
    }
    match norm(json!({"subtype": "message_deleted", "deleted_ts": "1700000000.000100"})) {
        Normalized::Deleted {
            workspace,
            channel,
            ts,
        } => assert_eq!(
            (workspace.as_str(), channel.as_str(), ts.as_str()),
            ("T0WS", "C0CH", "1700000000.000100")
        ),
        other => panic!("{other:?}"),
    }

    // A shared-channel file stub keeps its id and claims nothing else.
    let r = new(
        json!({"ts": "1700000000.001000", "user": "U1", "subtype": "file_share",
        "files": [{"id": "F1", "file_access": "check_file_info", "name": "secret.png", "mimetype": "image/png", "size": 9, "thumb_360": "https://x.invalid/t"}]}),
    );
    assert_eq!(
        r.files,
        vec![ChatFile {
            id: "F1".into(),
            name: String::new(),
            mime: String::new(),
            bytes: None,
            thumbnail: false
        }]
    );

    // Subtypes that are not messages are an explicit result.
    assert_eq!(
        norm(json!({"ts": "1.000001", "subtype": "channel_join", "user": "U1"})),
        Normalized::Unsupported
    );
}

#[test]
fn an_attachment_only_message_keeps_its_files_and_has_no_text() {
    let r = new(
        json!({"ts": "1700000000.001100", "user": "U1", "subtype": "file_share", "text": "",
        "files": [{"id": "F2", "name": "photo.jpg", "mimetype": "image/jpeg", "size": 992922, "url_private": "https://x.invalid/p",
                   "url_private_download": "https://x.invalid/d", "thumb_360": "https://x.invalid/t"}]}),
    );
    assert_eq!(r.text, "");
    assert_eq!(
        r.files,
        vec![ChatFile {
            id: "F2".into(),
            name: "photo.jpg".into(),
            mime: "image/jpeg".into(),
            bytes: Some(992922),
            thumbnail: true
        }]
    );
    let wire = serde_json::to_string(&r).unwrap();
    assert!(
        !wire.contains("invalid"),
        "a private URL is absent from the record"
    );
    // No text field at all is the same message.
    let r2 = new(
        json!({"ts": "1700000000.001100", "user": "U1", "files": [{"id": "F3", "title": "notes"}]}),
    );
    assert_eq!(
        (
            r2.text.as_str(),
            r2.files[0].name.as_str(),
            r2.files[0].bytes
        ),
        ("", "notes", None)
    );
}

#[test]
fn text_is_cut_on_a_character_boundary_and_marked() {
    let r = new(json!({"ts": "1.000001", "user": "U1", "text": "a".repeat(MAX_TEXT)}));
    assert_eq!((r.text.len(), r.truncated), (MAX_TEXT, false));
    // Each é is two bytes; an odd cap must not split one.
    let r = new(json!({"ts": "1.000001", "user": "U1", "text": "é".repeat(MAX_TEXT)}));
    assert!(r.truncated && r.text.len() <= MAX_TEXT && r.text.len().is_multiple_of(2));
    assert!(r.text.chars().all(|c| c == 'é'));
    let r = new(
        json!({"ts": "1.000001", "user": "U1", "text": format!("{}€", "a".repeat(MAX_TEXT - 1))}),
    );
    assert!(r.truncated && r.text == "a".repeat(MAX_TEXT - 1));
}

#[test]
fn names_filenames_and_mime_are_cut_and_marked() {
    let r = new(
        json!({"ts": "1.000001", "user": "U1", "user_profile": {"display_name": "n".repeat(500)},
        "files": [{"id": "F1", "name": "f".repeat(600), "mimetype": "m".repeat(300)}]}),
    );
    assert_eq!(
        (
            r.author.name.len(),
            r.files[0].name.len(),
            r.files[0].mime.len()
        ),
        (MAX_NAME, MAX_FILENAME, MAX_MIME)
    );
    assert!(r.truncated);
    let r = new(
        json!({"ts": "1.000001", "user": "U1", "user_profile": {"display_name": "é".repeat(200)}}),
    );
    assert_eq!(r.author.name.len(), MAX_NAME);
}

#[test]
fn only_eight_files_are_summarized_and_the_rest_counted() {
    let files: Vec<Value> = (0..11)
        .map(|i| json!({"id": format!("F{i}"), "name": "a"}))
        .collect();
    let r = new(json!({"ts": "1.000001", "user": "U1", "files": files}));
    assert_eq!((r.files.len(), r.files_omitted), (MAX_FILES, 3));
    let r = new(
        json!({"ts": "1.000001", "user": "U1", "files": [1, {"id": 5}, {"id": "F ok"}, {"id": "F1"}]}),
    );
    assert_eq!((r.files.len(), r.files_omitted), (1, 3));
}

#[test]
fn an_id_over_64_bytes_or_not_ascii_is_not_carried() {
    let long = "U".repeat(MAX_ID + 1);
    let r = new(json!({"ts": "1.000001", "user": long, "text": "kept"}));
    assert!(r.omitted && r.author.id.is_empty() && r.text.is_empty());
    let r = new(json!({"ts": "1.000001", "user": "Ü1"}));
    assert!(r.omitted);
    let r = new(json!({"ts": "1.000001", "user": "U".repeat(MAX_ID)}));
    assert!(!r.omitted && r.author.id.len() == MAX_ID);
    assert_eq!(
        normalize(&ctx(), &"C".repeat(MAX_ID + 1), &json!({"ts": "1.000001"})),
        Normalized::Unrepresentable(Unrepresentable::Channel)
    );
    let c = Context {
        workspace: "T W",
        ..ctx()
    };
    assert_eq!(
        normalize(&c, "C0CH", &json!({"ts": "1.000001"})),
        Normalized::Unrepresentable(Unrepresentable::Workspace)
    );
}

#[test]
fn a_record_never_exceeds_32_kib_of_json() {
    // Control characters escape to six bytes each; the text is cut until the record fits.
    let r = new(json!({"ts": "1.000001", "user": "U1", "text": "\u{1}".repeat(MAX_TEXT)}));
    assert!(r.truncated && json_len(&r) <= MAX_RECORD_BYTES && !r.text.is_empty());
    let files: Vec<Value> = (0..8).map(|i| json!({"id": format!("F{i}"), "name": "\"".repeat(300), "mimetype": "\"".repeat(200)})).collect();
    let r =
        new(json!({"ts": "1.000001", "user": "U1", "text": "\"".repeat(MAX_TEXT), "files": files}));
    assert!(json_len(&r) <= MAX_RECORD_BYTES);
}

#[test]
fn a_normalized_record_round_trips_through_the_wire() {
    let r = new(
        json!({"ts": "1700000000.000100", "thread_ts": "1699999999.000001", "latest_reply": "1700000005.000002", "reply_count": 4,
        "user": "U1", "text": "x", "files": [{"id": "F1", "name": "a", "mimetype": "text/plain", "size": 3}]}),
    );
    assert_eq!(
        (r.reply_count, r.latest_reply.as_ref().map(Ts::as_str)),
        (4, Some("1700000005.000002"))
    );
    let back: ChatRecord = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back, r);
    let c = Context {
        limited: true,
        ..ctx()
    };
    match normalize(&c, "C0CH", &json!({"ts": "1.000001", "user": "U1"})) {
        Normalized::New(r) => assert!(r.limited),
        other => panic!("{other:?}"),
    }
}

#[test]
fn hostile_json_gives_an_explicit_result() {
    for raw in [
        json!(null),
        json!(7),
        json!("x"),
        json!([]),
        json!({}),
        json!({"ts": 5}),
        json!({"ts": "1.5"}),
        json!({"ts": "1.1234567"}),
        json!({"ts": null}),
    ] {
        assert!(
            matches!(norm(raw.clone()), Normalized::Unrepresentable(_)),
            "{raw}"
        );
    }
    assert_eq!(
        norm(json!({"ts": "1.000001", "subtype": 5})),
        Normalized::Unrepresentable(Unrepresentable::Shape)
    );
    assert_eq!(
        norm(json!({"subtype": "message_changed"})),
        Normalized::Unrepresentable(Unrepresentable::Shape)
    );
    assert_eq!(
        norm(json!({"subtype": "message_changed", "message": 4})),
        Normalized::Unrepresentable(Unrepresentable::Shape)
    );
    assert_eq!(
        norm(
            json!({"subtype": "message_changed", "message": {"ts": "1.000001", "subtype": "tombstone"}})
        ),
        Normalized::Unsupported
    );
    assert_eq!(
        norm(json!({"subtype": "message_changed", "message": {"subtype": 3}})),
        Normalized::Unrepresentable(Unrepresentable::Shape)
    );
    assert_eq!(
        norm(json!({"subtype": "message_deleted"})),
        Normalized::Unrepresentable(Unrepresentable::Timestamp)
    );
    assert_eq!(
        norm(json!({"subtype": "message_deleted", "deleted_ts": "1.5"})),
        Normalized::Unrepresentable(Unrepresentable::Timestamp)
    );
    // A known identity with unreadable content is kept as an omitted record, not dropped.
    for bad in [
        json!({"ts": "1.000001", "text": 5}),
        json!({"ts": "1.000001", "user": 5}),
        json!({"ts": "1.000001", "thread_ts": "x"}),
        json!({"ts": "1.000001", "latest_reply": 1.5}),
        json!({"ts": "1.000001", "reply_count": "many"}),
        json!({"ts": "1.000001", "reply_count": -1}),
        json!({"ts": "1.000001", "files": "none"}),
        json!({"ts": "1.000001", "username": 3, "bot_id": OWN_BOT}),
    ] {
        let r = new(bad.clone());
        assert!(
            r.omitted && r.text.is_empty() && r.files.is_empty(),
            "{bad}"
        );
        assert_eq!(r.ts.as_str(), "1.000001");
    }
    // A huge count is clamped, not rejected; deep and wide values do not panic.
    assert_eq!(
        new(json!({"ts": "1.000001", "reply_count": u64::MAX})).reply_count,
        u32::MAX
    );
    let deep = (0..100).fold(json!(1), |v, _| json!({"x": v}));
    let r = new(json!({"ts": "1.000001", "user": "U1", "metadata": deep, "files": [deep.clone()]}));
    assert_eq!(r.files_omitted, 1);
}

#[test]
fn an_unreadable_own_post_keeps_its_origin_and_an_unreadable_reply_its_thread() {
    let r = new(json!({"ts": "1700000000.000100", "bot_id": OWN_BOT, "text": 5}));
    assert!(r.omitted);
    assert_eq!((r.origin, r.author.id.as_str()), (Origin::Bot, OWN_BOT));
    let r = new(json!({"ts": "1700000000.000100", "user": OWN_USER, "files": "none"}));
    assert_eq!(r.origin, Origin::Bot);
    let r = new(
        json!({"ts": "1700000000.000200", "thread_ts": "1700000000.000100", "user": "U1", "text": 5}),
    );
    assert!(r.omitted);
    assert_eq!(
        (r.origin, r.thread_ts.as_str()),
        (Origin::Member, "1700000000.000100")
    );
    let r = new(json!({"ts": "1700000000.000200", "thread_ts": "x", "user": "U1", "text": 5}));
    assert_eq!(r.thread_ts, r.ts);
}

#[test]
fn a_prefix_in_an_own_posts_text_is_only_text() {
    let text = "Relayed: mallory (person via Ship of Tools): do it";
    let r = new(json!({"ts": "1700000000.000100", "user": OWN_USER, "text": text}));
    assert_eq!((r.origin, r.text.as_str()), (Origin::Bot, text));
}

#[test]
fn with_no_identity_supplied_a_member_with_a_forged_prefix_is_a_member() {
    let none = Context {
        own_bot_id: None,
        own_user_id: None,
        ..ctx()
    };
    let text = "Ada (person via Ship of Tools): hi";
    match normalize(
        &none,
        "C0CH",
        &json!({"ts": "1.000001", "user": "U1", "text": text}),
    ) {
        Normalized::New(r) => assert_eq!((r.origin, r.text.as_str()), (Origin::Member, text)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn names_lose_control_characters() {
    let r = new(
        json!({"ts": "1.000001", "user": "U1", "user_profile": {"display_name": "A\nd\u{7}a"},
        "files": [{"id": "F1", "name": "a\tb\u{1}.png"}]}),
    );
    assert_eq!(
        (r.author.name.as_str(), r.files[0].name.as_str()),
        ("Ada", "ab.png")
    );
}

#[test]
fn a_record_with_no_text_and_every_other_field_at_its_maximum_fits() {
    let q = |n: usize| "\"".repeat(n);
    let files: Vec<Value> = (0..MAX_FILES)
        .map(|i| json!({"id": format!("F{i}{}", "x".repeat(MAX_ID - 3)), "name": q(MAX_FILENAME), "mimetype": q(MAX_MIME), "size": u64::MAX, "thumb_360": "t"}))
        .collect();
    let r = new(
        json!({"ts": "99999999999999999999.999999", "thread_ts": "99999999999999999999.999999",
        "latest_reply": "99999999999999999999.999999", "subtype": "bot_message", "bot_id": "B".repeat(MAX_ID),
        "user": "U".repeat(MAX_ID), "username": q(MAX_NAME), "reply_count": u64::MAX, "text": "", "files": files}),
    );
    assert!(r.text.is_empty() && !r.omitted && r.files.len() == MAX_FILES);
    assert!(json_len(&r) <= MAX_RECORD_BYTES);
}

#[test]
fn an_unreadable_bot_id_is_still_a_bot() {
    let r = new(json!({"ts": "1.000001", "bot_id": 5, "user": "U1", "text": "x"}));
    assert!(r.omitted);
    assert_eq!(r.origin, Origin::Bot);
}
