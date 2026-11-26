//! The read and the verdicts of a delete of a post (chat guarantee 36), against the fake Slack's `Posts`. Every id,
//! name and time is synthetic.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::api::delete::{file_verdict, own_post_files, Kept, Listed, Refusal, Verdict, POST_READ_LIMIT};
use crate::api::ApiError;
use crate::api_tests::{bounded, code, info, posts_fake, ready, OWN_BOT, OWN_USER};
use crate::fake_slack::{FakeSlack, Posts};

/// The Slack methods the fake was asked, in order, without `auth.test`.
fn asked(fake: &FakeSlack) -> Vec<String> {
    fake.seen()
        .iter()
        .filter_map(|s| s.path().strip_prefix("/api/").map(String::from))
        .filter(|m| m != "auth.test")
        .collect()
}

fn world() -> Posts {
    Posts::new(OWN_USER, OWN_BOT)
}

fn ts_of(message: &Listed) -> Option<&str> {
    match message {
        Listed::Message(m) => m["ts"].as_str(),
        _ => None,
    }
}

/// The read finds a message by its exact time in one page, a reply in its thread, and it ends a delete when it cannot say
/// whether the message is there: a reply asked for with no thread, a wrong thread, a deleted parent's placeholder and a
/// page that is one of several are never a message to delete.
#[tokio::test]
async fn the_read_finds_the_message_by_its_exact_time_or_ends_the_delete() {
    bounded(async {
        let mut posts = world()
            .bot_post("C0CHAN", "1700000100.000100", None)
            .bot_post("C0CHAN", "1700000100.000500", None)
            .bot_post("C0CHAN", "1700000100.000600", Some("1700000100.000500"))
            .bot_post("C0CHAN", "1700000100.000800", None)
            .tombstone("C0CHAN", "1700000100.000700");
        // A page of a hundred and one messages near the one asked for, which is none of them.
        for i in 1..=101 {
            posts = posts.member_post("C0BUSY", &format!("1700000300.{i:06}"), None, None);
        }
        let fake = posts_fake(&Arc::new(posts)).await;
        let api = ready(&fake).await;
        let find = |thread: Option<&'static str>, ts: &'static str| api.find_post("C0CHAN", thread, ts);

        assert_eq!(ts_of(&find(None, "1700000100.000100").await.unwrap()), Some("1700000100.000100"));
        // The one read: both bounds a second either side, exclusive, the limit, no cursor.
        let read = fake.seen().into_iter().find(|s| s.path() == "/api/conversations.history").unwrap();
        let param = |name: &str| read.param(name);
        assert_eq!(
            [param("channel"), param("oldest"), param("latest"), param("inclusive"), param("limit"), param("cursor")],
            [Some("C0CHAN".into()), Some("1700000099.000100".into()), Some("1700000101.000100".into()), Some("false".into()), Some(POST_READ_LIMIT.to_string()), None]
        );
        assert_eq!(asked(&fake), ["conversations.history"]);

        // A reply is found in its thread, and the parent is found by its own time in the channel or in its thread.
        let reply = find(Some("1700000100.000500"), "1700000100.000600").await.unwrap();
        assert_eq!(ts_of(&reply), Some("1700000100.000600"));
        assert_eq!(ts_of(&find(Some("1700000100.000500"), "1700000100.000500").await.unwrap()), Some("1700000100.000500"));
        assert_eq!(asked(&fake).last().map(String::as_str), Some("conversations.replies"));

        // No thread: the reply is not in the channel's history, so nothing is found and only that read was sent.
        let before = asked(&fake).len();
        assert_eq!(find(None, "1700000100.000600").await, Ok(Listed::Gone));
        assert_eq!(asked(&fake)[before..], ["conversations.history"]);
        // A thread that holds no such reply, and a thread Slack does not hold.
        assert_eq!(find(Some("1700000100.000800"), "1700000100.000600").await, Ok(Listed::Gone));
        assert_eq!(find(Some("1700000100.000900"), "1700000100.000600").await, Ok(Listed::Gone));
        // A time Slack lists nothing at, and the placeholder of a deleted parent that has replies.
        assert_eq!(find(None, "1700000100.000300").await, Ok(Listed::Gone));
        assert_eq!(find(None, "1700000100.000700").await, Ok(Listed::Gone));

        // A hundred and one messages inside the window, none of them the one asked for: the page is not the whole answer.
        assert_eq!(api.find_post("C0BUSY", None, "1700000300.500000").await, Ok(Listed::Incomplete));
        // The message among them, at the page's edge, is still found when the page holds it.
        assert_eq!(ts_of(&api.find_post("C0BUSY", None, "1700000300.000101").await.unwrap()), Some("1700000300.000101"));

        // A time that is not a time is refused before anything is sent.
        let before = fake.seen().len();
        for bad in ["", "1700000100", "1700000100.1", "x.000100"] {
            assert_eq!(api.find_post("C0CHAN", None, bad).await, Err(ApiError::BadBound), "{bad}");
        }
        assert_eq!(api.find_post("bad id", None, "1700000100.000100").await, Err(ApiError::BadRequest));
        assert_eq!(fake.seen().len(), before);
        fake.shutdown().await;
    })
    .await;
}

/// A member's post, a member's post that shares a file and another app's message are not the app's, by the one rule
/// for an own post; the app's own, a share of its own included, is. The read that found each is the only call sent.
#[tokio::test]
async fn a_message_that_is_not_the_apps_is_refused_after_the_read_alone() {
    bounded(async {
        let posts = world()
            .member_post("C0CHAN", "1700000100.000100", None, None)
            .member_post("C0CHAN", "1700000100.000200", None, Some("F0PERS"))
            .bot_post("C0CHAN", "1700000100.000300", None)
            .share("C0CHAN", "1700000100.000400", None, "F0AAAA");
        let fake = posts_fake(&Arc::new(posts)).await;
        let api = ready(&fake).await;
        let mut count = 0;
        for (ts, own) in [("1700000100.000100", false), ("1700000100.000200", false), ("1700000100.000300", true), ("1700000100.000400", true)] {
            let Listed::Message(message) = api.find_post("C0CHAN", None, ts).await.unwrap() else { panic!("{ts} is listed") };
            assert_eq!(api.is_apps_message(&message), Ok(own), "{ts}");
            count += 1;
            assert_eq!(asked(&fake), vec!["conversations.history"; count], "the read alone for {ts}");
        }
        // Another app's message, a message with no author and a bare reference to the bot's id in text are not the app's.
        for other in [
            json!({"ts": "1700000100.000500", "user": "U0OTHERBOT", "bot_id": "B0OTHER", "text": "x"}),
            json!({"ts": "1700000100.000500", "text": "x"}),
            json!({"ts": "1700000100.000500", "user": "U0MEM", "text": format!("{OWN_BOT} {OWN_USER}")}),
            json!({"ts": "1700000100.000500", "user": null, "bot_id": null}),
        ] {
            assert_eq!(api.is_apps_message(&other), Ok(false), "{other}");
        }
        assert_eq!(asked(&fake).len(), count, "a pure rule sends nothing");
        // Before the identity is read, nothing is the app's.
        let unready = crate::api_tests::new_api(&fake);
        assert_eq!(unready.is_apps_message(&json!({"bot_id": OWN_BOT})), Err(ApiError::NoIdentity));
        fake.shutdown().await;
    })
    .await;
}

/// The files of a post come from its message alone: none, one, several once each, and a message whose files cannot be
/// read whole stops the delete.
#[test]
fn a_posts_files_are_read_from_the_message_alone() {
    let ids = |message: Value| own_post_files(&message);
    let entry = |id: &str| json!({"id": id, "name": "n.png"});
    let many = |n: usize| json!({"files": (0..n).map(|i| entry(&format!("F0N{i:04}"))).collect::<Vec<_>>()});
    for (what, message, files) in [
        ("no key", json!({"ts": "1700000100.000100"}), vec![]),
        ("null", json!({"files": null}), vec![]),
        ("empty", json!({"files": []}), vec![]),
        ("one", json!({"files": [entry("F0AAAA")]}), vec!["F0AAAA"]),
        ("two, in order", json!({"files": [entry("F0BBBB"), entry("F0AAAA")]}), vec!["F0BBBB", "F0AAAA"]),
        ("a duplicate once", json!({"files": [entry("F0AAAA"), entry("F0BBBB"), entry("F0AAAA")]}), vec!["F0AAAA", "F0BBBB"]),
        ("a tombstone entry like any", json!({"files": [{"id": "F0AAAA", "mode": "tombstone"}]}), vec!["F0AAAA"]),
        ("an entry with only an id", json!({"files": [{"id": "F0AAAA"}]}), vec!["F0AAAA"]),
    ] {
        assert_eq!(ids(message), Ok(files.into_iter().map(String::from).collect()), "{what}");
    }
    let eight: Vec<String> = (0..8).map(|i| format!("F0N{i:04}")).collect();
    assert_eq!(ids(many(8)), Ok(eight), "eight");
    for (what, message) in [
        ("nine", many(9)),
        ("a string", json!({"files": "F0AAAA"})),
        ("an object", json!({"files": {"id": "F0AAAA"}})),
        ("a number", json!({"files": 1})),
        ("an entry that is no object", json!({"files": ["F0AAAA"]})),
        ("an entry with no id", json!({"files": [entry("F0AAAA"), {"name": "n"}]})),
        ("a null id", json!({"files": [{"id": null}]})),
        ("a number id", json!({"files": [{"id": 7}]})),
        ("an id that is no id", json!({"files": [{"id": "F0 AAAA"}]})),
        ("an empty id", json!({"files": [{"id": ""}]})),
        ("nine of one id", json!({"files": vec![entry("F0AAAA"); 9]})),
    ] {
        assert_eq!(ids(message), Err(Refusal::UnclearFile), "{what}");
    }
}

const THIS: &str = "1700000100.000100";

fn verdict(answer: &Value) -> Verdict {
    file_verdict(OWN_USER, &Ok(answer.clone()), "F0AAAA", "C0CHAN", THIS)
}

fn shared(entries: Value) -> Value {
    json!({"ok": true, "file": {"id": "F0AAAA", "user": OWN_USER, "channels": ["C0CHAN"], "groups": [], "ims": [], "shares": entries}})
}

/// A file is deleted only when it is the app's own and its one listed share is this message: any other listing keeps it,
/// and anything the rule cannot match to this post is unclear. The shapes are `files.info` as the fake plays it from
/// the measurements, and the variants of each part.
#[tokio::test]
async fn a_files_verdict_is_delete_only_for_the_apps_file_shared_in_this_one_message() {
    bounded(async {
        let posts = world()
            .share("C0CHAN", THIS, None, "F0AAAA")
            .share("C0CHAN", "1700000100.000200", None, "F0ALSO")
            .also_shared("F0ALSO", "C0OTHER", "1700000100.000900")
            .share("C0CHAN", "1700000100.000300", None, "F0IM")
            .in_im("F0IM", "D0DIRECT")
            .share("C0CHAN", "1700000100.000400", None, "F0TWICE")
            .also_shared("F0TWICE", "C0CHAN", "1700000100.000800")
            .share("G0PRIV", "1700000100.000500", None, "F0PRIV")
            .share("C0CHAN", "1700000100.000600", Some("1700000100.000050"), "F0REPLY")
            .share("C0CHAN", "1700000100.000850", None, "F0DUP")
            .also_shared("F0DUP", "C0CHAN", "1700000100.000850")
            .share("C0CHAN", "1700000100.000650", None, "F0NONE")
            .unshared("F0NONE")
            .member_post("C0CHAN", "1700000100.000700", None, Some("F0PERS"));
        let fake = posts_fake(&Arc::new(posts)).await;
        let judged = |file: &'static str, channel: &'static str, ts: &'static str| {
            let fake = &fake;
            async move { file_verdict(OWN_USER, &Ok(info(fake, file).await), file, channel, ts) }
        };
        assert_eq!(judged("F0AAAA", "C0CHAN", THIS).await, Verdict::Delete);
        assert_eq!(judged("F0ALSO", "C0CHAN", "1700000100.000200").await, Verdict::Kept(Kept::SharedElsewhere));
        assert_eq!(judged("F0IM", "C0CHAN", "1700000100.000300").await, Verdict::Kept(Kept::SharedElsewhere));
        assert_eq!(judged("F0TWICE", "C0CHAN", "1700000100.000400").await, Verdict::Kept(Kept::SharedElsewhere));
        assert_eq!(judged("F0PRIV", "G0PRIV", "1700000100.000500").await, Verdict::Delete);
        assert_eq!(judged("F0PERS", "C0CHAN", "1700000100.000700").await, Verdict::Kept(Kept::NotTheApps));
        assert_eq!(judged("F0NONE", "C0CHAN", "1700000100.000650").await, Verdict::Unclear);
        // A reply's share is listed at the reply's own time with the thread's (measured); asked at the parent's time it is another time.
        assert_eq!(judged("F0REPLY", "C0CHAN", "1700000100.000600").await, Verdict::Delete);
        assert_eq!(judged("F0REPLY", "C0CHAN", "1700000100.000050").await, Verdict::Unclear);
        // The same file asked for as another channel's post is not this post's: this message is not among its shares.
        assert_eq!(judged("F0PRIV", "C0CHAN", "1700000100.000500").await, Verdict::Unclear);
        // Slack listing one file twice for this very message is still this message alone, never "shared elsewhere".
        assert_eq!(judged("F0DUP", "C0CHAN", "1700000100.000850").await, Verdict::Delete);
        // After the message alone is deleted the file is shared nowhere, and is unclear.
        assert_eq!(code(&crate::api_tests::slack(&fake, "chat.delete", "", Some(json!({"channel": "C0CHAN", "ts": THIS}))).await.unwrap().1), "ok");
        assert_eq!(judged("F0AAAA", "C0CHAN", THIS).await, Verdict::Unclear);
        fake.shutdown().await;
    })
    .await;

    // Answers and shapes the fake does not play.
    let share = |channel: &str, ts: &str| json!({channel: [{"channel_name": "chan", "reply_count": 0, "ts": ts}]});
    let deleted: Result<Value, ApiError> = Err(ApiError::Slack("file_deleted".into()));
    let unknown: Result<Value, ApiError> = Err(ApiError::Slack("file_not_found".into()));
    assert_eq!(file_verdict(OWN_USER, &deleted, "F0AAAA", "C0CHAN", THIS), Verdict::AlreadyDeleted);
    assert_eq!(file_verdict(OWN_USER, &unknown, "F0AAAA", "C0CHAN", THIS), Verdict::Unclear, "not found is not deleted");
    let limited = ApiError::Limited { retry_after: std::time::Duration::from_secs(3), assumed: false };
    for failed in [limited, ApiError::Slack("not_in_channel".into()), ApiError::Malformed, ApiError::Closed] {
        assert_eq!(file_verdict(OWN_USER, &Err(failed.clone()), "F0AAAA", "C0CHAN", THIS), Verdict::Failed(failed));
    }

    let one = || shared(json!({"public": share("C0CHAN", THIS)}));
    assert_eq!(verdict(&one()), Verdict::Delete);
    // The private kind is read like the public one.
    let private = json!({"ok": true, "file": {"id": "F0AAAA", "user": OWN_USER, "channels": [], "groups": ["C0CHAN"], "ims": [], "shares": {"private": share("C0CHAN", THIS)}}});
    assert_eq!(verdict(&private), Verdict::Delete);
    // The older keys alone, or the share's alone, are enough when nothing else is listed.
    let mut bare = one();
    bare["file"].as_object_mut().unwrap().retain(|k, _| !["channels", "groups", "ims"].contains(&k.as_str()));
    assert_eq!(verdict(&bare), Verdict::Delete);

    let mut other_channel = one();
    other_channel["file"]["shares"]["public"]["C0OTHER"] = json!([{"ts": THIS}]);
    let mut other_private = one();
    other_private["file"]["shares"]["private"] = share("G0PRIV", THIS);
    let mut other_time = one();
    other_time["file"]["shares"]["public"]["C0CHAN"] = json!([{"ts": THIS}, {"ts": "1700000100.000900"}]);
    let mut in_other = one();
    in_other["file"]["groups"] = json!(["G0PRIV"]);
    let mut in_im = one();
    in_im["file"]["ims"] = json!(["D0DIRECT"]);
    for (what, answer) in [("another channel", other_channel), ("another private channel", other_private), ("this channel twice", other_time), ("a group listed", in_other), ("a direct message", in_im)] {
        assert_eq!(verdict(&answer), Verdict::Kept(Kept::SharedElsewhere), "{what}");
    }

    // The same entry listed twice, in one kind or in both, is this message alone.
    let mut twice = one();
    twice["file"]["shares"]["public"]["C0CHAN"] = json!([{"ts": THIS}, {"ts": THIS}]);
    assert_eq!(verdict(&twice), Verdict::Delete);
    let mut both_kinds = one();
    both_kinds["file"]["shares"]["private"] = share("C0CHAN", THIS);
    assert_eq!(verdict(&both_kinds), Verdict::Delete);

    let mut person = one();
    person["file"]["user"] = json!("U0MEM");
    assert_eq!(verdict(&person), Verdict::Kept(Kept::NotTheApps));
    let mut person_odd = shared(json!(7));
    person_odd["file"]["user"] = json!("U0MEM");
    assert_eq!(verdict(&person_odd), Verdict::Kept(Kept::NotTheApps), "a person's file is kept whatever else it says");

    let mut no_user = one();
    no_user["file"].as_object_mut().unwrap().remove("user");
    let mut number_user = one();
    number_user["file"]["user"] = json!(7);
    let mut wrong_file = one();
    wrong_file["file"]["id"] = json!("F0OTHER");
    let mut entry_no_ts = one();
    entry_no_ts["file"]["shares"]["public"]["C0CHAN"] = json!([{"channel_name": "chan"}]);
    let mut entry_bad_ts = one();
    entry_bad_ts["file"]["shares"]["public"]["C0CHAN"] = json!([{"ts": "soon"}]);
    let mut entry_list_not = one();
    entry_list_not["file"]["shares"]["public"]["C0CHAN"] = json!({"ts": THIS});
    let mut entry_not_object = one();
    entry_not_object["file"]["shares"]["public"]["C0CHAN"] = json!(["x"]);
    let mut kind_not_object = one();
    kind_not_object["file"]["shares"]["public"] = json!([THIS]);
    let mut channels_not_array = one();
    channels_not_array["file"]["channels"] = json!("C0CHAN");
    let mut channel_not_string = one();
    channel_not_string["file"]["groups"] = json!([7]);
    let mut ims_not_array = one();
    ims_not_array["file"]["ims"] = json!({});
    for (what, answer) in [
        ("no file", json!({"ok": true})),
        ("a file that is not an object", json!({"ok": true, "file": "F0AAAA"})),
        ("no user", no_user),
        ("a user that is no string", number_user),
        ("another file", wrong_file),
        ("a share kind that is neither public nor private", {
            let mut v = one();
            v["file"]["shares"]["org"] = share("C0ELSE", THIS);
            v
        }),
        ("a share kind that is no object, beside the right one", {
            let mut v = one();
            v["file"]["shares"]["org"] = json!(7);
            v
        }),
        ("shares empty", shared(json!({}))),
        ("shares missing", json!({"ok": true, "file": {"id": "F0AAAA", "user": OWN_USER, "channels": [], "groups": [], "ims": []}})),
        ("shares null", shared(Value::Null)),
        ("shares not an object", shared(json!([]))),
        ("this channel at another time only", shared(json!({"public": share("C0CHAN", "1700000100.000900")}))),
        ("an entry with no time", entry_no_ts),
        ("an entry with a bad time", entry_bad_ts),
        ("an entry list that is no list", entry_list_not),
        ("an entry that is no object", entry_not_object),
        ("a share kind that is no object", kind_not_object),
        ("channels that is no array", channels_not_array),
        ("a channel that is no string", channel_not_string),
        ("ims that is no array", ims_not_array),
    ] {
        assert_eq!(verdict(&answer), Verdict::Unclear, "{what}");
    }
    // A reply's file as Slack was measured to list it (2026-11-26): one entry at the reply's own time with the thread's, and keys
    // beside the ones the rule reads. The rule reads `ts` and the channel; nothing else of the entry decides.
    let reply = json!({"ok": true, "file": {
        "id": "F0AAAA", "user": OWN_USER, "mode": "hosted", "filetype": "png", "is_public": true, "is_external": false,
        "channels": ["C0CHAN"], "groups": [], "ims": [],
        "shares": {"public": {"C0CHAN": [{
            "channel_name": "chan", "is_silent_share": false, "reply_count": 0, "reply_users": [], "reply_users_count": 0,
            "share_user_id": OWN_USER, "source": "UPLOAD", "team_id": "T0AAAA", "ts": "1700000100.000600",
            "latest_reply": "1700000100.000600", "thread_ts": "1700000100.000300",
        }]}},
    }});
    assert_eq!(file_verdict(OWN_USER, &Ok(reply.clone()), "F0AAAA", "C0CHAN", "1700000100.000600"), Verdict::Delete, "the reply's own time");
    assert_eq!(file_verdict(OWN_USER, &Ok(reply), "F0AAAA", "C0CHAN", "1700000100.000300"), Verdict::Unclear, "the parent's time is not the share's");
    // A time that is not a time cannot be matched to anything.
    assert_eq!(file_verdict(OWN_USER, &Ok(one()), "F0AAAA", "C0CHAN", "soon"), Verdict::Unclear);
}
