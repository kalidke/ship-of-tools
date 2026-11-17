use std::collections::HashSet;
use std::io::Cursor;
use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use image::{ImageFormat, RgbaImage};
use sot_protocol::chat::{ChatFile, ChatRecord, PICTURE_MIMES};
use sot_protocol::ops::{ChatCode, ChatFileReq, ChatFileVariant, ChatTarget};

use super::{decode, ChatPictures, MAX_FINAL, MAX_HELD};
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

/// A zlib stream of `n` zero bytes (`n - 1` a multiple of 258): one fixed-Huffman block, a literal zero then matches of 258
/// at distance 1, so 400 MiB takes about 2.6 MB.
fn zeros_zlib(n: usize) -> Vec<u8> {
    assert_eq!((n - 1) % 258, 0);
    let mut out = vec![0x78, 0x01];
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut put = |out: &mut Vec<u8>, value: u32, len: u32, msb_first: bool| {
        for i in 0..len {
            let bit = if msb_first { (value >> (len - 1 - i)) & 1 } else { (value >> i) & 1 };
            acc |= bit << bits;
            bits += 1;
            if bits == 8 {
                out.push(acc as u8);
                (acc, bits) = (0, 0);
            }
        }
    };
    put(&mut out, 1, 1, false); // the final block
    put(&mut out, 1, 2, false); // fixed Huffman codes
    put(&mut out, 0x30, 8, true); // literal 0
    for _ in 0..(n - 1) / 258 {
        put(&mut out, 0xC5, 8, true); // length 258 (symbol 285)
        put(&mut out, 0, 5, true); // distance 1
    }
    put(&mut out, 0, 7, true); // end of block
    if bits > 0 {
        out.push(acc as u8);
    }
    let b = (n % 65521) as u32;
    out.extend_from_slice(&((b << 16) | 1).to_be_bytes());
    out
}

/// An 8 x 8 PNG carrying an `iCCP` chunk whose profile inflates to about `inflated` bytes.
fn iccp_bomb(inflated: usize) -> Vec<u8> {
    let png = encoded(8, 8, ImageFormat::Png);
    let mut data = b"icc\0\0".to_vec();
    data.extend(zeros_zlib(1 + inflated / 258 * 258));
    let mut crc = flate2::Crc::new();
    crc.update(b"iCCP");
    crc.update(&data);
    let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(b"iCCP");
    chunk.extend(&data);
    chunk.extend_from_slice(&crc.sum().to_be_bytes());
    // After the signature (8 bytes) and IHDR (25).
    [&png[..33], &chunk, &png[33..]].concat()
}

#[test]
fn at_most_one_thumbnail_is_asked_per_host_and_none_twice() {
    let t0 = Instant::now();
    let mut p = connected(&["a", "b"], t0);
    // Five pictures and a PDF placed: one asked, the PDF never.
    let mut files = pngs(5);
    files.push(file("PDF1", "application/pdf"));
    place(&mut p, "a", &files, t0);
    assert_eq!(p.next_ask(), Some(t0), "the window wakes for the asks a placement owes");
    let first = asked(&p.asks(t0));
    assert_eq!(first.len(), 1);
    assert_eq!(p.next_ask(), None, "with one in flight no wake is owed");
    assert!(first.iter().all(|(h, r)| h == "a" && r.target == target() && r.file != "PDF1"));
    // Asked again with one in flight: nothing.
    assert!(p.asks(t0).is_empty());
    // Another host's pictures have their own one, whatever the first host has in flight.
    place(&mut p, "b", &pngs(3), t0);
    let other = asked(&p.asks(t0));
    assert_eq!(other.len(), 1);
    assert!(other.iter().all(|(h, _)| h == "b"));
    // The first host placed again: each answer asks one more, and none is asked twice.
    place(&mut p, "a", &files, t0);
    let (h, req) = first[0].clone();
    let mut all = vec![req.file.clone()];
    let next = p.on_reply(&h, &req, picture(4, 4), t0);
    assert_eq!(asked(&next).len(), 1);
    all.extend(answer_all(&mut p, next, t0).into_iter().map(|(_, r)| r.file));
    assert_eq!(all.len(), 5, "{all:?}");
    assert_eq!(all.iter().collect::<HashSet<_>>().len(), 5);
    assert!(p.asks(t0 + Duration::from_secs(3600)).is_empty());
    assert!(p.held("a", "WS1", "CH1", "F004").is_some());
}

#[test]
fn a_thumbnail_past_its_bounds_is_never_decoded_whole() {
    // A side past 2048 is refused by the decoder's limits.
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
    // One parse, all of it under the limits: an 8 x 8 PNG whose ICC profile inflates far past 32 MiB costs no more than one
    // that inflates just past it, as the decoder stops at the bound for either. A first pass under the default limits (512
    // MiB) would inflate the large one whole, about ten times the work.
    let (small, large) = (iccp_bomb(40 << 20), iccp_bomb(400 << 20));
    let fastest = |bytes: &[u8]| {
        (0..3)
            .map(|_| {
                let at = Instant::now();
                assert!(decode(bytes).is_none_or(|p| p.dimensions() == (8, 8)));
                at.elapsed()
            })
            .min()
            .unwrap()
    };
    let (small_cost, large_cost) = (fastest(&small), fastest(&large));
    assert!(large_cost < small_cost * 3, "{large_cost:?} against {small_cost:?}: the profile was inflated past the bound");
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
    // The drawer places the records, and its next frame asks for one of their thumbnails.
    d.layout(&mut fonts, body(), 1.0, body());
    let first = thumbs(&d.tick(shown("ROW1"), at).0);
    assert_eq!(first, ["F000"]);
    assert!(thumbs(&d.tick(shown("ROW1"), at).0).is_empty());
    // Another host's link change leaves it in flight.
    d.on_connection(&host("b"), true, at);
    d.on_connection(&host("b"), false, at);
    assert!(thumbs(&d.tick(shown("ROW1"), at).0).is_empty());
    // The link goes: nothing is asked while it is down, and when it comes back the same one is asked again.
    d.on_connection(&host("a"), false, at);
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(thumbs(&d.tick(shown("ROW1"), at).0).is_empty());
    let outs = d.on_connection(&host("a"), true, at);
    let mut again = thumbs(&outs);
    d.layout(&mut fonts, body(), 1.0, body());
    again.extend(thumbs(&d.tick(shown("ROW1"), at).0));
    assert_eq!(again, first);
    // A reply goes to the pictures owner, and the next is asked in its place.
    let req = ChatFileReq { target: target(), file: first[0].clone(), variant: ChatFileVariant::Thumb };
    let next = d.on_evt(&host("a"), ChatEvt::File { req, reply: picture(4, 4) }, at);
    assert_eq!(thumbs(&next), ["F001"]);
}

#[test]
fn every_picture_type_is_a_format_the_decoder_reads() {
    for mime in PICTURE_MIMES {
        let format = ImageFormat::from_mime_type(mime).unwrap_or_else(|| panic!("{mime}"));
        assert_eq!(format.to_mime_type(), mime);
    }
}

#[test]
fn a_final_refusal_stays_final_past_64_and_the_refusals_have_their_own_bound() {
    let t0 = Instant::now();
    let mut p = connected(&["a"], t0);
    let refuse_all = |p: &mut ChatPictures| {
        let mut outs = p.asks(t0);
        let mut n = 0;
        while let Some((h, req)) = asked(&outs).into_iter().next() {
            n += 1;
            outs = p.on_reply(&h, &req, ChatReply::Refused(ChatCode::NotAnImage), t0);
        }
        n
    };
    // 74 pictures, each refused `not_an_image`, placed 64 and then 10 at a time.
    let files = pngs(MAX_HELD + 10);
    place(&mut p, "a", &files[..MAX_HELD], t0);
    assert_eq!(refuse_all(&mut p), MAX_HELD);
    place(&mut p, "a", &files[MAX_HELD..], t0);
    assert_eq!(refuse_all(&mut p), 10);
    // Placed again, the first ten (now the least recently placed) are not asked again.
    place(&mut p, "a", &files[..10], t0);
    assert!(p.asks(t0 + Duration::from_secs(3600)).is_empty());
    // Past `MAX_FINAL` refusals the oldest is forgotten, and asked again when placed.
    let more: Vec<ChatFile> = (0..MAX_FINAL).map(|i| file(&format!("G{i:04}"), "image/png")).collect();
    for batch in more.chunks(MAX_HELD) {
        place(&mut p, "a", batch, t0);
        assert_eq!(refuse_all(&mut p), batch.len());
    }
    place(&mut p, "a", &files[..1], t0);
    assert_eq!(asked(&p.asks(t0)).len(), 1, "the oldest refusal was forgotten");
    place(&mut p, "a", &more[MAX_FINAL - 1..], t0);
    assert!(p.asks(t0).is_empty(), "the newest refusal is kept");
}

#[test]
fn a_decoder_panic_is_a_final_refusal_and_never_the_windows_end() {
    let t0 = Instant::now();
    let mut p = connected(&["a"], t0);
    p.decode = |_| panic!("a decoder's panic, as a test seam");
    place(&mut p, "a", &pngs(2), t0);
    let (h, req) = asked(&p.asks(t0)).remove(0);
    let next = asked(&p.on_reply(&h, &req, picture(4, 4), t0));
    assert!(p.held("a", "WS1", "CH1", &req.file).is_none());
    // The owner goes on: the next file is asked, and the one that panicked never again.
    assert_eq!(next.len(), 1);
    assert_ne!(next[0].1.file, req.file);
    p.on_reply(&next[0].0, &next[0].1, ChatReply::Refused(ChatCode::NotAnImage), t0);
    place(&mut p, "a", &pngs(2), t0);
    assert!(p.asks(t0 + Duration::from_secs(3600)).is_empty());
}

#[test]
fn a_file_whose_retries_ran_out_is_asked_again_when_its_record_changes() {
    let t0 = Instant::now();
    let mut p = connected(&["a"], t0);
    // Slack had no thumbnail yet: `failed` four times, then refused.
    let early = ChatFile { thumbnail: false, ..pngs(1).remove(0) };
    place(&mut p, "a", std::slice::from_ref(&early), t0);
    let mut at = t0;
    for _ in 0..=3 {
        let (h, req) = asked(&p.asks(at)).remove(0);
        p.on_reply(&h, &req, ChatReply::Refused(ChatCode::Failed), at);
        at += Duration::from_secs(5);
        place(&mut p, "a", std::slice::from_ref(&early), at);
    }
    assert!(p.asks(at + Duration::from_secs(3600)).is_empty(), "the same summary is not asked again");
    // Its record's edit reaches the view with the thumbnail: asked again.
    let late = ChatFile { thumbnail: true, ..early.clone() };
    place(&mut p, "a", std::slice::from_ref(&late), at);
    let (h, req) = asked(&p.asks(at)).remove(0);
    p.on_reply(&h, &req, picture(4, 4), at);
    assert!(p.held("a", "WS1", "CH1", &late.id).is_some());
    // A refusal for good is not undone by a change.
    let (mut p, bad) = (connected(&["a"], t0), file("B1", "image/png"));
    place(&mut p, "a", std::slice::from_ref(&bad), t0);
    let (h, req) = asked(&p.asks(t0)).remove(0);
    p.on_reply(&h, &req, ChatReply::Refused(ChatCode::TooLarge), t0);
    place(&mut p, "a", &[ChatFile { bytes: Some(1), ..bad }], t0);
    assert!(p.asks(t0).is_empty());
}

#[test]
fn a_reply_for_a_picture_not_in_flight_changes_nothing() {
    let t0 = Instant::now();
    let mut p = connected(&["a"], t0);
    place(&mut p, "a", &pngs(2), t0);
    let (h, req) = asked(&p.asks(t0)).remove(0);
    let next = asked(&p.on_reply(&h, &req, picture(4, 4), t0)).remove(0);
    // A second reply to the held one, and a reply for the one still wanted elsewhere or never placed, are dropped.
    p.on_reply(&h, &req, ChatReply::Refused(ChatCode::NotAnImage), t0);
    assert!(p.held("a", "WS1", "CH1", &req.file).is_some());
    let never = ChatFileReq { file: "F999".into(), ..req.clone() };
    assert!(p.on_reply(&h, &never, picture(4, 4), t0).is_empty());
    assert!(p.held("a", "WS1", "CH1", "F999").is_none());
    // The one in flight is still taken.
    p.on_reply(&next.0, &next.1, picture(4, 4), t0);
    assert_eq!(p.held_count(), 2);
}

#[test]
fn only_the_last_placement_is_asked_so_a_hidden_drawer_asks_nothing() {
    let t0 = Instant::now();
    let mut p = connected(&["a"], t0);
    place(&mut p, "a", &pngs(3), t0);
    // Hidden (no host placed) before the frame's asks: nothing is asked and no wake is owed.
    p.place(None, std::iter::empty(), t0);
    assert!(p.asks(t0).is_empty());
    assert_eq!(p.next_ask(), None);
    // Another set placed: only it is asked, never the earlier one.
    let others: Vec<ChatFile> = (0..2).map(|i| file(&format!("H{i}"), "image/png")).collect();
    place(&mut p, "a", &others, t0);
    let outs = p.asks(t0);
    let all = answer_all(&mut p, outs, t0);
    assert!(all.iter().all(|(_, r)| r.file.starts_with('H')), "{all:?}");
    assert_eq!(all.len(), 2);
}
