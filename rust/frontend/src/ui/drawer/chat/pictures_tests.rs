use std::collections::HashSet;
use std::io::Cursor;
use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use image::{ImageFormat, RgbaImage};
use sot_protocol::chat::{ChatFile, ChatRecord};
use sot_protocol::ops::{ChatCode, ChatFileReq, ChatFileVariant, ChatTarget};

use super::{decode, ChatPictures, MAX_HELD};
use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatPicture, ChatReply, OutgoingReq};
use crate::ui::drawer::chat::owner::Out;
use crate::ui::drawer::chat::thread_tests::{body, channel, shown};

fn host(h: &str) -> HostKey {
    h.to_string()
}

fn target() -> ChatTarget {
    serde_json::from_value(serde_json::json!({ "row": "ROW1", "workspace": "WS1", "channel": "CH1" })).unwrap()
}

fn file(id: &str, mime: &str) -> ChatFile {
    serde_json::from_value(serde_json::json!({ "id": id, "name": "figure", "mime": mime, "bytes": 100, "thumbnail": true })).unwrap()
}

fn pngs(n: usize) -> Vec<ChatFile> {
    (0..n).map(|i| file(&format!("F{i:03}"), "image/png")).collect()
}

fn encoded(w: u32, h: u32, format: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    RgbaImage::new(w, h).write_to(&mut out, format).unwrap();
    out.into_inner()
}

fn picture(w: u32, h: u32) -> ChatReply<ChatPicture> {
    ChatReply::Done(ChatPicture { mime: "image/png".into(), bytes: encoded(w, h, ImageFormat::Png) })
}

fn place(p: &mut ChatPictures, on: &str, files: &[ChatFile], now: Instant) {
    let h = host(on);
    p.place(Some((&h, &target())), files.iter().map(|f| ("WS1", "CH1", f)), now);
}

fn connected(hosts: &[&str], now: Instant) -> ChatPictures {
    let mut p = ChatPictures::default();
    hosts.iter().for_each(|h| p.on_connection(&host(h), true, now));
    p
}

/// The thumbnail requests among `outs`: their host and file.
fn asked(outs: &[Out]) -> Vec<(HostKey, ChatFileReq)> {
    outs.iter()
        .map(|(h, r)| match r {
            OutgoingReq::ChatFile { req } => {
                assert_eq!(req.variant, ChatFileVariant::Thumb);
                (h.clone(), req.clone())
            }
            _ => panic!("only thumbnails are asked for"),
        })
        .collect()
}

/// Answers every ask with a small picture until nothing more is asked; every request asked along the way, in order.
fn answer_all(p: &mut ChatPictures, mut outs: Vec<Out>, now: Instant) -> Vec<(HostKey, ChatFileReq)> {
    let mut all = Vec::new();
    while let Some((h, req)) = asked(&outs).into_iter().next() {
        all.push((h.clone(), req.clone()));
        outs.remove(0);
        outs.extend(p.on_reply(&h, &req, picture(4, 4), now));
    }
    all
}

#[test]
fn at_most_two_thumbnails_are_asked_per_host_and_none_twice() {
    let t0 = Instant::now();
    let mut p = connected(&["a", "b"], t0);
    // Five pictures and a PDF placed: two asked, the PDF never.
    let mut files = pngs(5);
    files.push(file("PDF1", "application/pdf"));
    place(&mut p, "a", &files, t0);
    assert_eq!(p.next_ask(), Some(t0), "the window wakes for the asks a placement owes");
    let first = asked(&p.asks(t0));
    assert_eq!(first.len(), 2);
    assert_eq!(p.next_ask(), None, "with two in flight no wake is owed");
    assert!(first.iter().all(|(h, r)| h == "a" && r.target == target() && r.file != "PDF1"));
    // Asked again with two in flight: nothing.
    assert!(p.asks(t0).is_empty());
    // Another host's pictures have their own two, whatever the first host has in flight.
    place(&mut p, "b", &pngs(3), t0);
    let other = asked(&p.asks(t0));
    assert_eq!(other.len(), 2);
    assert!(other.iter().all(|(h, _)| h == "b"));
    // The first host placed again: each answer asks one more, and none is asked twice.
    place(&mut p, "a", &files, t0);
    let (h, req) = first[0].clone();
    let mut all: Vec<_> = first.iter().map(|(_, r)| r.file.clone()).collect();
    let next = p.on_reply(&h, &req, picture(4, 4), t0);
    assert_eq!(asked(&next).len(), 1);
    all.extend(answer_all(&mut p, next, t0).into_iter().map(|(_, r)| r.file));
    let (h, req) = first[1].clone();
    let outs = p.on_reply(&h, &req, picture(4, 4), t0);
    all.extend(answer_all(&mut p, outs, t0).into_iter().map(|(_, r)| r.file));
    assert_eq!(all.len(), 5, "{all:?}");
    assert_eq!(all.iter().collect::<HashSet<_>>().len(), 5);
    assert!(p.asks(t0 + Duration::from_secs(3600)).is_empty());
    assert!(p.held("a", "WS1", "CH1", "F004").is_some());
}

#[test]
fn a_thumbnail_past_its_bounds_is_never_decoded_whole() {
    // The header is read first: a side past 2048 is refused before any pixel is decoded.
    assert!(decode(&encoded(2049, 1, ImageFormat::Png)).is_none());
    assert!(decode(&encoded(1, 2049, ImageFormat::Png)).is_none());
    // Within it, a picture is held at 360 on its long side, its shape kept.
    let held = decode(&encoded(1000, 500, ImageFormat::Png)).unwrap();
    assert_eq!(held.dimensions(), (360, 180));
    assert_eq!(decode(&encoded(2048, 2, ImageFormat::Png)).unwrap().dimensions(), (360, 1));
    assert_eq!(decode(&encoded(30, 20, ImageFormat::Png)).unwrap().dimensions(), (30, 20));
    // Only the four formats decode: a BMP is refused, and so are bytes that are no picture.
    assert!(decode(&encoded(4, 4, ImageFormat::Bmp)).is_none());
    assert!(decode(b"<html>sign in</html>").is_none());
    // An over-bound reply is held as nothing, and never asked again.
    let t0 = Instant::now();
    let mut p = connected(&["a"], t0);
    place(&mut p, "a", &pngs(1), t0);
    let (h, req) = asked(&p.asks(t0)).remove(0);
    assert!(p.on_reply(&h, &req, picture(2049, 1), t0).is_empty());
    assert!(p.held("a", "WS1", "CH1", "F000").is_none());
    assert!(p.asks(t0 + Duration::from_secs(3600)).is_empty());
}

#[test]
fn the_least_recently_placed_leaves_past_64() {
    let t0 = Instant::now();
    let mut p = connected(&["a"], t0);
    let files = pngs(MAX_HELD + 6);
    // A frame places at most 64 pictures: the rest are not asked for.
    place(&mut p, "a", &files, t0);
    let outs = p.asks(t0);
    let asked_first = answer_all(&mut p, outs, t0);
    assert_eq!(asked_first.len(), MAX_HELD);
    assert_eq!(p.held_count(), MAX_HELD);
    // The first is now the least recently placed; one more picture placed and held pushes it out, and only it.
    place(&mut p, "a", &files[1..MAX_HELD], t0);
    assert!(p.asks(t0).is_empty(), "a held picture is not asked again");
    place(&mut p, "a", &files[MAX_HELD..=MAX_HELD], t0);
    let outs = p.asks(t0);
    let more = answer_all(&mut p, outs, t0);
    assert_eq!(more.len(), 1);
    assert_eq!(p.held_count(), MAX_HELD);
    assert!(p.held("a", "WS1", "CH1", "F000").is_none());
    assert!(p.held("a", "WS1", "CH1", "F001").is_some());
    assert!(p.held("a", "WS1", "CH1", &files[MAX_HELD].id).is_some());
    // Placed again, the one that left is asked again.
    place(&mut p, "a", &files[..1], t0);
    assert_eq!(asked(&p.asks(t0)).len(), 1);
}

#[test]
fn a_refusal_is_final_or_asked_again_by_its_code() {
    let t0 = Instant::now();
    let s = Duration::from_secs;
    let ms = Duration::from_millis;
    // One picture placed, its ask in flight; `reply` answers it at `at` and says what is asked at `at + later`.
    let one = |p: &mut ChatPictures, at: Instant| asked(&p.asks(at)).into_iter().next();
    let start = || {
        let mut p = connected(&["a"], t0);
        place(&mut p, "a", &pngs(1), t0);
        let (h, req) = asked(&p.asks(t0)).remove(0);
        (p, h, req)
    };
    // Final for the window session: never asked again, whatever the link does.
    for code in [ChatCode::NotAnImage, ChatCode::TooLarge, ChatCode::BadRequest] {
        let (mut p, h, req) = start();
        assert!(p.on_reply(&h, &req, ChatReply::Refused(code), t0).is_empty());
        assert_eq!(p.next_ask(), None);
        p.on_connection(&h, false, t0);
        p.on_connection(&h, true, t0);
        assert!(one(&mut p, t0 + s(3600)).is_none(), "{code:?}");
    }
    // An answer that does not decode is final too.
    let (mut p, h, req) = start();
    p.on_reply(&h, &req, ChatReply::Done(ChatPicture { mime: "image/png".into(), bytes: b"not a picture".to_vec() }), t0);
    assert!(one(&mut p, t0 + s(3600)).is_none());
    // `failed`: asked again 5 s later, at most three times.
    let (mut p, h, req) = start();
    let mut at = t0;
    for _ in 0..3 {
        assert!(p.on_reply(&h, &req, ChatReply::Refused(ChatCode::Failed), at).is_empty());
        assert!(one(&mut p, at + ms(4900)).is_none());
        at += s(5);
        assert!(one(&mut p, at).is_some());
    }
    p.on_reply(&h, &req, ChatReply::Refused(ChatCode::Failed), at);
    assert!(one(&mut p, at + s(3600)).is_none(), "the fourth failure is final");
    // `busy` and `unavailable`: on the 5 s pace, as often as they come.
    for code in [ChatCode::Busy, ChatCode::Unavailable] {
        let (mut p, h, req) = start();
        let mut at = t0;
        for _ in 0..6 {
            p.on_reply(&h, &req, ChatReply::Refused(code), at);
            assert_eq!(p.next_ask(), Some(at + s(5)), "the window wakes for the retry");
            assert!(one(&mut p, at + ms(4900)).is_none());
            at += s(5);
            assert!(one(&mut p, at).is_some(), "{code:?}");
        }
    }
    // `limited`: after the limited pace, 30 s.
    let (mut p, h, req) = start();
    p.on_reply(&h, &req, ChatReply::Refused(ChatCode::Limited), t0);
    assert_eq!(p.next_ask(), Some(t0 + s(30)));
    assert!(one(&mut p, t0 + s(29)).is_none());
    assert!(one(&mut p, t0 + s(30)).is_some());
    // A daemon that does not know the op is not asked again until its link changes.
    let (mut p, h, req) = start();
    p.on_reply(&h, &req, ChatReply::Unsupported, t0);
    assert_eq!(p.next_ask(), None);
    assert!(one(&mut p, t0 + s(3600)).is_none());
    p.on_connection(&h, true, t0);
    assert!(one(&mut p, t0).is_some());
}

/// A record of `CH1` at `ts` carrying `files`.
fn record(ts: &str, files: &[ChatFile]) -> ChatRecord {
    serde_json::from_value(serde_json::json!({ "workspace": "WS1", "channel": "CH1", "ts": ts, "thread_ts": ts,
        "author": { "id": "U1", "name": "member one" }, "origin": "member", "text": "a figure", "reply_count": 0, "files": files })).unwrap()
}

fn thumbs(outs: &[Out]) -> Vec<String> {
    let mut files: Vec<String> = outs.iter().filter_map(|(_, r)| match r {
        OutgoingReq::ChatFile { req } => Some(req.file.clone()),
        _ => None,
    }).collect();
    files.sort();
    files
}

#[test]
fn a_link_change_asks_again_what_was_in_flight() {
    let mut fonts = FontSystem::new();
    let files = pngs(3);
    let (mut d, at) = channel(&[record("1700000001.000100", &files[..2]), record("1700000002.000100", &files[2..])]);
    // The drawer places the records, and its next frame asks for two of their thumbnails.
    d.layout(&mut fonts, body(), 1.0, body());
    let first = thumbs(&d.tick(shown("ROW1"), at).0);
    assert_eq!(first.len(), 2);
    assert!(thumbs(&d.tick(shown("ROW1"), at).0).is_empty());
    // Another host's link change leaves them in flight.
    d.on_connection(&host("b"), true, at);
    d.on_connection(&host("b"), false, at);
    assert!(thumbs(&d.tick(shown("ROW1"), at).0).is_empty());
    // The link goes: nothing is asked while it is down, and when it comes back the same two are asked again.
    d.on_connection(&host("a"), false, at);
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(thumbs(&d.tick(shown("ROW1"), at).0).is_empty());
    let outs = d.on_connection(&host("a"), true, at);
    let mut again = thumbs(&outs);
    d.layout(&mut fonts, body(), 1.0, body());
    again.extend(thumbs(&d.tick(shown("ROW1"), at).0));
    assert_eq!(again, first);
    // A reply goes to the pictures owner, and the third is asked in its place.
    let req = ChatFileReq { target: target(), file: first[0].clone(), variant: ChatFileVariant::Thumb };
    let next = d.on_evt(&host("a"), ChatEvt::File { req, reply: picture(4, 4) }, at);
    assert_eq!(thumbs(&next), ["F002"]);
}
