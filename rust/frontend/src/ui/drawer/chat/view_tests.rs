use super::project::{project, Kind};
use super::view::*;
use crate::ui::render::quad::ScreenRect;
use cosmic_text::FontSystem;
use sot_protocol::chat::ChatRecord;

fn rec(n: usize) -> ChatRecord {
    serde_json::from_value(serde_json::json!({ "workspace": "T1", "channel": "C1", "ts": format!("1700000{n:03}.000100"),
        "thread_ts": format!("1700000{n:03}.000100"), "author": { "id": "U1", "name": "Ada" }, "origin": "member",
        "text": format!("message {n}"), "reply_count": 0, "files": [] })).unwrap()
}

fn tops(p: &[Placed]) -> Vec<(usize, f32)> {
    p.iter().map(|p| (p.index, p.top)).collect()
}

#[test]
fn the_view_shows_the_held_records_newest_at_the_bottom_and_shapes_only_what_it_shows() {
    // Three records of known heights end at the body's bottom, in order.
    let heights = [30.0, 40.0, 50.0];
    let three = place(3, 200.0, None, |i| heights[i]);
    assert_eq!(tops(&three), [(0, 80.0), (1, 110.0), (2, 150.0)]);
    assert_eq!(three.last().map(|p| p.top + p.height), Some(200.0));
    // An arrival takes the bottom and lifts the rest.
    let heights = [30.0, 40.0, 50.0, 20.0];
    let four = place(4, 200.0, None, |i| heights[i]);
    assert_eq!(tops(&four), [(0, 60.0), (1, 90.0), (2, 130.0), (3, 180.0)]);
    // From a position, records run down from the top and stop at the body's bottom.
    assert_eq!(tops(&place(4, 100.0, Some(1), |i| heights[i])), [(1, 0.0), (2, 40.0), (3, 90.0)]);
    // With 256 records, only those reaching the body are measured.
    let mut asked = Vec::new();
    let many = place(256, 100.0, None, |i| {
        asked.push(i);
        30.0
    });
    assert_eq!(asked, [255, 254, 253, 252]);
    assert_eq!(tops(&many), [(252, -20.0), (253, 10.0), (254, 40.0), (255, 70.0)]);

    // Through real buffers: only the records in view are shaped, and a scroll keeps up to 64 others.
    let mut fonts = FontSystem::new();
    let held: Vec<ChatRecord> = (0..256).map(rec).collect();
    let records: Vec<&ChatRecord> = held.iter().collect();
    let body = ScreenRect { x: 10.0, y: 20.0, w: 400.0, h: 120.0 };
    let offset = chrono::FixedOffset::east_opt(0).unwrap();
    let mut view = ChatView::default();
    view.layout(&mut fonts, &records, body, 1.0, 1_700_000_000, offset);
    let shown = view.areas().len();
    assert!((1..=8).contains(&shown), "{shown}");
    assert_eq!(view.cached(), shown);
    assert_eq!(view.placed_tops().last().map(|t| t.is_finite()), Some(true));
    view.from = Some(0);
    view.layout(&mut fonts, &records, body, 1.0, 1_700_000_000, offset);
    assert!(view.cached() <= view.areas().len() + 64);
    view.layout(&mut fonts, &records, ScreenRect { w: 200.0, ..body }, 1.0, 1_700_000_000, offset);
    assert!(view.cached() <= view.areas().len() + 64);
    view.hide();
    assert!(view.areas().is_empty());

    // Each kind has its own style; a body line that imitates a header keeps the body's.
    let kinds = [Kind::Header(false), Kind::Header(true), Kind::Origin, Kind::Body, Kind::Markup, Kind::Meta];
    for (i, a) in kinds.iter().enumerate() {
        for b in &kinds[i + 1..] {
            assert_ne!(attrs(*a), attrs(*b), "{a:?} {b:?}");
        }
    }
    let spans = project(&rec(1), 1_700_000_000, offset, false);
    assert_eq!(spans[0].kind, Kind::Header(false));
}
