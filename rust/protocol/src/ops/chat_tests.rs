use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::*;
use crate::chat::Origin;

fn ts(s: &str) -> Ts {
    Ts::parse(s).unwrap()
}

fn target() -> ChatTarget {
    ChatTarget { row: "ws-a-1".into(), workspace: "T1".into(), channel: ChatChannel::try_from("C1".to_string()).unwrap() }
}

fn target_json() -> Value {
    json!({"row": "ws-a-1", "workspace": "T1", "channel": "C1"})
}

/// A token built at run time, so no literal looks like one to a secret scan.
fn token(prefix: &str, n: usize) -> ChatToken {
    ChatToken::try_from(format!("{prefix}{}", "a1".repeat(n))).unwrap()
}

fn channel(id: &str) -> ChatChannel {
    ChatChannel::try_from(id.to_string()).unwrap()
}

fn record(text: &str) -> ChatRecord {
    ChatRecord { ts: ts("1.000001"), thread_ts: ts("1.000001"), text: text.into(), ..ChatRecord::fixture() }
}

/// The value is exactly `shape`, the shape decodes back to the value, and a field added later (in the payload and in
/// its target) is ignored: every chat payload grows by defaulted fields, requests included.
fn pinned<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: T, shape: Value) {
    assert_eq!(serde_json::to_value(&value).unwrap(), shape);
    assert_eq!(serde_json::from_value::<T>(shape.clone()).unwrap(), value);
    let mut grown = shape;
    grown["zz"] = json!(1);
    if let Some(t) = grown.get_mut("target") {
        t["zz"] = json!(1);
    }
    assert_eq!(serde_json::from_value::<T>(grown).unwrap(), value, "an added field is ignored");
}

#[test]
fn the_chat_ops_are_declared_in_the_one_table() {
    for name in [
        "chat.resolve",
        "chat.watch",
        "chat.unwatch",
        "chat.history",
        "chat.post",
        "chat.post_file",
        "chat.file",
        "chat.changed",
        "chat.status",
        "chat.connect",
        "chat.channels",
        "chat.pair",
        "chat.workspaces",
    ] {
        assert!(op::ALL.contains(&name), "{name} is not in op::ALL");
    }
}

#[test]
fn every_request_reply_and_event_keeps_its_shape() {
    pinned(ChatResolveReq { row: "ws-a-1".into() }, json!({"row": "ws-a-1"}));
    pinned(
        ChatResolveRes::Mapped { workspace: "T1".into(), channel: target().channel, delivers: false, name: None, workspace_name: None },
        json!({"state": "mapped", "workspace": "T1", "channel": "C1", "delivers": false}),
    );
    pinned(
        ChatResolveRes::Mapped { workspace: "T1".into(), channel: target().channel, delivers: false, name: Some("general".into()), workspace_name: None },
        json!({"state": "mapped", "workspace": "T1", "channel": "C1", "delivers": false, "name": "general"}),
    );
    pinned(
        ChatResolveRes::Mapped { workspace: "T1".into(), channel: target().channel, delivers: false, name: None, workspace_name: Some("Lab".into()) },
        json!({"state": "mapped", "workspace": "T1", "channel": "C1", "delivers": false, "workspace_name": "Lab"}),
    );
    pinned(ChatResolveRes::Unmapped, json!({"state": "unmapped"}));
    pinned(ChatWatchReq { target: target() }, json!({"target": target_json()}));
    pinned(ChatWatchRes { state: ChatWatchState::Pending }, json!({"state": "pending"}));
    pinned(ChatUnwatchReq { target: target() }, json!({"target": target_json()}));
    pinned(ChatUnwatchRes {}, json!({}));
    pinned(
        ChatHistoryReq {
            target: target(),
            thread: Some(ts("1.000001")),
            after: None,
            upper: Some(ChatUpper::Until(ts("2.000002"))),
            limit: 20,
            cursor: Some("c".into()),
        },
        json!({"target": target_json(), "thread": "1.000001", "after": null, "upper": {"until": "2.000002"}, "limit": 20, "cursor": "c"}),
    );
    pinned(
        ChatHistoryRes { records: vec![], next: None, limited: false },
        json!({"records": []}),
    );
    pinned(
        ChatPostReq { target: target(), thread: None, kind: ChatPostKind::Row, name: "r".into(), text: "hi".into() },
        json!({"target": target_json(), "thread": null, "kind": "row", "name": "r", "text": "hi"}),
    );
    pinned(ChatPostRes::Posted { ts: ts("3.000003") }, json!({"outcome": "posted", "ts": "3.000003"}));
    pinned(ChatPostRes::Refused { code: ChatCode::StaleRow }, json!({"outcome": "refused", "code": "stale_row"}));
    pinned(
        ChatPostRes::Limited { retry_after_secs: 30, assumed: true },
        json!({"outcome": "limited", "retry_after_secs": 30, "assumed": true}),
    );
    pinned(ChatPostRes::Unknown, json!({"outcome": "unknown"}));
    pinned(
        ChatChangedEvt { workspace: "T1".into(), channel: target().channel, put: vec![record("a")], deleted: vec![ts("1.000001")] },
        json!({"workspace": "T1", "channel": "C1", "put": [serde_json::to_value(record("a")).unwrap()], "deleted": ["1.000001"]}),
    );
    pinned(
        ChatStatusEvt { service: ChatService::Running, gap: true, resync: vec![target().channel] },
        json!({"service": "running", "gap": true, "resync": ["C1"]}),
    );
    pinned(ChatRefusal { code: Some(ChatCode::TooManyWatches) }, json!({"code": "too_many_watches"}));
    pinned(
        ChatResolveRes::Unconfigured { reason: ChatUnconfigured::TokensRefused, workspace: None },
        json!({"state": "unconfigured", "reason": "tokens_refused"}),
    );
    pinned(
        ChatConnectReq { bot_token: token("xoxb-", 2), user_token: None, app_token: token("xapp-", 2) },
        json!({"bot_token": "xoxb-a1a1", "app_token": "xapp-a1a1"}),
    );
    pinned(
        ChatConnectReq { bot_token: token("xoxb-", 2), user_token: Some(token("xoxp-", 2)), app_token: token("xapp-", 2) },
        json!({"bot_token": "xoxb-a1a1", "user_token": "xoxp-a1a1", "app_token": "xapp-a1a1"}),
    );
    pinned(ChatConnectRes { workspace: "T1".into(), name: None }, json!({"workspace": "T1"}));
    pinned(ChatConnectRes { workspace: "T1".into(), name: Some("Alpha Lab".into()) }, json!({"workspace": "T1", "name": "Alpha Lab"}));
    // A reply of this branch's first form, which no release sent: its `unread` is an unknown field, ignored.
    let first: ChatConnectRes = serde_json::from_value(json!({"workspace": "T1", "unread": [{"id": "G0PRIVATE1"}]})).unwrap();
    assert_eq!(first, ChatConnectRes { workspace: "T1".into(), name: None });
    pinned(ChatChannelsReq { workspace: None }, json!({}));
    pinned(ChatChannelsReq { workspace: Some("T1".into()) }, json!({"workspace": "T1"}));
    pinned(ChatWorkspacesReq {}, json!({}));
    let unread = |id: &str, name: Option<&str>| ChatUnread { id: ChatChannel::try_from(id.to_string()).unwrap(), name: name.map(str::to_string) };
    let info = |id: &str, running| ChatWorkspaceInfo { id: id.into(), name: None, running, app_name: None, app_id: None, unread: None };
    pinned(
        ChatWorkspacesRes {
            default: "T1".into(),
            workspaces: vec![
                ChatWorkspaceInfo {
                    name: Some("Alpha".into()),
                    app_name: Some("Ship of Tools (lab-user)".into()),
                    app_id: Some("A0NEW".into()),
                    unread: Some(vec![unread("G0PRIVATE1", Some("lab-private")), unread("C0PUBLIC1", None)]),
                    ..info("T1", true)
                },
                ChatWorkspaceInfo { unread: Some(Vec::new()), ..info("T2", true) },
                info("T3", false),
            ],
        },
        json!({"default": "T1", "workspaces": [
            {"id": "T1", "name": "Alpha", "running": true, "app_name": "Ship of Tools (lab-user)", "app_id": "A0NEW",
             "unread": [{"id": "G0PRIVATE1", "name": "lab-private"}, {"id": "C0PUBLIC1"}]},
            {"id": "T2", "running": true, "unread": []},
            {"id": "T3", "running": false}]}),
    );
    // The connected app's name is read as a workspace's name is, and its id as an id: one that is not usable is none, and
    // the entry stays.
    let read: ChatWorkspacesRes = serde_json::from_value(json!({"default": "T1", "workspaces": [
        {"id": "T1", "running": true, "app_name": 7, "app_id": "A0/../x"}, {"id": "T2", "running": true, "app_name": "", "app_id": 7}]})).unwrap();
    assert_eq!(read.workspaces, [info("T1", true), info("T2", true)]);
    // `unread`: absent, null or not a list is not known; a list is known, of its readable entries (a name past `MAX_NAME`
    // is none, the entry kept) the first `MAX_WATCHED`; the entry is never refused for it.
    let long = "x".repeat(crate::chat::MAX_NAME + 1);
    let many: Vec<Value> = [json!({"id": "not a channel"}), json!({"id": "G0PRIVATE1", "name": long}), json!(7)]
        .into_iter()
        .chain((0..40).map(|n| json!({"id": format!("C{n:08}"), "name": "lab-general"})))
        .collect();
    let read = |unread: Value| -> Option<Vec<ChatUnread>> {
        let res: ChatWorkspacesRes = serde_json::from_value(json!({"default": "T1", "workspaces": [{"id": "T1", "running": true, "unread": unread}]})).unwrap();
        res.workspaces[0].unread.clone()
    };
    for unknown in [Value::Null, json!(7), json!({"id": "G0PRIVATE1"})] {
        assert_eq!(read(unknown.clone()), None, "{unknown}");
    }
    let known = read(json!(many)).expect("a list is known");
    assert_eq!((known.len(), &known[0], known[1].id.as_str()), (MAX_WATCHED, &unread("G0PRIVATE1", None), "C00000000"));
    assert_eq!(read(json!([])), Some(Vec::new()), "an empty list is known, and says the app reads every mapped channel");
    pinned(
        ChatChannelsRes {
            channels: vec![ChatChannelInfo { id: channel("C1"), name: "general".into(), private: false, member: true }],
            more: true,
        },
        json!({"channels": [{"id": "C1", "name": "general", "private": false, "member": true}], "more": true}),
    );
    pinned(ChatChannelsRes { channels: vec![], more: false }, json!({"channels": []}));
    pinned(ChatPairReq { row: "ws-a-1".into(), workspace: None, channel: Some(channel("C1")) }, json!({"row": "ws-a-1", "channel": "C1"}));
    pinned(ChatPairReq { row: "ws-a-1".into(), workspace: None, channel: None }, json!({"row": "ws-a-1", "channel": null}));
    pinned(
        ChatPairReq { row: "ws-a-1".into(), workspace: Some("T2".into()), channel: Some(channel("C1")) },
        json!({"row": "ws-a-1", "workspace": "T2", "channel": "C1"}),
    );
    pinned(ChatPairRes {}, json!({}));
}

/// An absent channel unpairs as a null one does.
/// The setup requests name their workspace or none: the last release's payloads, frozen here, decode with none (the
/// file's workspace), a present one is an id within its bound, and a payload naming one decodes in an older reader's
/// shape, which ignores the field it does not know.
#[test]
fn the_setup_family_carries_its_workspace_and_an_older_shape_still_decodes() {
    let channels: ChatChannelsReq = serde_json::from_value(json!({})).unwrap();
    let pair: ChatPairReq = serde_json::from_value(json!({"row": "r", "channel": "C1"})).unwrap();
    assert_eq!((channels.workspace, pair.workspace), (None, None));
    let null: ChatPairReq = serde_json::from_value(json!({"row": "r", "workspace": null})).unwrap();
    assert_eq!(null.workspace, None);
    for bad in [json!(""), json!("T 1"), json!(5), json!("T".repeat(65))] {
        assert!(serde_json::from_value::<ChatChannelsReq>(json!({"workspace": bad})).is_err(), "{bad}");
        assert!(serde_json::from_value::<ChatPairReq>(json!({"row": "r", "workspace": bad})).is_err(), "{bad}");
    }
    #[derive(serde::Deserialize)]
    struct OlderPair {
        #[allow(dead_code)]
        row: String,
        channel: Option<ChatChannel>,
    }
    let older: OlderPair = serde_json::from_value(json!({"row": "r", "workspace": "T2", "channel": "C1"})).unwrap();
    assert_eq!(older.channel, Some(channel("C1")));
    // An older daemon's connect reply and unconfigured resolve decode with no name and no workspace; a bad name reads as
    // none and keeps the reply, as does a workspace that is no id.
    let connected: ChatConnectRes = serde_json::from_value(json!({"workspace": "T1"})).unwrap();
    assert_eq!(connected.name, None);
    for bad in [json!(""), json!(5), json!("n".repeat(161))] {
        let r: ChatConnectRes = serde_json::from_value(json!({"workspace": "T1", "name": bad})).unwrap();
        assert_eq!(r.name, None, "{bad}");
    }
    // A mapped reply names its workspace only on a hub serving several: an older daemon's has none, a bad name reads as
    // none and keeps the mapping, and an older reader ignores the field.
    let mapped = |extra: Value| {
        let mut r = json!({"state": "mapped", "workspace": "T1", "channel": "C1", "delivers": true});
        r["workspace_name"] = extra;
        serde_json::from_value::<ChatResolveRes>(r).unwrap()
    };
    let workspace_name = |r: ChatResolveRes| match r {
        ChatResolveRes::Mapped { workspace_name, channel, .. } => {
            assert_eq!(channel, target().channel, "the mapping survives");
            workspace_name
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(workspace_name(mapped(json!("Lab"))), Some("Lab".into()));
    for bad in [json!(""), json!(5), json!(null), json!("n".repeat(161))] {
        assert_eq!(workspace_name(mapped(bad.clone())), None, "{bad}");
    }
    let older: ChatResolveRes = serde_json::from_value(json!({"state": "mapped", "workspace": "T1", "channel": "C1", "delivers": true})).unwrap();
    assert_eq!(workspace_name(older), None);
    let off: ChatResolveRes = serde_json::from_value(json!({"state": "unconfigured", "reason": "not_connected"})).unwrap();
    assert_eq!(off, ChatResolveRes::Unconfigured { reason: ChatUnconfigured::NotConnected, workspace: None });
    let bad: ChatResolveRes = serde_json::from_value(json!({"state": "unconfigured", "reason": "not_connected", "workspace": "T 1"})).unwrap();
    assert_eq!(bad, ChatResolveRes::Unconfigured { reason: ChatUnconfigured::NotConnected, workspace: None });
}

/// C13: a workspace list is cosmetic for its reader: past `MAX_WORKSPACES` it stops at the bound, an entry it cannot
/// read is skipped, and neither refuses the reply; a name that is not one reads as none.
#[test]
fn a_workspace_list_past_its_bound_or_with_a_bad_entry_still_decodes() {
    let entry = |i: usize| json!({"id": format!("T{i}"), "name": format!("w{i}"), "running": true});
    let long: Vec<Value> = (0..MAX_WORKSPACES + 3).map(entry).collect();
    let res: ChatWorkspacesRes = serde_json::from_value(json!({"default": "T0", "workspaces": long})).unwrap();
    assert_eq!(res.workspaces.len(), MAX_WORKSPACES);
    assert_eq!(res.workspaces[0].id, "T0");
    let mixed = json!({"default": "T0", "workspaces": [{"id": "not an id", "running": true}, {"running": true}, 7, {"id": "T1", "name": 5, "running": false}]});
    let res: ChatWorkspacesRes = serde_json::from_value(mixed).unwrap();
    assert_eq!(res.workspaces, [ChatWorkspaceInfo { id: "T1".into(), name: None, running: false, app_name: None, app_id: None, unread: None }]);
    assert!(serde_json::from_value::<ChatWorkspacesRes>(json!({"workspaces": []})).is_err(), "a reply names its default");
}

#[test]
fn a_pair_with_no_channel_key_unpairs() {
    let req: ChatPairReq = serde_json::from_value(json!({"row": "r"})).unwrap();
    assert_eq!(req.channel, None);
}

/// A token is a `ChatToken` in a request's derived `Debug`, plain or pretty, and a decode refusal names no byte of it.
#[test]
fn a_connect_request_formats_no_token_byte() {
    let (bot, app) = (token("xoxb-", 20), token("xapp-", 20));
    let user = token("xoxp-", 20);
    let req = ChatConnectReq { bot_token: bot.clone(), user_token: Some(user.clone()), app_token: app.clone() };
    for shown in [format!("{req:?}"), format!("{req:#?}"), format!("{bot:?}"), format!("{:?}", Some(&app)), format!("{user:?}")] {
        assert!(!shown.contains("a1a1") && !shown.contains("xoxb") && !shown.contains("xoxp") && !shown.contains("xapp"), "a Debug form holds token bytes");
        assert!(shown.contains("<redacted>"), "a Debug form says the token is redacted");
    }
    // The wire still carries the bytes whole, and the accessor is how the one sender reads them.
    let wire = serde_json::to_string(&req).unwrap();
    assert!(wire.contains(bot.expose()) && wire.contains(app.expose()) && wire.contains(user.expose()));
    let too_long = json!({"bot_token": format!("xoxb-{}", "a1".repeat(300)), "app_token": "xapp-a"});
    let refused = serde_json::from_value::<ChatConnectReq>(too_long).unwrap_err().to_string();
    assert!(!refused.contains("a1a1"), "a decode refusal holds token bytes");
}

/// The resolve reply as a window before `unconfigured` decoded it: the new state reads as the state it keeps for any
/// later one, which the window shows as its no-configuration words.
#[test]
fn an_older_reader_reads_unconfigured_as_invalid() {
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    enum Before {
        Unmapped,
        #[serde(other)]
        Invalid,
    }
    for reason in ["not_connected", "tokens_refused", "start_failed", "from_the_future"] {
        let reply = json!({"state": "unconfigured", "reason": reason});
        assert_eq!(serde_json::from_value::<Before>(reply).unwrap(), Before::Invalid, "{reason}");
    }
    let later = json!({"state": "unconfigured", "reason": "from_the_future"});
    assert_eq!(serde_json::from_value::<ChatResolveRes>(later).unwrap(), ChatResolveRes::Unconfigured { reason: ChatUnconfigured::Other, workspace: None });
}

/// The reason an ended service's chat gives for another app's token, as it is spelled on the wire.
#[test]
fn the_wrong_app_reason_is_pinned_on_the_wire() {
    pinned(ChatResolveRes::Unconfigured { reason: ChatUnconfigured::WrongApp, workspace: None }, json!({"state": "unconfigured", "reason": "wrong_app"}));
    pinned(
        ChatResolveRes::Unconfigured { reason: ChatUnconfigured::TokensRefused, workspace: Some("T2".into()) },
        json!({"state": "unconfigured", "reason": "tokens_refused", "workspace": "T2"}),
    );
}

/// A full list, each name at its bound and doubled by escaping (the daemon's names carry no control character, which
/// would be six bytes each), fits one frame; the decoder bounds the entries and the name, not the escaped size.
#[test]
fn a_full_channel_list_fits_the_envelope() {
    let entry = ChatChannelInfo { id: channel(&"C".repeat(64)), name: "\"".repeat(MAX_NAME), private: true, member: true };
    let full = ChatChannelsRes { channels: vec![entry; MAX_LISTED], more: true };
    let bytes = serde_json::to_vec(&full).unwrap().len();
    assert!(bytes < crate::codec::MAX_ENVELOPE_BYTES, "{bytes} bytes of a full list");
}

/// A connect carries a user token an older payload lacks: the old shape decodes with `None`, `None` is not written, and
/// the user token is bounded like the others.
#[test]
fn a_connect_carries_a_user_token_an_older_payload_lacks() {
    let old: ChatConnectReq = serde_json::from_value(json!({"bot_token": "b", "app_token": "a"})).unwrap();
    assert_eq!(old.user_token, None);
    assert!(!serde_json::to_string(&old).unwrap().contains("user_token"));
    let user = |n: usize| json!({"bot_token": "b", "user_token": "u".repeat(n), "app_token": "a"});
    assert!(serde_json::from_value::<ChatConnectReq>(user(MAX_TOKEN)).is_ok());
    assert!(serde_json::from_value::<ChatConnectReq>(user(MAX_TOKEN + 1)).is_err());
}

#[test]
fn the_setup_payloads_stop_at_their_bounds() {
    let connect = |n: usize| json!({"bot_token": "b".repeat(n), "app_token": "a"});
    assert!(serde_json::from_value::<ChatConnectReq>(connect(MAX_TOKEN)).is_ok());
    assert!(serde_json::from_value::<ChatConnectReq>(connect(MAX_TOKEN + 1)).is_err());
    let app = |n: usize| json!({"bot_token": "b", "app_token": "a".repeat(n)});
    assert!(serde_json::from_value::<ChatConnectReq>(app(MAX_TOKEN)).is_ok());
    assert!(serde_json::from_value::<ChatConnectReq>(app(MAX_TOKEN + 1)).is_err());
    let entry = json!({"id": "C1", "name": "n", "private": false, "member": false});
    let list = |n: usize| json!({"channels": vec![entry.clone(); n]});
    assert!(serde_json::from_value::<ChatChannelsRes>(list(MAX_LISTED)).is_ok());
    assert!(serde_json::from_value::<ChatChannelsRes>(list(MAX_LISTED + 1)).is_err());
    let named = |n: usize| json!({"channels": [{"id": "C1", "name": "n".repeat(n), "private": false, "member": false}]});
    assert!(serde_json::from_value::<ChatChannelsRes>(named(MAX_NAME)).is_ok());
    assert!(serde_json::from_value::<ChatChannelsRes>(named(MAX_NAME + 1)).is_err());
    let bad_id = json!({"channels": [{"id": "C 1", "name": "n", "private": false, "member": false}]});
    assert!(serde_json::from_value::<ChatChannelsRes>(bad_id).is_err());
    let row = |n: usize| json!({"row": "r".repeat(n), "channel": "C1"});
    assert!(serde_json::from_value::<ChatPairReq>(row(256)).is_ok());
    assert!(serde_json::from_value::<ChatPairReq>(row(257)).is_err());
    assert!(serde_json::from_value::<ChatPairReq>(json!({"row": "r", "channel": "c 1"})).is_err());
    assert!(serde_json::from_value::<ChatConnectRes>(json!({"workspace": "T".repeat(65)})).is_err());
}

#[test]
fn a_page_carries_records_that_round_trip() {
    let page = ChatHistoryRes { records: vec![record("a"), record("b")], next: Some("n".into()), limited: true };
    let back: ChatHistoryRes = serde_json::from_str(&serde_json::to_string(&page).unwrap()).unwrap();
    assert_eq!(back, page);
}

#[test]
fn a_code_added_later_reads_as_other() {
    let r: ChatRefusal = serde_json::from_value(json!({"error": "text", "code": "from_the_future"})).unwrap();
    assert_eq!(r.code, Some(ChatCode::Other));
}

#[test]
fn an_older_daemons_unknown_op_refusal_reads_as_a_refusal_with_no_code() {
    let r: ChatRefusal = serde_json::from_value(json!({"error": "unknown op: chat.post"})).unwrap();
    assert_eq!(r.code, None);
}

#[test]
fn a_phase_or_outcome_added_later_reads_as_the_safe_one() {
    let status: ChatStatusEvt = serde_json::from_value(json!({"service": "paused", "gap": false, "resync": ["C1"]})).unwrap();
    assert_eq!((status.service, status.resync.len()), (ChatService::Other, 1));
    let post: ChatPostRes = serde_json::from_value(json!({"outcome": "queued", "ts": "1.000001"})).unwrap();
    assert_eq!(post, ChatPostRes::Unknown);
}

#[test]
fn a_wait_is_clamped_to_an_hour() {
    let limited = |n: u64| json!({"outcome": "limited", "retry_after_secs": n, "assumed": false});
    let wait = |n: u64| match serde_json::from_value::<ChatPostRes>(limited(n)).unwrap() {
        ChatPostRes::Limited { retry_after_secs, .. } => retry_after_secs,
        other => panic!("{other:?}"),
    };
    assert_eq!((wait(30), wait(MAX_RETRY_SECS), wait(MAX_RETRY_SECS + 1), wait(u64::MAX)), (30, 3600, 3600, 3600));
}

#[test]
fn a_state_added_later_reads_as_a_safe_one() {
    let watch: ChatWatchRes = serde_json::from_value(json!({"state": "stale"})).unwrap();
    assert_eq!(watch.state, ChatWatchState::Other);
    let resolve: ChatResolveRes = serde_json::from_value(json!({"state": "disabled"})).unwrap();
    assert_eq!(resolve, ChatResolveRes::Invalid);
}

#[test]
fn a_record_with_an_origin_added_later_does_not_spoil_its_page() {
    let mut rec = serde_json::to_value(record("x")).unwrap();
    rec["origin"] = json!("webhook");
    let page: ChatHistoryRes = serde_json::from_value(json!({"records": [rec, serde_json::to_value(record("y")).unwrap()]})).unwrap();
    assert_eq!(page.records[0].origin, Origin::Other);
    assert_eq!(page.records.len(), 2);
}

#[test]
fn a_legal_page_fits_the_envelope() {
    let page = MAX_PAGE_RECORDS * crate::chat::MAX_RECORD_BYTES + MAX_CURSOR;
    assert!(page < crate::codec::MAX_ENVELOPE_BYTES, "{page} bytes of a full page");
}

#[test]
fn a_number_where_a_string_belongs_is_refused() {
    let post = json!({"target": target_json(), "kind": "row", "name": 5, "text": "t"});
    assert!(serde_json::from_value::<ChatPostReq>(post).is_err());
    let history = json!({"target": target_json(), "limit": 5, "after": 1.5});
    assert!(serde_json::from_value::<ChatHistoryReq>(history).is_err());
}

#[test]
fn a_timestamp_with_the_wrong_digits_is_refused_in_a_request() {
    let history = json!({"target": target_json(), "limit": 5, "after": "1700000000.1234567"});
    assert!(serde_json::from_value::<ChatHistoryReq>(history).is_err());
    let post = json!({"target": target_json(), "thread": "1.12345", "kind": "row", "name": "n", "text": "t"});
    assert!(serde_json::from_value::<ChatPostReq>(post).is_err());
}

#[test]
fn both_upper_bounds_cannot_be_asked_for() {
    let both = json!({"target": target_json(), "limit": 5, "upper": {"before": "1.000001", "until": "2.000002"}});
    assert!(serde_json::from_value::<ChatHistoryReq>(both).is_err());
}

#[test]
fn a_limit_is_one_to_twenty() {
    for (n, ok) in [(0, false), (1, true), (20, true), (21, false)] {
        let history = json!({"target": target_json(), "limit": n});
        assert_eq!(serde_json::from_value::<ChatHistoryReq>(history).is_ok(), ok, "limit {n}");
    }
}

#[test]
fn a_cursor_a_text_and_a_name_stop_at_their_bounds() {
    let history = |c: usize| json!({"target": target_json(), "limit": 5, "cursor": "c".repeat(c)});
    assert!(serde_json::from_value::<ChatHistoryReq>(history(1024)).is_ok());
    assert!(serde_json::from_value::<ChatHistoryReq>(history(1025)).is_err());
    let res = |c: usize| json!({"records": [], "next": "c".repeat(c)});
    assert!(serde_json::from_value::<ChatHistoryRes>(res(1025)).is_err());
    let post = |name: usize, text: usize| {
        json!({"target": target_json(), "kind": "row", "name": "n".repeat(name), "text": "t".repeat(text)})
    };
    assert!(serde_json::from_value::<ChatPostReq>(post(160, 8192)).is_ok());
    assert!(serde_json::from_value::<ChatPostReq>(post(160, 8193)).is_err());
    assert!(serde_json::from_value::<ChatPostReq>(post(161, 1)).is_err());
}

/// A name is never a reason to refuse the mapping: one at the bound is kept, and one that is too long, empty, not a
/// string or null reads as no name, the reply still mapped.
#[test]
fn a_name_that_is_too_long_or_not_a_string_reads_as_no_name() {
    let name_of = |name: Value| match serde_json::from_value::<ChatResolveRes>(json!({"state": "mapped", "workspace": "T1", "channel": "C1", "delivers": true, "name": name})).unwrap() {
        ChatResolveRes::Mapped { name, channel, .. } => {
            assert_eq!(channel.as_str(), "C1", "the mapping survives");
            name
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(name_of(json!("n".repeat(160))), Some("n".repeat(160)));
    for bad in [json!("n".repeat(161)), json!(""), json!(7), json!(["a"]), json!({"a": 1}), Value::Null] {
        assert_eq!(name_of(bad.clone()), None, "{bad}");
    }
}

/// The resolve reply as the window before `name` decoded it: the new reply, with its name, still reads, and an older
/// daemon's reply, with none, reads as a mapped channel with no name.
#[test]
fn an_older_reader_decodes_the_named_reply_and_this_one_the_unnamed() {
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    enum Before {
        Mapped { workspace: String, channel: ChatChannel, delivers: bool },
        Unmapped,
        #[serde(other)]
        Invalid,
    }
    let named = json!({"state": "mapped", "workspace": "T1", "channel": "C1", "delivers": true, "name": "general"});
    assert_eq!(serde_json::from_value::<Before>(named).unwrap(), Before::Mapped { workspace: "T1".into(), channel: target().channel, delivers: true });
    let unnamed = json!({"state": "mapped", "workspace": "T1", "channel": "C1", "delivers": true});
    assert_eq!(
        serde_json::from_value::<ChatResolveRes>(unnamed).unwrap(),
        ChatResolveRes::Mapped { workspace: "T1".into(), channel: target().channel, delivers: true, name: None, workspace_name: None }
    );
}

#[test]
fn a_target_names_a_real_channel_and_bounded_ids() {
    for good in ["C1", "c_1-X"] {
        let t = json!({"row": "r", "workspace": "T1", "channel": good});
        assert!(serde_json::from_value::<ChatTarget>(t).is_ok(), "{good:?}");
    }
    for bad in ["", "c 1", "c/1", "cé", &"c".repeat(65)] {
        let t = json!({"row": "r", "workspace": "T1", "channel": bad});
        assert!(serde_json::from_value::<ChatTarget>(t).is_err(), "{bad:?}");
    }
    let long_row = json!({"row": "r".repeat(257), "workspace": "T1", "channel": "C1"});
    assert!(serde_json::from_value::<ChatTarget>(long_row).is_err());
    let long_workspace = json!({"row": "r", "workspace": "T".repeat(65), "channel": "C1"});
    assert!(serde_json::from_value::<ChatTarget>(long_workspace).is_err());
    let long = "W".repeat(65);
    let mapped = json!({"state": "mapped", "workspace": long, "channel": "C1", "delivers": true});
    assert!(serde_json::from_value::<ChatResolveRes>(mapped).is_err());
    let changed = json!({"workspace": long, "channel": "C1", "put": [], "deleted": []});
    assert!(serde_json::from_value::<ChatChangedEvt>(changed).is_err());
}

#[test]
fn a_list_of_records_stops_at_a_page_and_an_event_at_its_bounds() {
    let page = |n: usize| json!({"records": vec![serde_json::to_value(record("x")).unwrap(); n]});
    assert!(serde_json::from_value::<ChatHistoryRes>(page(21)).is_ok());
    assert!(serde_json::from_value::<ChatHistoryRes>(page(22)).is_err());
    let put = |n: usize| json!({"workspace": "T", "channel": "C1", "put": vec![serde_json::to_value(record("x")).unwrap(); n], "deleted": []});
    assert!(serde_json::from_value::<ChatChangedEvt>(put(21)).is_ok());
    assert!(serde_json::from_value::<ChatChangedEvt>(put(22)).is_err());
    let event = |n: usize| json!({"workspace": "T", "channel": "C1", "put": [], "deleted": vec!["1.000001"; n]});
    assert!(serde_json::from_value::<ChatChangedEvt>(event(21)).is_ok());
    assert!(serde_json::from_value::<ChatChangedEvt>(event(22)).is_err());
    let status = |n: usize| json!({"service": "running", "gap": false, "resync": vec!["C1"; n]});
    assert!(serde_json::from_value::<ChatStatusEvt>(status(32)).is_ok());
    assert!(serde_json::from_value::<ChatStatusEvt>(status(33)).is_err());
}

#[test]
fn every_code_has_its_one_text() {
    let table = [
        (ChatCode::Unavailable, "the chat is not available here"),
        (ChatCode::Unmapped, "the row's folder maps to no channel"),
        (ChatCode::StaleRow, "the row is gone or was replaced"),
        (ChatCode::StaleMapping, "the row now maps to another channel"),
        (ChatCode::BadRequest, "the request is not valid"),
        (ChatCode::TooManyWatches, "too many channels are being followed at once"),
        (ChatCode::Limited, "Slack asked for a pause; try again in a moment"),
        (ChatCode::Busy, "the chat is busy; try again in a moment"),
        (ChatCode::BotTokenRefused, "Slack refused the \"Bot User OAuth Token\""),
        (ChatCode::AppTokenRefused, "Slack refused the \"App-Level Token\""),
        (ChatCode::UserTokenRefused, "Slack refused the \"User OAuth Token\""),
        (ChatCode::NoUserToken, "no Slack code that posts as you is saved; open the channel list (Ctrl+Shift+k, or Cmd+Shift+k on a Mac) and choose its last entry, Connect a Slack workspace (Connect Slack again on an older computer)"),
        (ChatCode::NotAMember, "you are not in that channel in Slack; join it there first"),
        (ChatCode::OtherWorkspace, "these Slack codes are for another Slack workspace, and Ship of Tools on the computer that runs your agents is too old to connect more than one: whoever runs Ship of Tools there can update it"),
        (ChatCode::TooManyWorkspaces, "the computer that runs your agents already holds 8 Slack workspaces, the most it can: to add another, remove one workspace's file chat-secrets-<id>.toml (<id> is that workspace's ID) from Ship of Tools' settings folder there, ~/.config/sot on Linux and macOS or %LOCALAPPDATA%\\sot\\config on Windows"),
        (ChatCode::NoSuchChannel, "that channel is not in this Slack workspace, or it is a private channel your Slack app is not in"),
        (ChatCode::NotInChannel, "your Slack app is not in that channel"),
        (ChatCode::ChannelTaken, "another project already uses that channel"),
        (ChatCode::NotSaved, "the chat settings could not be saved"),
        (ChatCode::NotAFile, "the file is not a readable file in the row's folder"),
        (ChatCode::TooLarge, "the file is larger than the chat carries"),
        (ChatCode::NotAnImage, "that file is not a picture the chat can show"),
        (ChatCode::Failed, "the chat request failed"),
        (ChatCode::Other, "the chat request failed"),
    ];
    for (code, words) in table {
        assert_eq!(code.text(), words, "{code:?}");
    }
    for code in ["bot_token_refused", "app_token_refused", "user_token_refused", "no_user_token", "not_a_member", "other_workspace", "not_in_channel", "channel_taken", "not_saved", "not_a_file", "too_large", "not_an_image", "too_many_workspaces", "no_such_channel"] {
        let r: ChatRefusal = serde_json::from_value(json!({"error": "text", "code": code})).unwrap();
        assert_ne!(r.code, Some(ChatCode::Other), "{code} is a code of this reader");
    }
}

fn post_file_json() -> Value {
    json!({"target": target_json(), "thread": "1.000001", "kind": "row", "name": "r", "node_id": "files:figs/a.png", "comment": "fig 1"})
}

fn file_res(len: u64, mime: &str) -> Value {
    json!({"mime": mime, "blob": {"len": len, "mime": mime}})
}

#[test]
fn the_file_pair_keeps_its_shape_and_its_bounds() {
    pinned(
        ChatPostFileReq {
            target: target(),
            thread: Some(ts("1.000001")),
            kind: ChatPostKind::Row,
            name: "r".into(),
            node_id: "files:figs/a.png".into(),
            comment: Some("fig 1".into()),
        },
        post_file_json(),
    );
    pinned(
        ChatPostFileReq { target: target(), thread: None, kind: ChatPostKind::Person, name: "p".into(), node_id: "files:a".into(), comment: None },
        json!({"target": target_json(), "thread": null, "kind": "person", "name": "p", "node_id": "files:a", "comment": null}),
    );
    pinned(
        ChatPostFileRes::Posted { file: "F1".into(), ts: Some(ts("3.000003")) },
        json!({"outcome": "posted", "file": "F1", "ts": "3.000003"}),
    );
    pinned(ChatPostFileRes::Posted { file: "F1".into(), ts: None }, json!({"outcome": "posted", "file": "F1"}));
    pinned(ChatPostFileRes::Refused { code: ChatCode::NotAFile }, json!({"outcome": "refused", "code": "not_a_file"}));
    pinned(
        ChatPostFileRes::Limited { retry_after_secs: 30, assumed: false },
        json!({"outcome": "limited", "retry_after_secs": 30, "assumed": false}),
    );
    pinned(ChatPostFileRes::Unknown, json!({"outcome": "unknown"}));
    pinned(
        ChatFileReq { target: target(), file: "F1".into(), variant: ChatFileVariant::Thumb },
        json!({"target": target_json(), "file": "F1", "variant": "thumb"}),
    );
    pinned(
        ChatFileReq { target: target(), file: "F1".into(), variant: ChatFileVariant::Original },
        json!({"target": target_json(), "file": "F1", "variant": "original"}),
    );
    let res: ChatFileRes = serde_json::from_value(file_res(10, "image/png")).unwrap();
    assert_eq!((res.mime.as_str(), res.blob.len, res.blob.mime.as_str()), ("image/png", 10, "image/png"));
    assert_eq!(serde_json::to_value(&res).unwrap(), file_res(10, "image/png"));

    // The bounds, each at its edge and one past.
    assert_eq!((MAX_CHAT_FILE, MAX_THUMB), (16 * 1024 * 1024, 256 * 1024));
    let req = |f: &dyn Fn(&mut Value)| {
        let mut v = post_file_json();
        f(&mut v);
        serde_json::from_value::<ChatPostFileReq>(v).is_ok()
    };
    assert!(req(&|v| v["node_id"] = json!("n".repeat(MAX_NODE_ID))));
    assert!(!req(&|v| v["node_id"] = json!("n".repeat(MAX_NODE_ID + 1))));
    assert!(req(&|v| v["comment"] = json!("c".repeat(MAX_TEXT))));
    assert!(!req(&|v| v["comment"] = json!("c".repeat(MAX_TEXT + 1))));
    assert!(req(&|v| v["name"] = json!("n".repeat(MAX_NAME))));
    assert!(!req(&|v| v["name"] = json!("n".repeat(MAX_NAME + 1))));
    assert!(req(&|v| {
        v.as_object_mut().unwrap().remove("comment");
    }));
    assert!(!req(&|v| v["node_id"] = json!(5)));
    assert!(!req(&|v| v["thread"] = json!("1.12345")));
    assert!(!req(&|v| v["kind"] = json!("bot")));
    let file = |id: &str, variant: &str| json!({"target": target_json(), "file": id, "variant": variant});
    assert!(serde_json::from_value::<ChatFileReq>(file(&"F".repeat(64), "thumb")).is_ok());
    for bad in ["", "F 1", "F/1", "Fé", &"F".repeat(65)] {
        assert!(serde_json::from_value::<ChatFileReq>(file(bad, "thumb")).is_err(), "{bad:?}");
    }
    assert!(serde_json::from_value::<ChatFileReq>(file("F1", "large")).is_err(), "a variant of this reader's two");
    let reply = |len: u64, mime: &str| serde_json::from_value::<ChatFileRes>(file_res(len, mime)).is_ok();
    assert!(reply(MAX_CHAT_FILE as u64, "image/png"));
    assert!(!reply(MAX_CHAT_FILE as u64 + 1, "image/png"));
    assert!(!reply(u64::MAX, "image/png"));
    assert!(reply(0, &"m".repeat(crate::chat::MAX_MIME)));
    assert!(!reply(0, &"m".repeat(crate::chat::MAX_MIME + 1)));
    assert!(serde_json::from_value::<ChatFileRes>(json!({"mime": "image/png"})).is_err(), "a reply names its blob");
    let posted = |id: &str| serde_json::from_value::<ChatPostFileRes>(json!({"outcome": "posted", "file": id})).is_ok();
    assert!(posted("F1") && !posted("") && !posted("F 1") && !posted(&"F".repeat(65)));
}

#[test]
fn a_file_outcome_or_code_added_later_reads_as_the_safe_one() {
    let post: ChatPostFileRes = serde_json::from_value(json!({"outcome": "queued", "file": "F1"})).unwrap();
    assert_eq!(post, ChatPostFileRes::Unknown);
    let wait = |n: u64| match serde_json::from_value::<ChatPostFileRes>(json!({"outcome": "limited", "retry_after_secs": n, "assumed": true})).unwrap() {
        ChatPostFileRes::Limited { retry_after_secs, .. } => retry_after_secs,
        other => panic!("{other:?}"),
    };
    assert_eq!((wait(30), wait(u64::MAX)), (30, MAX_RETRY_SECS));
    let refused: ChatPostFileRes = serde_json::from_value(json!({"outcome": "refused", "code": "from_the_future"})).unwrap();
    assert_eq!(refused, ChatPostFileRes::Refused { code: ChatCode::Other });
    // The codes of this pair, read by a reader before them (no such variants), are `other`.
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Before {
        BadRequest,
        #[serde(other)]
        Other,
    }
    for code in ["not_a_file", "too_large", "not_an_image"] {
        assert_eq!(serde_json::from_value::<Before>(json!(code)).unwrap(), Before::Other, "{code}");
        assert_ne!(serde_json::from_value::<ChatCode>(json!(code)).unwrap(), ChatCode::Other, "{code}");
    }
    // An older daemon answers either op as an unknown op: a refusal with no code.
    for op in ["chat.post_file", "chat.file"] {
        let r: ChatRefusal = serde_json::from_value(json!({"error": format!("unknown op: {op}")})).unwrap();
        assert_eq!(r.code, None);
    }
}
