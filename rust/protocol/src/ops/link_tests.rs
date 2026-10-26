use super::*;
use crate::Frame;

/// The link family's wire lines, frozen: a change to any of them is a change to the link and raises `LINK`.
#[test]
fn link_payloads_keep_their_shape() {
    let line = |f: Frame| serde_json::to_string(&f).unwrap();
    let hello = Frame::req(1, op::LINK_HELLO, serde_json::to_value(LinkHelloReq { link: LINK }).unwrap());
    assert_eq!(line(hello), r#"{"v":4,"id":1,"kind":"req","op":"link.hello","payload":{"link":1}}"#);
    let answer = Frame::res(1, op::LINK_HELLO, serde_json::to_value(LinkHelloRes { link: 1, build: "0.7.0".into() }).unwrap());
    assert_eq!(line(answer), r#"{"v":4,"id":1,"kind":"res","op":"link.hello","payload":{"build":"0.7.0","link":1}}"#);
    let open = Frame::req(2, op::LINK_OPEN, serde_json::to_value(LinkOpenReq { ch: 3 }).unwrap());
    assert_eq!(line(open), r#"{"v":4,"id":2,"kind":"req","op":"link.open","payload":{"ch":3}}"#);
    let opened = Frame::res(2, op::LINK_OPEN, serde_json::to_value(LinkOpenRes { ch: 3, ok: true }).unwrap());
    assert_eq!(line(opened), r#"{"v":4,"id":2,"kind":"res","op":"link.open","payload":{"ch":3,"ok":true}}"#);
    let refused = LinkRefusal { error: "the daemon did not answer".into(), code: "unreachable".into(), ch: Some(3), link: None };
    let refused = Frame::res(2, op::LINK_OPEN, serde_json::to_value(refused).unwrap());
    assert_eq!(line(refused), r#"{"v":4,"id":2,"kind":"res","op":"link.open","payload":{"ch":3,"code":"unreachable","error":"the daemon did not answer"}}"#);
    let data = LinkDataEvt { ch: 3, blob: crate::BlobDescriptor { len: 5, mime: "application/octet-stream".into() } };
    let data = Frame::evt(op::LINK_DATA, serde_json::to_value(data).unwrap());
    assert_eq!(line(data), r#"{"v":4,"id":0,"kind":"evt","op":"link.data","payload":{"blob":{"len":5,"mime":"application/octet-stream"},"ch":3}}"#);
    let credit = Frame::evt(op::LINK_CREDIT, serde_json::to_value(LinkCreditEvt { ch: 3, bytes: 65536 }).unwrap());
    assert_eq!(line(credit), r#"{"v":4,"id":0,"kind":"evt","op":"link.credit","payload":{"bytes":65536,"ch":3}}"#);
    for (reason, word) in [(LinkCloseReason::Eof, "eof"), (LinkCloseReason::Refused, "refused"), (LinkCloseReason::Reset, "reset")] {
        let close = Frame::evt(op::LINK_CLOSE, serde_json::to_value(LinkCloseEvt { ch: 3, reason }).unwrap());
        assert_eq!(line(close), format!(r#"{{"v":4,"id":0,"kind":"evt","op":"link.close","payload":{{"ch":3,"reason":"{word}"}}}}"#));
    }
    // A hello answer from a router that names no build still reads.
    let bare: LinkHelloRes = serde_json::from_str(r#"{"link":1}"#).unwrap();
    assert_eq!(bare, LinkHelloRes { link: 1, build: String::new() });
    assert_eq!((LINK, CHUNK, WINDOW, MAX_CHANNELS), (1, 65_536, 262_144, 128));
}
