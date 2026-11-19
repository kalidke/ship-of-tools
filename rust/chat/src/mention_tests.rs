//! Guarantee 35: a post's `@name`s linked by the rule (pure, against invented members) and through `Api` against the
//! fake Slack's member list. Every id, name and token is synthetic.

use std::time::Duration;

use serde_json::{json, Value};
use sot_protocol::chat::{escape_text, MAX_TEXT};
use sot_protocol::ops::ChatUnlinkedWhy::{self, *};

use super::{link, Linked, Member};
use crate::api::{Api, FileOutcome, PostKind, PostOutcome, MAX_MEMBERS};
use crate::fake_slack::{FakeSlack, Identity, Members, Reply, Seen, Teams, Upload};
use crate::http::Secret;

const PERSON: PostKind = PostKind::Person;
const ROW: PostKind = PostKind::Row;

fn people() -> Vec<Value> {
    vec![
        Members::person("U0ADA", "Ada", "Ada Example"),
        Members::person("U0BEN", "", "Ben Sample"),
        Members::person("U0CY1", "cy", "Cy One"),
        Members::person("U0CY2", "", "Cy Two"),
        Members::person("U0DEE", "dee<b>&|x", "Dee Odd"),
        Members::person("U0CHAN", "channel", "Chan Nel"),
        Members::person("U0HERE", "here", "Here Now"),
        // Not people a post may name: a bot, a deactivated account, Slackbot.
        json!({"id": "U0BOT", "is_bot": true, "profile": {"display_name": "Eve", "real_name": "Eve Bot"}}),
        json!({"id": "U0GONE", "deleted": true, "profile": {"display_name": "Eve", "real_name": "Eve Gone"}}),
        json!({"id": "USLACKBOT", "profile": {"display_name": "Slackbot", "real_name": "Slackbot"}}),
    ]
}

fn members() -> Vec<Member> {
    people().iter().filter_map(Member::of).collect()
}

fn linked(text: &str, kind: PostKind) -> Linked {
    link(text, kind, Ok(&members()), MAX_TEXT)
}

fn names(l: &Linked) -> Vec<(&str, ChatUnlinkedWhy)> {
    l.unlinked.iter().map(|u| (u.name.as_str(), u.why)).collect()
}

#[test]
fn a_unique_name_becomes_a_mention_by_any_of_its_names() {
    for (typed, sent) in [
        ("@Ada Example hi", "<@U0ADA> hi"),
        ("@Ada hi", "<@U0ADA> hi"),
        ("@ben sample", "<@U0BEN>"),
        ("@Ben", "<@U0BEN>"),
        ("@Cy Two", "<@U0CY2>"),
        ("(@ada)", "(<@U0ADA>)"),
    ] {
        for kind in [PERSON, ROW] {
            let l = linked(typed, kind);
            assert_eq!(l.text, sent, "{typed}");
            assert!(l.unlinked.is_empty(), "{typed}: {:?}", l.unlinked);
        }
    }
    assert_eq!(linked("@Ada", PERSON).linked, [("U0ADA".to_string(), "Ada".to_string())], "the member linked, for the name cache");
}

#[test]
fn case_punctuation_a_possessive_two_mentions_and_both_ends() {
    let l = linked("@ADA, @Ben's turn. thanks @ben sample.", ROW);
    assert_eq!(l.text, "<@U0ADA>, <@U0BEN>'s turn. thanks <@U0BEN>.");
    assert!(l.unlinked.is_empty());
    assert_eq!(linked("@ada", PERSON).text, "<@U0ADA>");
    assert_eq!(linked("hi @Ada!", PERSON).text, "hi <@U0ADA>!");
}

#[test]
fn no_member_and_several_members_stay_text_and_are_named() {
    let typed = "@Zed and @cy and @Eve, @Slackbot";
    let l = linked(typed, PERSON);
    assert_eq!(l.text, escape_text(typed));
    assert_eq!(names(&l), [("Zed", NoMember), ("cy", SeveralMembers), ("Eve", NoMember), ("Slackbot", NoMember)]);
    // Ada's first word with no boundary after it is no one's.
    let l = linked("@Adam @ada_x @ada.example", ROW);
    assert_eq!(l.text, "@Adam @ada_x @ada.example");
    assert_eq!(names(&l), [("Adam", NoMember), ("ada_x", NoMember), ("ada.example", NoMember)]);
}

#[test]
fn nothing_typed_reaches_slack_unescaped() {
    let typed = "<@U0ADA> <!channel> <!here> <https://example.com|x> a&b <#C0CHAN>";
    for kind in [PERSON, ROW] {
        let l = linked(typed, kind);
        assert_eq!(l.text, escape_text(typed));
        assert!(l.unlinked.is_empty() && l.linked.is_empty());
    }
    // A member whose name holds `<`, `>`, `&` or `|` is linked by id alone: nothing of the name is sent.
    assert_eq!(linked("@dee<b>&|x ok", ROW).text, "<@U0DEE> ok");
    // Members named `channel` and `here` are never what those words mean.
    let row = linked("@channel and @here", ROW);
    assert_eq!(row.text, "@channel and @here");
    assert_eq!(names(&row), [("channel", ChannelWide), ("here", ChannelWide)]);
    assert_eq!(linked("@here", PERSON).text, "<!here>");
    // Whatever was linked, every `<` sent opens a token built here.
    for typed in ["@Ada <b> & @Ben>", "a <@Ada> @Ada|x", "@Ada&lt;"] {
        let sent = linked(typed, PERSON).text;
        let rest = ["<@U0ADA>", "<@U0BEN>"].iter().fold(sent.clone(), |s, t| s.replace(t, ""));
        assert!(!rest.contains(['<', '>']), "{typed} sent as {sent}");
    }
}

#[test]
fn a_persons_channel_wide_words_notify_and_a_rows_never_do() {
    let typed = "@channel @Here @everyone";
    assert_eq!(linked(typed, PERSON).text, "<!channel> <!here> <!everyone>");
    let row = linked(typed, ROW);
    assert_eq!(row.text, typed);
    assert_eq!(names(&row), [("channel", ChannelWide), ("Here", ChannelWide), ("everyone", ChannelWide)]);
    assert!(!row.wanted, "the member list is not read for channel-wide words");
    // A longer word is no channel-wide word.
    assert_eq!(names(&linked("@channels", PERSON)), [("channels", NoMember)]);
    // They need no member list, so a person's still notify when it could not be read.
    let unread = link("@here @Ada", PERSON, Err(MembersUnread), MAX_TEXT);
    assert_eq!(unread.text, "<!here> @Ada");
    assert_eq!(names(&unread), [("Ada", MembersUnread)]);
}

#[test]
fn inside_code_a_quote_an_address_or_a_word_nothing_is_linked() {
    for typed in [
        "`@Ada` x",
        "```\n@Ada\n``` x",
        "```@Ada and @Ben```",
        "> @Ada said",
        "line\n>>> quoted\n@Ada",
        "ada@example.com x@Ada",
    ] {
        let l = linked(typed, PERSON);
        assert_eq!(l.text, escape_text(typed), "{typed}");
        assert!(l.unlinked.is_empty() && !l.wanted, "{typed}");
    }
    // After the code closes, or on the line after a quote, a name links.
    assert_eq!(linked("`x` @Ada", PERSON).text, "`x` <@U0ADA>");
    assert_eq!(linked("> quoted\n@Ada", PERSON).text, "&gt; quoted\n<@U0ADA>");
}

#[test]
fn a_list_not_read_or_too_large_links_no_name_and_says_why() {
    for why in [MembersUnread, TooManyMembers] {
        let l = link("@Ada and @Ben", ROW, Err(why), MAX_TEXT);
        assert_eq!(l.text, "@Ada and @Ben");
        assert_eq!(names(&l), [("Ada", why), ("Ben", why)]);
        assert!(l.wanted);
    }
}

#[test]
fn a_text_that_linking_would_push_past_its_room_goes_as_typed() {
    let l = link("@Ada & @Ben", PERSON, Ok(&members()), 18);
    assert_eq!(l.text, "@Ada &amp; @Ben");
    assert_eq!(names(&l), [("Ada", TooLong), ("Ben", TooLong)]);
    assert!(l.linked.is_empty());
    assert_eq!(link("@Ada & @Ben", PERSON, Ok(&members()), 23).text, "<@U0ADA> &amp; <@U0BEN>", "exactly the room fits");
}

#[test]
fn the_names_left_as_text_are_at_most_max_mentions_and_each_once() {
    let typed: String = (0..40).map(|n| format!("@z{n} @z0 ")).collect();
    let l = linked(&typed, ROW);
    assert_eq!(l.unlinked.len(), sot_protocol::chat::MAX_MENTIONS);
    assert_eq!(l.unlinked.iter().filter(|u| u.name == "z0").count(), 1);
}

// Through `Api`, against the fake Slack.

const BOT: &str = "sentinel-bot-token-not-a-credential";
const USER: &str = "sentinel-person-token-not-a-credential";
const WS: &str = "T0TEAM";

async fn bounded<T>(body: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(90), body).await.expect("the test exceeded its bound")
}

/// A fake that lists `people` (or answers the member list with `list`'s reply), shares files, and acknowledges posts.
async fn fake_with(people: Vec<Value>, list: Option<fn() -> Reply>) -> FakeSlack {
    let (members, upload) = (Members(people), Upload::new());
    FakeSlack::start(Identity::Trusted, move |seen: &Seen, port| {
        if seen.path().ends_with("/users.list") {
            if let Some(list) = list {
                return list();
            }
        }
        if seen.path() == "/api/chat.postMessage" {
            return Reply::json(&json!({"ok": true, "ts": "1700000900.000100"}));
        }
        members.reply(seen).or_else(|| upload.reply(seen, port)).unwrap_or(Reply::Drop)
    })
    .await
}

fn api(fake: &FakeSlack) -> Api {
    Api::new(fake.http(), Secret::new(BOT), WS).unwrap().with_person(Some(Secret::new(USER))).with_base(fake.url("slack.com", "/api/"))
}

fn sent(fake: &FakeSlack, method: &str) -> Vec<Seen> {
    fake.seen().into_iter().filter(|s| s.path().ends_with(method)).collect()
}

fn posted(outcome: &PostOutcome) -> bool {
    matches!(outcome, PostOutcome::Posted(_))
}

#[tokio::test]
async fn a_person_and_a_row_mention_in_a_channel_and_a_thread() {
    bounded(async {
        let fake = fake_with(people(), None).await;
        let api = api(&fake);
        let (outcome, unlinked) = api.post_linked("C0CHAN", None, PERSON, "", "hi @Ada & @Zed").await.unwrap();
        assert!(posted(&outcome), "{outcome:?}");
        assert_eq!(unlinked.iter().map(|u| (u.name.as_str(), u.why)).collect::<Vec<_>>(), [("Zed", NoMember)]);
        let (outcome, unlinked) = api.post_linked("C0CHAN", Some("1700000100.000000"), ROW, "alpha-h", "@Ben Sample see").await.unwrap();
        assert!(posted(&outcome) && unlinked.is_empty(), "{outcome:?} {unlinked:?}");
        let posts = sent(&fake, "/chat.postMessage");
        assert_eq!(posts.len(), 2, "each post sent once");
        assert_eq!(posts[0].json().unwrap()["text"], "hi <@U0ADA> &amp; @Zed");
        assert_eq!(posts[0].authorization.as_deref(), Some(&*format!("Bearer {USER}")), "a person's post under their token");
        let reply = posts[1].json().unwrap();
        assert_eq!((reply["text"].as_str(), reply["thread_ts"].as_str()), (Some("<@U0BEN> see"), Some("1700000100.000000")));
        // The member list is read under the bot's token, once for both posts.
        let lists = sent(&fake, "/users.list");
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0].authorization.as_deref(), Some(&*format!("Bearer {BOT}")));
        // A member linked is named from the cache at once, as a post read back shows it.
        assert_eq!(api.cached_name("U0ADA").as_deref(), Some("Ada"));
    })
    .await
}

#[tokio::test]
async fn a_file_comment_mentions_and_a_rows_label_never_does() {
    bounded(async {
        let fake = fake_with(people(), None).await;
        let api = api(&fake);
        let png = b"\x89PNG bytes".to_vec();
        let (outcome, unlinked) =
            api.post_file_linked(api.transfer().unwrap(), "C0CHAN", None, ROW, "ada", "a.png", png.clone(), Some("for @Ada & @Zed")).await.unwrap();
        assert!(matches!(outcome, FileOutcome::Posted { .. }), "{outcome:?}");
        assert_eq!(unlinked.iter().map(|u| (u.name.as_str(), u.why)).collect::<Vec<_>>(), [("Zed", NoMember)]);
        let (outcome, _) =
            api.post_file_linked(api.transfer().unwrap(), "C0CHAN", Some("1700000100.000000"), PERSON, "", "a.png", png.clone(), Some("@ben")).await.unwrap();
        assert!(matches!(outcome, FileOutcome::Posted { .. }), "{outcome:?}");
        let (outcome, unlinked) = api.post_file_linked(api.transfer().unwrap(), "C0CHAN", None, ROW, "ada", "a.png", png, None).await.unwrap();
        assert!(matches!(outcome, FileOutcome::Posted { .. }) && unlinked.is_empty(), "{outcome:?}");
        let comments: Vec<Value> = sent(&fake, "/files.completeUploadExternal").iter().map(|s| s.json().unwrap()["initial_comment"].clone()).collect();
        assert_eq!(comments, [json!("ada (row via Ship of Tools): for <@U0ADA> &amp; @Zed"), json!("<@U0BEN>"), json!("ada (row via Ship of Tools)")]);
    })
    .await
}

#[tokio::test]
async fn the_member_list_is_read_only_for_a_name_paged_and_kept() {
    bounded(async {
        // Ada is the last of 450, on the third page.
        let mut many: Vec<Value> = (0..449).map(|n| Members::person(&format!("U0P{n}"), &format!("p{n}"), &format!("Person {n}"))).collect();
        many.push(Members::person("U0ADA", "Ada", "Ada Example"));
        let fake = fake_with(many, None).await;
        let api = api(&fake);
        api.post("C0CHAN", None, ROW, "alpha-h", "no names here, a@b.com and @channel").await.unwrap();
        assert_eq!(sent(&fake, "/users.list").len(), 0, "a cold list is read only when a name needs it");
        let (_, unlinked) = api.post_linked("C0CHAN", None, ROW, "alpha-h", "@Ada").await.unwrap();
        assert!(unlinked.is_empty(), "{unlinked:?}");
        let lists = sent(&fake, "/users.list");
        let cursors: Vec<Option<String>> = lists.iter().map(|s| s.param("cursor")).collect();
        assert_eq!(cursors, [None, Some("200".into()), Some("400".into())], "three pages, each from the last one's cursor");
        assert!(lists.iter().all(|s| s.param("limit").as_deref() == Some("200")));
        api.post("C0CHAN", None, ROW, "alpha-h", "@p7 again").await.unwrap();
        assert_eq!(sent(&fake, "/users.list").len(), 3, "a fresh list is kept");
        let texts: Vec<Value> = sent(&fake, "/chat.postMessage").iter().map(|s| s.json().unwrap()["text"].clone()).collect();
        assert_eq!(texts[1..], [json!("<@U0ADA>"), json!("<@U0P7> again")]);
    })
    .await
}

#[tokio::test]
async fn a_workspace_over_the_bound_links_no_name_and_says_so() {
    bounded(async {
        let all: Vec<Value> = (0..=MAX_MEMBERS).map(|n| Members::person(&format!("U0P{n}"), &format!("p{n}"), "")).collect();
        let fake = fake_with(all, None).await;
        let api = api(&fake);
        let (outcome, unlinked) = api.post_linked("C0CHAN", None, ROW, "alpha-h", "@p1 hi").await.unwrap();
        assert!(posted(&outcome), "{outcome:?}");
        assert_eq!(unlinked.iter().map(|u| (u.name.as_str(), u.why)).collect::<Vec<_>>(), [("p1", TooManyMembers)]);
        assert_eq!(sent(&fake, "/users.list").len(), MAX_MEMBERS / 200, "the walk stops at its bound");
        assert_eq!(sent(&fake, "/chat.postMessage")[0].json().unwrap()["text"], "@p1 hi");
    })
    .await
}

#[tokio::test]
async fn a_failed_or_slow_member_list_posts_the_text_once_and_says_so() {
    bounded(async {
        let refused: fn() -> Reply = || Reply::json(&json!({"ok": false, "error": "internal_error"}));
        let hung: fn() -> Reply = || Reply::Hang;
        for list in [refused, hung] {
            let fake = fake_with(people(), Some(list)).await;
            let api = api(&fake).with_deadlines(Duration::from_secs(15), Duration::from_secs(30), Duration::from_millis(500));
            for n in 1..=2 {
                let started = tokio::time::Instant::now();
                let (outcome, unlinked) = api.post_linked("C0CHAN", None, PERSON, "", "@Ada hi").await.unwrap();
                assert!(started.elapsed() < Duration::from_secs(5), "the post waited past the names deadline");
                assert!(posted(&outcome), "{outcome:?}");
                assert_eq!(unlinked.iter().map(|u| (u.name.as_str(), u.why)).collect::<Vec<_>>(), [("Ada", MembersUnread)]);
                let posts = sent(&fake, "/chat.postMessage");
                assert_eq!(posts.len(), n, "one post a call, never resent");
                assert_eq!(posts[n - 1].json().unwrap()["text"], "@Ada hi");
                assert_eq!(sent(&fake, "/users.list").len(), n, "a failure is not remembered");
            }
        }
    })
    .await
}

#[tokio::test]
async fn a_name_resolves_only_among_its_own_workspaces_members() {
    bounded(async {
        const A: &str = "T0ALPHA";
        const B: &str = "T0BETA";
        let teams = Teams::start(&[(A, "Alpha Lab"), (B, "Beta Lab")]).await;
        teams.channels(A, &[("C0AAAA01", "alpha", false, true)]);
        teams.channels(B, &[("C0BBBB01", "beta", false, true)]);
        teams.members(A, &[Members::person("U0ADA", "Ada", "Ada Example")]);
        teams.members(B, &[Members::person("U0BEN", "Ben", "Ben Sample")]);
        let api = |w: &str| {
            Api::new(teams.fake().http(), Secret::new(&teams.bot(w)), w)
                .unwrap()
                .with_person(Some(Secret::new(&teams.user(w))))
                .with_base(teams.fake().url("slack.com", "/api/"))
        };
        let (a, b) = (api(A), api(B));
        let (_, in_a) = a.post_linked("C0AAAA01", None, PERSON, "", "@Ada and @Ben").await.unwrap();
        let (_, in_b) = b.post_linked("C0BBBB01", None, PERSON, "", "@Ada and @Ben").await.unwrap();
        assert_eq!(in_a.iter().map(|u| (u.name.as_str(), u.why)).collect::<Vec<_>>(), [("Ben", NoMember)]);
        assert_eq!(in_b.iter().map(|u| (u.name.as_str(), u.why)).collect::<Vec<_>>(), [("Ada", NoMember)]);
        let text = |w: &str| teams.calls(w, "xoxp", "/chat.postMessage")[0].json().unwrap()["text"].clone();
        assert_eq!((text(A), text(B)), (json!("<@U0ADA> and @Ben"), json!("@Ada and <@U0BEN>")));
        assert_eq!((teams.calls(A, "xoxb", "/users.list").len(), teams.calls(B, "xoxb", "/users.list").len()), (1, 1), "each list under its own bot");
    })
    .await
}
