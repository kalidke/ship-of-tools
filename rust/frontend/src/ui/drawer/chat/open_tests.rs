//! Opening a chat picture in the preview (the plan's D10): a press on a tile or `chat.open_image` asks for its original, one
//! in flight, and only a checked original of the file still wanted is handed to State.

use std::io::Cursor;
use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use image::{ImageFormat, RgbaImage};
use sot_protocol::chat::{ChatFile, ChatRecord};
use sot_protocol::ops::{ChatCode, ChatFileReq, ChatFileVariant};

use super::compose::{compose_rows, Edit};
use super::drawer::{ChatDrawer, Previewed};
use super::owner::Out;
use super::row_channel_tests::answer as resolved;
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
    assert_eq!(d.take_notice().as_deref(), Some("picture not opened: the connection to a is down"));
    // The link comes back: the press made while it was down is not sent.
    let mut outs = d.on_connection(&"a".to_string(), true, false, at);
    outs.extend(d.tick(shown("ROW1"), at).0);
    assert!(settle(&mut d, outs, at).is_empty(), "a press made while the link was down was sent later");
    // An open asked and then cut by a link change is given up and said, and not sent again when the link returns.
    let req = files(&click(&mut d, on, at), ChatFileVariant::Original).remove(0);
    d.take_notice();
    d.on_connection(&"a".to_string(), false, false, at);
    assert_eq!(d.take_notice().as_deref(), Some("picture not opened: the connection to a changed before it arrived"));
    let mut outs = d.on_connection(&"a".to_string(), true, false, at);
    outs.extend(d.tick(shown("ROW1"), at).0);
    assert!(settle(&mut d, outs, at).is_empty());
    answer(&mut d, req, png(4, 4), at);
    assert!(d.take_opened().is_none(), "a reply across a link change was installed");
    // A hub that does not know the op: said at once, and the next press says it again without asking.
    let req = files(&click(&mut d, on, at), ChatFileVariant::Original).remove(0);
    d.on_evt(&"a".to_string(), ChatEvt::File { req, reply: ChatReply::Unsupported }, at);
    assert_eq!(d.take_notice().as_deref(), Some("picture not opened: update Ship of Tools on a to open it"));
    assert!(click(&mut d, on, at).is_empty());
    assert_eq!(d.take_notice().as_deref(), Some("picture not opened: update Ship of Tools on a to open it"));
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

/// A row switched back to is drawn from what was held before its resolve is answered, and until then the drawer only shows:
/// every action it can send (the pace, Enter, a file's post, a thread, a picture's open by a click and by the mark, the picker)
/// sends nothing for it, whatever the draft or the marked message was. The answer lifts it.
#[test]
fn a_row_drawn_before_its_resolve_is_answered_sends_nothing_for_its_channel() {
    let mut fonts = FontSystem::new();
    let (mut d, at, _) = figures();
    let row = shown("ROW1");
    d.edit(row.as_ref(), Edit::Text("a draft")).unwrap();
    let later = |ms: u64| at + Duration::from_millis(ms);
    d.tick(shown("ROW2"), later(1000));
    d.tick(shown("ROW1"), later(2000));
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(d.held(), 1, "the first frame draws the held record");
    assert_eq!(d.tiles().len(), 2, "and its picture tiles");
    d.mark(true, Instant::now(), Duration::ZERO);
    d.layout(&mut fonts, body(), 1.0, body());
    let on = center(d.tiles()[0].rect);
    let now = later(2010);
    let actions: Vec<(&str, Vec<Out>)> = vec![
        ("the frame's pace", d.tick(shown("ROW1"), now).0),
        ("Enter on the draft", d.send(row.as_ref(), Some("Ada"), now).unwrap_or_default()),
        ("the post of a file", d.send_file(row.as_ref(), Previewed::File("files:a.png"), now).unwrap_or_default()),
        ("the thread of the marked message", d.open_thread()),
        ("the open of the marked picture", d.open_marked(now).unwrap_or_default()),
        ("a click on a tile", click(&mut d, on, now)),
        ("the channel picker", d.open_picker(row.as_ref()).unwrap_or_default()),
        ("a later frame", d.tick(shown("ROW1"), later(2100)).0),
    ];
    for (name, outs) in &actions {
        assert!(outs.is_empty(), "{name} sent {} requests for a channel not yet confirmed", outs.len());
    }
    assert!(d.send(row.as_ref(), Some("Ada"), now).is_err() && d.send_file(row.as_ref(), Previewed::File("files:a.png"), now).is_err());
    assert_eq!(d.draft().map(|(t, _)| t), Some("a draft"), "the draft stands");
    // The resolve is the one request, and its answer lifts the hold: the same Enter now posts.
    let outs = d.tick(shown("ROW1"), later(2160)).0;
    assert!(matches!(outs.as_slice(), [(_, OutgoingReq::ChatResolve { .. })]));
    d.on_evt(&"a".to_string(), resolved("ROW1", serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH1", "delivers": false })), later(2170));
    let posted = d.send(row.as_ref(), Some("Ada"), later(2180)).unwrap();
    assert!(matches!(posted.as_slice(), [(_, OutgoingReq::ChatPost { .. })]));
}

/// A key handled in the frame of a row change acts on a row the drawer has not followed yet: a draft of that row, begun in the
/// channel it had then, is posted for no channel the window has confirmed.
#[test]
fn a_post_key_for_a_row_the_frame_has_not_followed_posts_nothing() {
    let (mut d, at, _) = figures();
    let later = |ms: u64| at + Duration::from_millis(ms);
    let host = "a".to_string();
    // ROW2 is shown, mapped to another channel, and a draft begun in it; then ROW1 is shown again and answered.
    d.tick(shown("ROW2"), later(1000));
    d.tick(shown("ROW2"), later(1150));
    d.on_evt(&host, resolved("ROW2", serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH2", "delivers": false })), later(1160));
    d.edit(shown("ROW2").as_ref(), Edit::Text("for two")).unwrap();
    d.tick(shown("ROW1"), later(2000));
    d.tick(shown("ROW1"), later(2150));
    d.on_evt(&host, resolved("ROW1", serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH1", "delivers": false })), later(2160));
    assert_eq!(d.draft(), None, "ROW1 has no draft");
    // The key's row is ROW2, which the last frame did not follow: nothing goes, and the draft stays.
    assert!(d.send(shown("ROW2").as_ref(), Some("Ada"), later(2210)).is_err());
    assert!(d.send_file(shown("ROW2").as_ref(), Previewed::File("files:a.png"), later(2210)).is_err());
}

/// Typing at a row's first frame begins its draft in the channel drawn, and the foot is drawn with the messages; only the
/// send waits. An answer that names another channel leaves the draft where it was begun, as a row paired again with a draft
/// held does.
#[test]
fn typing_at_the_first_frame_begins_the_draft_in_the_channel_drawn() {
    let (mut d, at, _) = figures();
    let later = |ms: u64| at + Duration::from_millis(ms);
    let row = shown("ROW1");
    let off = chrono::FixedOffset::east_opt(0).unwrap();
    d.tick(shown("ROW2"), later(1000));
    d.tick(shown("ROW1"), later(2000));
    assert_eq!(d.held(), 1);
    assert!(d.compose_text_at(false, 0, off).is_some(), "the input rows are drawn with the messages");
    assert_eq!(d.edit(row.as_ref(), Edit::Text("hi")), Ok(()), "a key is not lost nor told there is no channel");
    assert_eq!(d.draft(), Some(("hi", "CH1")));
    assert!(d.send(row.as_ref(), Some("Ada"), later(2010)).is_err(), "the send waits");
    d.tick(shown("ROW1"), later(2150));
    d.on_evt(&"a".to_string(), resolved("ROW1", serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH9", "delivers": false })), later(2160));
    assert_eq!(d.draft(), Some(("hi", "CH1")), "the draft stays where it was begun");
    let foot = compose_rows(&d.compose_text_at(false, 0, off).unwrap(), 60, 6).join("|");
    assert!(foot.contains("to CH1"), "and the foot says where it goes: {foot}");
}

/// A message typed at the first frame and sent after an answer that names the same channel goes to the row's own target: its
/// row, workspace and channel, as the watch's are.
#[test]
fn a_draft_typed_at_the_first_frame_posts_to_the_rows_target_once_the_same_channel_answers() {
    let (mut d, at, _) = figures();
    let later = |ms: u64| at + Duration::from_millis(ms);
    let row = shown("ROW1");
    d.tick(shown("ROW2"), later(1000));
    d.tick(shown("ROW1"), later(2000));
    assert_eq!(d.edit(row.as_ref(), Edit::Text("hi")), Ok(()));
    d.tick(shown("ROW1"), later(2150));
    d.on_evt(&"a".to_string(), resolved("ROW1", serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH1", "delivers": false })), later(2160));
    let out = d.send(row.as_ref(), Some("Ada"), later(2170)).unwrap();
    match out.as_slice() {
        [(_, OutgoingReq::ChatPost { req })] => {
            assert_eq!((req.target.row.as_str(), req.target.workspace.as_str(), req.target.channel.as_str()), ("ROW1", "WS1", "CH1"));
        }
        other => panic!("one post expected, got {}", other.len()),
    }
}

/// A key for a row the frame does not follow never begins a draft in the channel drawn for the row it does.
#[test]
fn a_key_for_another_row_never_begins_a_draft_in_the_drawn_channel() {
    let (mut d, at, _) = figures();
    let later = |ms: u64| at + Duration::from_millis(ms);
    d.tick(shown("ROW2"), later(1000));
    d.tick(shown("ROW1"), later(2000));
    assert_eq!(d.held(), 1, "ROW1 is drawn from memory, unconfirmed");
    assert!(d.edit(shown("ROW3").as_ref(), Edit::Text("x")).is_err(), "ROW3 is not the row the frame follows");
    assert_eq!(d.draft(), None);
}
