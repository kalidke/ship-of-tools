//! Opening a chat picture in the preview (the plan's D10): a press on a tile or `chat.open_image` asks for its original, one
//! in flight, and only a checked original of the file still wanted is handed to State.

use std::io::Cursor;
use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use image::{ImageFormat, RgbaImage};
use sot_protocol::chat::{ChatFile, ChatRecord};
use sot_protocol::ops::{ChatCode, ChatFileReq, ChatFileVariant};

use super::drawer::ChatDrawer;
use super::owner::Out;
use super::thread_tests::{body, channel, shown};
use crate::net::transport::{ChatEvt, ChatPicture, ChatReply, OutgoingReq};

fn file(id: &str, mime: &str) -> ChatFile {
    serde_json::from_value(serde_json::json!({ "id": id, "name": "figure", "mime": mime, "bytes": 100, "thumbnail": true })).unwrap()
}

fn record(ts: &str, files: &[ChatFile]) -> ChatRecord {
    serde_json::from_value(serde_json::json!({ "workspace": "WS1", "channel": "CH1", "ts": ts, "thread_ts": ts,
        "author": { "id": "U1", "name": "member one" }, "origin": "member", "text": "a figure", "reply_count": 0, "files": files })).unwrap()
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    RgbaImage::from_pixel(w, h, image::Rgba([200, 80, 60, 255])).write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

/// The `chat.file` requests among `outs`, by variant and file.
fn files(outs: &[Out], variant: ChatFileVariant) -> Vec<ChatFileReq> {
    outs.iter()
        .filter_map(|(_, r)| match r {
            OutgoingReq::ChatFile { req } if req.variant == variant => Some(req.clone()),
            _ => None,
        })
        .collect()
}

fn originals(outs: &[Out]) -> Vec<String> {
    files(outs, ChatFileVariant::Original).into_iter().map(|r| r.file).collect()
}

fn answer(d: &mut ChatDrawer, req: ChatFileReq, bytes: Vec<u8>, at: Instant) -> Vec<Out> {
    d.on_evt(&"a".to_string(), ChatEvt::File { req, reply: ChatReply::Done(ChatPicture { mime: "image/png".into(), bytes }) }, at)
}

/// A drawer showing one record of two pictures after a PDF, laid out; the instant of its read.
fn figures() -> (ChatDrawer, Instant, FontSystem) {
    let mut fonts = FontSystem::new();
    let (mut d, at) = channel(&[record("1700000001.000100", &[file("P1", "application/pdf"), file("F1", "image/png"), file("F2", "image/png")])]);
    d.layout(&mut fonts, body(), 1.0, body());
    (d, at, fonts)
}

/// Answers every thumbnail asked among `outs`, and those asked after, with a small picture; the originals asked meanwhile.
fn settle(d: &mut ChatDrawer, mut outs: Vec<Out>, at: Instant) -> Vec<String> {
    let mut asked = Vec::new();
    loop {
        asked.extend(originals(&outs));
        let Some(thumb) = files(&outs, ChatFileVariant::Thumb).into_iter().next() else { return asked };
        outs = answer(d, thumb, png(4, 4), at);
    }
}

/// A left press and its release at `at`, as the window's mouse routes them.
fn click(d: &mut ChatDrawer, at: (f32, f32), now: Instant) -> Vec<Out> {
    d.press_tile(at);
    d.release_tile(at, now)
}

fn center(r: crate::ui::render::quad::ScreenRect) -> (f32, f32) {
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

#[test]
fn a_press_on_a_tile_opens_that_file_and_selects_nothing() {
    let (mut d, at, _) = figures();
    let tiles = d.tiles();
    assert_eq!(tiles.iter().map(|t| t.file.as_str()).collect::<Vec<_>>(), ["F1", "F2"]);
    // A press on the second tile: the drawer's press selects nothing (no line is under it), and it opens that file once.
    let on = center(tiles[1].rect);
    let hit = d.hit(on);
    assert!(hit.is_some(), "a press on a tile is a press in the body");
    d.press(hit);
    d.extend_drag((on.0 + 40.0, on.1));
    assert_eq!(d.copy_text(), None, "a press on a tile selected text");
    assert_eq!(originals(&click(&mut d, on, at)), ["F2"]);
    // Pressed again while it is in flight, nothing more goes; a press beside the tiles opens nothing.
    assert!(originals(&click(&mut d, on, at)).is_empty());
    let beside = (tiles[1].rect.x + tiles[1].rect.w + 2.0, on.1);
    assert!(click(&mut d, beside, at).is_empty());
}

#[test]
fn a_drag_that_starts_on_a_tile_opens_nothing() {
    let (mut d, at, _) = figures();
    let on = center(d.tiles()[0].rect);
    // Pressed on the tile and released 40 px away, or on another tile: nothing is asked. A release with no press opens nothing.
    d.press_tile(on);
    assert!(d.release_tile((on.0 + 40.0, on.1), at).is_empty());
    d.press_tile(on);
    assert!(d.release_tile(center(d.tiles()[1].rect), at).is_empty());
    assert!(d.release_tile(on, at).is_empty());
    // Released where it was pressed, it opens, and the status line says it is coming.
    d.press_tile(on);
    assert_eq!(originals(&d.release_tile((on.0 + 2.0, on.1 - 2.0), at)), ["F1"]);
    assert_eq!(d.take_notice().as_deref(), Some("opening the picture…"));
}

#[test]
fn an_open_on_a_link_that_is_down_or_an_old_hub_says_so_and_is_never_sent_later() {
    let (mut d, at, _) = figures();
    let on = center(d.tiles()[0].rect);
    d.on_connection(&"a".to_string(), false, false, at);
    assert!(click(&mut d, on, at).is_empty());
    assert_eq!(d.take_notice().as_deref(), Some("picture not opened: the connection to the computer that runs your agents is down"));
    // The link comes back: the press made while it was down is not sent.
    let mut outs = d.on_connection(&"a".to_string(), true, false, at);
    outs.extend(d.tick(shown("ROW1"), at).0);
    assert!(settle(&mut d, outs, at).is_empty(), "a press made while the link was down was sent later");
    // An open asked and then cut by a link change is given up and said, and not sent again when the link returns.
    let req = files(&click(&mut d, on, at), ChatFileVariant::Original).remove(0);
    d.take_notice();
    d.on_connection(&"a".to_string(), false, false, at);
    assert_eq!(d.take_notice().as_deref(), Some("picture not opened: the connection to the computer that runs your agents changed before it arrived"));
    let mut outs = d.on_connection(&"a".to_string(), true, false, at);
    outs.extend(d.tick(shown("ROW1"), at).0);
    assert!(settle(&mut d, outs, at).is_empty());
    answer(&mut d, req, png(4, 4), at);
    assert!(d.take_opened().is_none(), "a reply across a link change was installed");
    // A hub that does not know the op: said at once, and the next press says it again without asking.
    let req = files(&click(&mut d, on, at), ChatFileVariant::Original).remove(0);
    d.on_evt(&"a".to_string(), ChatEvt::File { req, reply: ChatReply::Unsupported }, at);
    assert_eq!(d.take_notice().as_deref(), Some("picture not opened: update Ship of Tools on the computer that runs your agents to open it"));
    assert!(click(&mut d, on, at).is_empty());
    assert_eq!(d.take_notice().as_deref(), Some("picture not opened: update Ship of Tools on the computer that runs your agents to open it"));
}

#[test]
fn the_key_opens_the_marked_messages_first_image() {
    let (mut d, at, mut fonts) = figures();
    assert_eq!(d.open_marked(at).err(), Some("no message marked: ArrowUp marks one"));
    d.mark(true, Instant::now(), Duration::ZERO);
    d.layout(&mut fonts, body(), 1.0, body());
    // The first of its pictures, never the PDF before it.
    assert_eq!(originals(&d.open_marked(at).unwrap()), ["F1"]);
    // A marked message with no picture opens nothing.
    let (mut plain, at) = channel(&[record("1700000001.000100", &[file("P1", "application/pdf")])]);
    plain.layout(&mut fonts, body(), 1.0, body());
    plain.mark(true, Instant::now(), Duration::ZERO);
    plain.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(plain.open_marked(at).err(), Some("the marked message has no picture"));
}

#[test]
fn one_open_is_in_flight_and_a_late_reply_is_dropped() {
    let (mut d, at, _) = figures();
    let tiles = d.tiles();
    // A thumbnail in flight holds the host's one transfer: the open waits for its reply, then goes.
    let thumb = files(&d.tick(shown("ROW1"), at).0, ChatFileVariant::Thumb).remove(0);
    assert!(originals(&click(&mut d, center(tiles[0].rect), at)).is_empty());
    let outs = answer(&mut d, thumb, png(4, 4), at);
    assert!(files(&outs, ChatFileVariant::Thumb).is_empty(), "a thumbnail went beside the original in flight");
    let first = files(&outs, ChatFileVariant::Original).remove(0);
    assert_eq!(first.file, "F1");
    // A newer open replaces the wanted file but waits for the one in flight, whose reply is then dropped.
    assert!(click(&mut d, center(tiles[1].rect), at).is_empty(), "a second original went while one was in flight");
    let next = files(&answer(&mut d, first.clone(), png(8, 8), at), ChatFileVariant::Original);
    assert!(d.take_opened().is_none(), "a reply for a file no longer wanted was installed");
    assert_eq!(next.iter().map(|r| r.file.as_str()).collect::<Vec<_>>(), ["F2"]);
    // A second reply for the dropped one changes nothing; the wanted one's is handed over once, for its row.
    answer(&mut d, first, png(8, 8), at);
    assert!(d.take_opened().is_none());
    let thumb = files(&answer(&mut d, next[0].clone(), png(6, 5), at), ChatFileVariant::Thumb).remove(0);
    let (_, row, bytes) = d.take_opened().expect("the wanted picture");
    assert_eq!((row.host.as_str(), row.workspace_id.as_str(), bytes), ("a", "ROW1", png(6, 5)), "the original as it came, undecoded");
    assert!(d.take_opened().is_none());
    // A row change gives the wanted open up: its reply installs nothing.
    answer(&mut d, thumb, png(4, 4), at);
    assert_eq!(originals(&click(&mut d, center(tiles[0].rect), at)), ["F1"]);
    d.tick(shown("ROW2"), at);
    let req = ChatFileReq { file: "F1".into(), ..next[0].clone() };
    answer(&mut d, req, png(6, 5), at);
    assert!(d.take_opened().is_none(), "a picture opened for another row was installed");
}

#[test]
fn a_refused_original_is_said_and_nothing_is_handed_on() {
    let (mut d, at, _) = figures();
    let tiles = d.tiles();
    let req = files(&click(&mut d, center(tiles[0].rect), at), ChatFileVariant::Original).remove(0);
    d.on_evt(&"a".to_string(), ChatEvt::File { req, reply: ChatReply::Refused(ChatCode::TooLarge) }, at);
    assert!(d.take_opened().is_none());
    assert_eq!(d.take_notice(), Some(format!("picture not opened: {}", ChatCode::TooLarge.text())));
}
