//! The read and the verdicts of a delete of a post (chat guarantee 36), against the fake Slack's `Posts`. Every id,
//! name and time is synthetic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sot_protocol::ops::ChatCode;

use crate::api::delete::{file_verdict, own_post_files, Kept, Listed, PostDelete, PostEnd, PostFiles, Refusal, Verdict, POST_READ_LIMIT};
use crate::api::{Api, ApiError, ASSUMED_RETRY, NAMES_DEADLINE};
use crate::api_tests::{bounded, code, info, posts_fake, ready, OWN_BOT, OWN_USER};
use crate::fake_slack::{Fault, FakeSlack, Posts};

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

/// The delete once, with an admission that always passes.
async fn delete(api: &Api, thread: Option<&str>, ts: &str) -> PostDelete {
    api.delete_post("C0CHAN", thread, ts, || async { Ok(()) }).await
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn files(deleted: &[&str]) -> PostFiles {
    PostFiles { deleted: ids(deleted), kept: Vec::new() }
}

const ASK: &str = "1700000100.000100";
const READ: &str = "conversations.history";

/// Files go before the message, every file is judged before one is deleted, each mutation is sent once, and the first
/// answer that is not an acknowledgement ends the delete. Each stop leaves a state the same ask, made again, finishes: a
/// file deleted but its answer lost reads as deleted, and a message whose files are all gone is deleted.
#[tokio::test]
async fn the_files_go_before_the_message_and_each_stop_leaves_a_state_the_next_ask_finishes() {
    bounded(async {
        let limited = |secs, assumed| PostEnd::Limited { retry_after: Duration::from_secs(secs), assumed };
        let refused = |why: &str| PostEnd::Refused(Refusal::Slack(why.into()));
        const WHOLE: [&str; 4] = [READ, "files.info", "files.delete", "chat.delete"];
        // (the mutation, its fault, how the first ask ends or None for any lost answer, the files it reports deleted, the
        // calls of the first ask, whether the file and the message stand after it, the second ask's calls and end)
        type Case = (&'static str, Fault, Option<PostEnd>, &'static [&'static str], &'static [&'static str], (bool, bool), &'static [&'static str], PostEnd);
        let cases: Vec<Case> = vec![
            ("files.delete", Fault::Drop, None, &[], &WHOLE[..3], (true, true), &WHOLE, PostEnd::Deleted),
            ("files.delete", Fault::DropAfter, None, &[], &WHOLE[..3], (false, true), &[READ, "files.info", "chat.delete"], PostEnd::Deleted),
            ("files.delete", Fault::Limit(Some(7)), Some(limited(7, false)), &[], &WHOLE[..3], (true, true), &WHOLE, PostEnd::Deleted),
            ("files.delete", Fault::Limit(None), Some(limited(ASSUMED_RETRY.as_secs(), true)), &[], &WHOLE[..3], (true, true), &WHOLE, PostEnd::Deleted),
            ("files.delete", Fault::Error("cant_delete_file"), Some(refused("cant_delete_file")), &[], &WHOLE[..3], (true, true), &WHOLE, PostEnd::Deleted),
            ("chat.delete", Fault::Drop, None, &["F0AAAA"], &WHOLE, (false, true), &[READ, "files.info", "chat.delete"], PostEnd::Deleted),
            ("chat.delete", Fault::DropAfter, None, &["F0AAAA"], &WHOLE, (false, false), &[READ], PostEnd::Gone),
            ("chat.delete", Fault::Limit(Some(7)), Some(limited(7, false)), &["F0AAAA"], &WHOLE, (false, true), &[READ, "files.info", "chat.delete"], PostEnd::Deleted),
            ("chat.delete", Fault::Error("cant_delete_message"), Some(PostEnd::Refused(Refusal::NotOwnMessage)), &["F0AAAA"], &WHOLE, (false, true), &[READ, "files.info", "chat.delete"], PostEnd::Deleted),
        ];
        for (method, fault, end, deleted, calls, (file_stands, message_stands), again, second) in cases {
            let what = format!("{method} {fault:?}");
            let posts = Arc::new(world().share("C0CHAN", ASK, None, "F0AAAA"));
            let fake = posts_fake(&posts).await;
            let api = ready(&fake).await;
            posts.fault(method, fault);
            let first = delete(&api, None, ASK).await;
            match end {
                Some(end) => assert_eq!(first.end, end, "{what}"),
                None => assert!(matches!(first.end, PostEnd::Unknown(ref why) if !why.is_empty()), "{what}: {:?}", first.end),
            }
            assert_eq!(first.files, files(deleted), "{what}");
            assert_eq!(first.step, method, "{what}");
            assert_eq!(asked(&fake), calls, "{what}: each call once, nothing after the stop");
            assert_eq!((posts.holds_file("F0AAAA"), posts.holds_message("C0CHAN", ASK)), (file_stands, message_stands), "{what}");

            // The same ask again, with nothing wrong, finishes it, and sends nothing but what is left.
            let before = asked(&fake).len();
            let next = delete(&api, None, ASK).await;
            assert_eq!(next.end, second, "{what}: the second ask");
            assert_eq!(asked(&fake)[before..], *again, "{what}: the second ask's calls");
            if second == PostEnd::Deleted {
                assert_eq!(next.files, files(&["F0AAAA"]), "{what}: the same files");
            }
            assert_eq!((posts.holds_file("F0AAAA"), posts.holds_message("C0CHAN", ASK)), (false, false), "{what}");
            fake.shutdown().await;
        }
    })
    .await;
}

/// Every file is judged, in the message's order, before any is deleted: the app's are deleted, in order, then the message;
/// a file also shared elsewhere is kept and said so, and one already deleted counts as deleted.
#[tokio::test]
async fn a_post_with_several_files_judges_them_all_first_and_deletes_only_the_apps() {
    bounded(async {
        let posts = Arc::new(
            world()
                .share_many("C0CHAN", ASK, None, &["F0AAAA", "F0BBBB", "F0CCCC", "F0DDDD"])
                .also_shared("F0BBBB", "C0OTHER", "1700000100.000900"),
        );
        let fake = posts_fake(&posts).await;
        let api = ready(&fake).await;
        // A person deleted one of them in Slack already.
        assert_eq!(code(&crate::api_tests::slack(&fake, "files.delete", "", Some(json!({"file": "F0DDDD"}))).await.unwrap().1), "ok");
        let done = delete(&api, None, ASK).await;
        assert_eq!(done.end, PostEnd::Deleted);
        assert_eq!(done.files, PostFiles { deleted: ids(&["F0AAAA", "F0CCCC", "F0DDDD"]), kept: vec![("F0BBBB".into(), Kept::SharedElsewhere)] });
        let seen = asked(&fake);
        assert_eq!(seen[1..], [READ, "files.info", "files.info", "files.info", "files.info", "files.delete", "files.delete", "chat.delete"][..], "{seen:?}");
        let deleted: Vec<String> = fake.seen().iter().filter(|s| s.path() == "/api/files.delete").skip(1).map(|s| s.json().unwrap()["file"].as_str().unwrap().to_string()).collect();
        assert_eq!(deleted, ["F0AAAA", "F0CCCC"], "in the message's order, the kept and the deleted ones never sent");
        assert_eq!((posts.holds_file("F0BBBB"), posts.holds_message("C0CHAN", ASK)), (true, false));
        fake.shutdown().await;
    })
    .await;
}

/// A file the delete cannot judge stops it before any mutation, for any file: the one before it, which it would have
/// deleted, is deleted by nothing.
#[tokio::test]
async fn an_unclear_file_stops_the_delete_before_any_mutation() {
    bounded(async {
        let posts = Arc::new(world().share_many("C0CHAN", ASK, None, &["F0AAAA", "F0BBBB"]).unshared("F0BBBB"));
        let fake = posts_fake(&posts).await;
        let api = ready(&fake).await;
        let done = delete(&api, None, ASK).await;
        assert_eq!(done.end, PostEnd::Refused(Refusal::UnclearFile));
        assert_eq!((done.step, done.file.as_str(), done.files), ("files.info", "F0BBBB", PostFiles::default()));
        assert_eq!(asked(&fake), [READ, "files.info", "files.info"]);
        assert!(posts.holds_file("F0AAAA") && posts.holds_file("F0BBBB") && posts.holds_message("C0CHAN", ASK));

        // A file Slack does not know is unclear too, never deleted already.
        let posts = Arc::new(world().share("C0CHAN", ASK, None, "F0AAAA"));
        posts.fault("files.info", Fault::Error("file_not_found"));
        let fake = posts_fake(&posts).await;
        let api = ready(&fake).await;
        assert_eq!(delete(&api, None, ASK).await.end, PostEnd::Refused(Refusal::UnclearFile));
        assert_eq!(asked(&fake), [READ, "files.info"]);
        fake.shutdown().await;
    })
    .await;
}

/// Where the read ends it: a message that is not the app's, a reply asked for with no thread, a thread or time Slack does
/// not list, a page with more behind it, and a limit; each sends only the read and leaves everything as it was. The same
/// delete of a reply, given its thread, deletes it whole.
#[tokio::test]
async fn the_delete_ends_at_the_read_when_the_message_is_not_to_be_deleted() {
    bounded(async {
        let mut world = world()
            .member_post("C0CHAN", "1700000100.000200", None, Some("F0PERS"))
            .bot_post("C0CHAN", "1700000100.000300", None)
            .bot_post("C0CHAN", "1700000100.000400", Some("1700000100.000300"))
            .share("C0CHAN", "1700000100.000500", Some("1700000100.000300"), "F0REPLY")
            .tombstone("C0CHAN", "1700000100.000700");
        for i in 1..=101 {
            world = world.member_post("C0CHAN", &format!("1700000200.{i:06}"), None, None);
        }
        let posts = Arc::new(world);
        let fake = posts_fake(&posts).await;
        let api = ready(&fake).await;
        let mut calls = 0;
        for (what, thread, ts, end, step) in [
            ("a member's post with a file", None, "1700000100.000200", PostEnd::Refused(Refusal::NotOwnMessage), READ),
            ("a reply, asked with no thread", None, "1700000100.000400", PostEnd::Gone, READ),
            ("a reply, asked in another thread", Some("1700000100.000200"), "1700000100.000400", PostEnd::Gone, "conversations.replies"),
            ("no such time", None, "1700000100.000600", PostEnd::Gone, READ),
            ("a deleted parent's placeholder", None, "1700000100.000700", PostEnd::Gone, READ),
            ("a page of many", None, "1700000200.500000", PostEnd::Refused(Refusal::Incomplete), READ),
        ] {
            let done = api.delete_post("C0CHAN", thread, ts, || async { Ok(()) }).await;
            assert_eq!((done.end, done.step, done.files), (end, step, PostFiles::default()), "{what}");
            calls += 1;
            assert_eq!(asked(&fake).len(), calls, "{what}: the read alone");
        }
        assert!(posts.holds_file("F0PERS") && posts.holds_message("C0CHAN", "1700000100.000200") && posts.holds_message("C0CHAN", "1700000100.000400"));

        // A read Slack limits ends it, and the pause that follows keeps the next ask from sending anything.
        posts.fault(READ, Fault::Limit(Some(5)));
        let limited = PostEnd::Limited { retry_after: Duration::from_secs(5), assumed: false };
        assert_eq!(delete(&api, None, "1700000100.000300").await.end, limited);
        assert_eq!(asked(&fake).len(), calls + 1);
        assert!(matches!(delete(&api, None, "1700000100.000300").await.end, PostEnd::Limited { .. }));
        assert_eq!(asked(&fake).len(), calls + 1, "a read pause is kept");

        // A reply is found in its thread, and deleted with the file it shared.
        let api = ready(&fake).await;
        let done = delete(&api, Some("1700000100.000300"), "1700000100.000500").await;
        assert_eq!((done.end, done.files), (PostEnd::Deleted, files(&["F0REPLY"])));
        assert_eq!(asked(&fake)[calls + 1..], ["conversations.replies", "files.info", "files.delete", "chat.delete"]);
        assert!(!posts.holds_file("F0REPLY") && !posts.holds_message("C0CHAN", "1700000100.000500"));
        fake.shutdown().await;
    })
    .await;
}

/// A fresh admission precedes every call but the first, and one that refuses ends the delete before that call, whatever
/// was done until then; the same ask with a good admission then finishes it.
#[tokio::test]
async fn a_recheck_that_fails_stops_the_delete_before_its_next_call() {
    bounded(async {
        for k in 2..=4usize {
            let posts = Arc::new(world().share("C0CHAN", ASK, None, "F0AAAA"));
            let fake = posts_fake(&posts).await;
            let api = ready(&fake).await;
            let rechecks = Arc::new(AtomicUsize::new(0));
            let counted = rechecks.clone();
            let failing = move || {
                let nth = counted.fetch_add(1, Ordering::SeqCst) + 2;
                async move { if nth == k { Err(ChatCode::StaleRow) } else { Ok(()) } }
            };
            let done = api.delete_post("C0CHAN", None, ASK, failing).await;
            assert_eq!(done.end, PostEnd::Refused(Refusal::Recheck(ChatCode::StaleRow)), "k={k}");
            assert_eq!((done.step, rechecks.load(Ordering::SeqCst)), ("recheck", k - 1), "k={k}");
            let all = [READ, "files.info", "files.delete", "chat.delete"];
            assert_eq!(asked(&fake), all[..k - 1], "k={k}: no call at or past the refused one");
            assert_eq!(done.files, files(if k == 4 { &["F0AAAA"] } else { &[] }), "k={k}");
            let next = delete(&api, None, ASK).await;
            assert_eq!((next.end, next.files), (PostEnd::Deleted, files(&["F0AAAA"])), "k={k}: a second ask finishes");
            assert!(!posts.holds_file("F0AAAA") && !posts.holds_message("C0CHAN", ASK), "k={k}");
            fake.shutdown().await;
        }
    })
    .await;
}

/// A call begins only when its own deadline fits in what is left of the delete's one deadline, a mutation's longer than a
/// read's; otherwise the delete ends before it, with the post as it was.
#[tokio::test(flavor = "multi_thread")]
async fn a_call_whose_deadline_does_not_fit_is_never_sent() {
    bounded(async {
        let posts = Arc::new(world().share("C0CHAN", ASK, None, "F0AAAA"));
        let fake = posts_fake(&posts).await;
        // Under the real read and mutation deadlines (15 s and 30 s): none fits in 10 s, the reads fit in 20 s.
        for (budget, calls, step, file) in [(10, 0, "deadline", ""), (20, 2, "deadline", "F0AAAA")] {
            let api = ready(&fake).await.with_delete_deadline(Duration::from_secs(budget));
            let before = asked(&fake).len();
            let done = delete(&api, None, ASK).await;
            assert_eq!(done.end, PostEnd::Refused(Refusal::Deadline), "{budget} s");
            assert_eq!((done.step, done.file.as_str()), (step, file), "{budget} s");
            assert_eq!(asked(&fake).len() - before, calls, "{budget} s");
            assert!(posts.holds_file("F0AAAA") && posts.holds_message("C0CHAN", ASK));
        }
        // What is left shrinks as the delete goes: a slow `files.info` leaves too little for the mutation after it.
        let scaled = |api: Api| api.with_deadlines(Duration::from_secs(2), Duration::from_secs(3), NAMES_DEADLINE).with_delete_deadline(Duration::from_secs(4));
        let slow = scaled(ready(&fake).await);
        posts.before("files.info", || std::thread::sleep(Duration::from_millis(1500)));
        let before = asked(&fake).len();
        let done = delete(&slow, None, ASK).await;
        assert_eq!((done.end, done.step), (PostEnd::Refused(Refusal::Deadline), "deadline"));
        assert_eq!(asked(&fake)[before..], [READ, "files.info"]);
        assert!(posts.holds_file("F0AAAA"));
        // With nothing slow the same budget is enough.
        let posts = Arc::new(world().share("C0CHAN", ASK, None, "F0AAAA"));
        let fake = posts_fake(&posts).await;
        let done = delete(&scaled(ready(&fake).await), None, ASK).await;
        assert_eq!((done.end, done.files), (PostEnd::Deleted, files(&["F0AAAA"])));
        fake.shutdown().await;
    })
    .await;
}

/// A `files.info` that fails ends the delete before any mutation, whatever way it fails: a file whose shares were not read is
/// not judged, so a delete that went on to the message would leave the file alive and shared nowhere. Each fault is once, and
/// the same ask again, on a client with no read pause left, finishes the delete.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_files_info_ends_the_delete_before_any_mutation() {
    bounded(async {
        type Check = fn(&PostEnd) -> bool;
        let cases: Vec<(&str, Fault, Check)> = vec![
            ("a dropped connection", Fault::Drop, |e| matches!(e, PostEnd::Refused(Refusal::Call(ApiError::Http(_))))),
            ("a 429 with a wait", Fault::Limit(Some(7)), |e| matches!(e, PostEnd::Limited { assumed: false, .. })),
            ("a 429 with none", Fault::Limit(None), |e| matches!(e, PostEnd::Limited { assumed: true, .. })),
            ("a refusal", Fault::Error("internal_error"), |e| matches!(e, PostEnd::Refused(Refusal::Call(ApiError::Slack(c))) if c == "internal_error")),
            ("a refusal of the channel", Fault::Error("not_in_channel"), |e| matches!(e, PostEnd::Refused(Refusal::Call(ApiError::Slack(c))) if c == "not_in_channel")),
            ("an answer that is no JSON", Fault::Garbled, |e| matches!(e, PostEnd::Refused(Refusal::Call(ApiError::Malformed)))),
            ("an answer that never comes", Fault::Hang, |e| matches!(e, PostEnd::Refused(Refusal::Call(ApiError::Http(_))))),
        ];
        for (what, fault, ends) in cases {
            let posts = Arc::new(world().share("C0CHAN", ASK, None, "F0AAAA"));
            let fake = posts_fake(&posts).await;
            // A short read deadline, so the answer that never comes times out within the test.
            let api = ready(&fake).await.with_deadlines(Duration::from_millis(400), Duration::from_secs(3), NAMES_DEADLINE);
            posts.fault("files.info", fault);
            let first = delete(&api, None, ASK).await;
            assert!(ends(&first.end), "{what}: {:?}", first.end);
            assert_eq!(first.step, "files.info", "{what}");
            assert_eq!(asked(&fake), [READ, "files.info"], "{what}: nothing is sent after the failed read");
            assert!(posts.holds_file("F0AAAA") && posts.holds_message("C0CHAN", ASK), "{what}: nothing was deleted");
            let next = delete(&ready(&fake).await, None, ASK).await;
            assert_eq!((next.end, next.files), (PostEnd::Deleted, files(&["F0AAAA"])), "{what}: the same ask finishes it");
            fake.shutdown().await;
        }
    })
    .await;
}

/// One deadline holds the whole delete, so a slow admission before a call cannot let that call begin past it: the deadline is
/// checked again after the recheck.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_recheck_does_not_let_a_call_start_past_the_deadline() {
    bounded(async {
        let posts = Arc::new(world().share("C0CHAN", ASK, None, "F0AAAA"));
        let fake = posts_fake(&posts).await;
        let api = ready(&fake).await.with_deadlines(Duration::from_millis(300), Duration::from_millis(300), NAMES_DEADLINE).with_delete_deadline(Duration::from_secs(1));
        let rechecks = Arc::new(AtomicUsize::new(0));
        let counted = rechecks.clone();
        // The second admission (before `files.delete`) takes longer than the whole delete's budget.
        let slow = move || {
            let nth = counted.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if nth == 2 {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                }
                Ok(())
            }
        };
        let done = api.delete_post("C0CHAN", None, ASK, slow).await;
        assert_eq!(done.end, PostEnd::Refused(Refusal::Deadline));
        assert_eq!(done.step, "deadline");
        assert_eq!(asked(&fake), [READ, "files.info"], "no mutation began past the deadline");
        assert!(posts.holds_file("F0AAAA") && posts.holds_message("C0CHAN", ASK));
        fake.shutdown().await;
    })
    .await;
}
