use super::{ReplImagePainter, ReplImageUpload, ReplPresentation};
use crate::ui::*;
use base64::Engine as _;
use std::{cell::RefCell, io::Cursor, rc::Rc};

type Owner = ReplPresentation<Handle>;

struct Handle {
    id: usize,
    retired: Rc<RefCell<Vec<usize>>>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.retired.borrow_mut().push(self.id);
    }
}

#[derive(Default)]
struct Upload {
    calls: Vec<(u32, u32, [u8; 4])>,
    retired_at_upload: Vec<Vec<usize>>,
    fail_next: bool,
    retired: Rc<RefCell<Vec<usize>>>,
}

impl ReplImageUpload<Handle> for Upload {
    fn upload(&mut self, rgba: &[u8], w: u32, h: u32) -> Result<Handle> {
        assert_eq!(rgba.len(), w as usize * h as usize * 4);
        self.retired_at_upload.push(self.retired.borrow().clone());
        self.calls.push((w, h, rgba[..4].try_into().unwrap()));
        if std::mem::take(&mut self.fail_next) {
            anyhow::bail!("upload failed");
        }
        Ok(Handle { id: self.calls.len(), retired: self.retired.clone() })
    }
}

#[derive(Debug, PartialEq)]
enum Paint {
    Scissor(u32, u32, u32, u32),
    Image(usize, (f32, f32, f32, f32), (u32, u32)),
}

#[derive(Default)]
struct Painter {
    events: Vec<Paint>,
    fail: bool,
}

impl ReplImagePainter<Handle> for Painter {
    fn scissor(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.events.push(Paint::Scissor(x, y, w, h));
    }
    fn image(&mut self, handle: &Handle, r: ScreenRect, surface: (u32, u32)) -> Result<()> {
        self.events.push(Paint::Image(handle.id, (r.x, r.y, r.w, r.h), surface));
        if self.fail { anyhow::bail!("paint failed"); }
        Ok(())
    }
}

fn row(host: &str, slug: &str) -> WsKey { (host.into(), slug.into()) }

fn target(row: &WsKey) -> ReplTarget {
    ReplTarget { host: row.0.clone(), workspace_id: Some(row.1.clone()), view_key: row.clone() }
}

fn frame(model: &mut ReplModel, row: &WsKey, id: u64, frame: ReplFrame) {
    model.apply_frame(row.0.clone(), id, row, row.clone(), frame);
}

fn start(model: &mut ReplModel, row: &WsKey, id: u64) {
    frame(model, row, id, ReplFrame::Started {
        run_id: format!("run-{id}"), origin: "session".into(), display: format!("eval {id}"),
    });
}

fn output(model: &mut ReplModel, row: &WsKey, id: u64, text: &str) {
    frame(model, row, id, ReplFrame::Stdout { text: text.into() });
}

fn finish(model: &mut ReplModel, row: &WsKey, id: u64) {
    frame(model, row, id, ReplFrame::Done { eval_id: id, elapsed_ms: 0 });
}

fn image(w: u32, h: u32, red: u8) -> ReplFrame {
    let pixels = image::RgbaImage::from_pixel(w, h, image::Rgba([red, 0, 0, 255]));
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels).write_to(&mut encoded, image::ImageFormat::Png).unwrap();
    let bytes = encoded.into_inner();
    ReplFrame::Image {
        mime: "image/png".into(), data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        bytes: bytes.len() as u64,
    }
}

fn rect(w: f32, h: f32) -> ScreenRect { ScreenRect { x: 20.0, y: 30.0, w, h } }

fn build(owner: &mut Owner, model: &ReplModel, row: &WsKey, scroll: u16) -> (Vec<RtLine<'static>>, u16) {
    owner.build(row, model.entries(), false, 8.0, 16.0, scroll)
}

fn paint(owner: &Owner) -> Vec<Paint> {
    let mut painter = Painter::default();
    owner.paint(true, 8.0, 16.0, (1000, 800), &mut painter).unwrap();
    painter.events
}

fn pictures(events: &[Paint]) -> Vec<(usize, f32, f32, f32, f32)> {
    events.iter().filter_map(|event| match event {
        Paint::Image(id, (x, y, w, h), _) => Some((*id, *x, *y, *w, *h)),
        Paint::Scissor(..) => None,
    }).collect()
}

fn text(lines: &[RtLine<'static>]) -> Vec<String> {
    lines.iter().map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
}

#[test]
fn images_follow_the_row_eval_and_frame_that_produced_them() {
    let (a, b, c) = (row("host-a", "row"), row("host-b", "row"), row("host-a", "other"));
    let (mut model, mut owner, mut upload) = (ReplModel::default(), Owner::default(), Upload::default());
    owner.commit_layout(rect(400.0, 400.0), (0, 100));
    start(&mut model, &a, 7);
    frame(&mut model, &a, 7, image(32, 16, 1));
    frame(&mut model, &a, 7, image(16, 32, 2));
    owner.refresh_images(&a, model.entries(), &mut upload);
    build(&mut owner, &model, &a, 0);
    assert_eq!(pictures(&paint(&owner)), [(1, 28.0, 46.0, 32.0, 16.0), (2, 28.0, 62.0, 16.0, 32.0)]);
    finish(&mut model, &a, 7);
    model.capture_row(a.clone());
    for (key, red, id) in [(&b, 3, 3), (&c, 4, 4)] {
        let (_, effects) = model.restore_row(key);
        owner.apply_effects(&effects);
        start(&mut model, key, 7);
        frame(&mut model, key, 7, image(24, 48, red));
        owner.refresh_images(key, model.entries(), &mut upload);
        build(&mut owner, &model, key, 0);
        assert_eq!(pictures(&paint(&owner)), [(id, 28.0, 46.0, 24.0, 48.0)]);
        finish(&mut model, key, 7);
        model.capture_row(key.clone());
    }
    let (found, effects) = model.restore_row(&a);
    assert!(found);
    owner.apply_effects(&effects);
    owner.refresh_images(&a, model.entries(), &mut upload);
    build(&mut owner, &model, &a, 0);
    assert_eq!(pictures(&paint(&owner)), [(5, 28.0, 46.0, 32.0, 16.0), (6, 28.0, 62.0, 16.0, 32.0)]);
    assert_eq!(upload.calls, [(32, 16, [1, 0, 0, 255]), (16, 32, [2, 0, 0, 255]),
        (24, 48, [3, 0, 0, 255]), (24, 48, [4, 0, 0, 255]),
        (32, 16, [1, 0, 0, 255]), (16, 32, [2, 0, 0, 255])]);
}

#[test]
fn image_cache_reuses_successes_and_retries_failures() {
    let key = row("host", "row");
    let (mut model, mut owner, mut upload) = (ReplModel::default(), Owner::default(), Upload::default());
    start(&mut model, &key, 1);
    frame(&mut model, &key, 1, image(2, 3, 1));
    owner.refresh_images(&key, model.entries(), &mut upload);
    owner.refresh_images(&key, model.entries(), &mut upload);
    assert_eq!(upload.calls.len(), 1);
    for data in ["%%%".to_string(), base64::engine::general_purpose::STANDARD.encode(b"not an image")] {
        frame(&mut model, &key, 1, ReplFrame::Image { mime: "image/png".into(), data_base64: data, bytes: 3 });
    }
    frame(&mut model, &key, 1, image(4, 5, 2));
    upload.fail_next = true;
    owner.refresh_images(&key, model.entries(), &mut upload);
    assert_eq!(upload.calls.len(), 2);
    owner.refresh_images(&key, model.entries(), &mut upload);
    owner.refresh_images(&key, model.entries(), &mut upload);
    assert_eq!(upload.calls.len(), 3, "failed upload retries, successful uploads stay cached");
    // The failed decodes never reached the upload port or displaced the successful images.
    assert_eq!(upload.calls[2], (4, 5, [2, 0, 0, 255]));
    for id in 2..=257 { start(&mut model, &key, id); }
    owner.refresh_images(&key, model.entries(), &mut upload);
    let mut retired = upload.retired.borrow().clone();
    retired.sort_unstable();
    assert_eq!(retired, [1, 3], "normal model trimming retires both cached frame indices");
    // Returning to a parked log causes fresh uploads: no parked texture cache is retained.
    let parked = row("host", "parked");
    start(&mut model, &key, 258);
    frame(&mut model, &key, 258, image(2, 3, 3));
    owner.refresh_images(&key, model.entries(), &mut upload);
    model.capture_row(key.clone());
    let (_, effects) = model.restore_row(&parked);
    owner.apply_effects(&effects);
    owner.refresh_images(&parked, model.entries(), &mut upload);
    assert!(upload.retired.borrow().contains(&4));
    let (_, effects) = model.restore_row(&key);
    owner.apply_effects(&effects);
    owner.refresh_images(&key, model.entries(), &mut upload);
    assert_eq!(upload.calls.len(), 5);
}

#[test]
fn image_cache_retains_frames_of_a_surviving_eval_after_restore() {
    let key = row("host", "row");
    let (mut model, mut owner, mut upload) = (ReplModel::default(), Owner::default(), Upload::default());
    owner.commit_layout(rect(400.0, 400.0), (0, 100));
    start(&mut model, &key, 7);
    model.capture_row(key.clone()); // The surviving eval has no image in this shorter snapshot.
    frame(&mut model, &key, 7, image(32, 16, 1));
    owner.refresh_images(&key, model.entries(), &mut upload);
    build(&mut owner, &model, &key, 0);
    assert_eq!(pictures(&paint(&owner)), [(1, 28.0, 46.0, 32.0, 16.0)]);

    let (found, effects) = model.restore_row(&key);
    assert!(found);
    owner.apply_effects(&effects);
    assert_eq!(model.entries().len(), 1);
    assert!(model.entries()[0].frames.is_empty());
    owner.refresh_images(&key, model.entries(), &mut upload);
    assert!(upload.retired.borrow().is_empty(), "surviving eval retains its absent frame index");
    build(&mut owner, &model, &key, 0);
    assert!(paint(&owner).is_empty(), "retention does not preserve obsolete image slots");

    frame(&mut model, &key, 7, image(32, 16, 1));
    owner.refresh_images(&key, model.entries(), &mut upload);
    build(&mut owner, &model, &key, 0);
    assert_eq!(upload.calls.len(), 1, "the returning frame reuses the original upload");
    assert_eq!(pictures(&paint(&owner)), [(1, 28.0, 46.0, 32.0, 16.0)]);
}

#[test]
fn image_cache_uploads_before_retiring_old_evals() {
    let key = row("host", "row");
    let (mut model, mut owner, mut upload) = (ReplModel::default(), Owner::default(), Upload::default());
    start(&mut model, &key, 7);
    frame(&mut model, &key, 7, image(32, 16, 1));
    owner.refresh_images(&key, model.entries(), &mut upload);

    let plan = model.begin_file_run(target(&key), "next.jl".into(), true);
    let id = plan.submission.eval_id();
    let effects = model.complete_submission(plan.submission, Ok(()));
    owner.apply_effects(&effects);
    frame(&mut model, &key, id, image(16, 32, 2));
    owner.refresh_images(&key, model.entries(), &mut upload);

    assert_eq!(upload.calls, [(32, 16, [1, 0, 0, 255]), (16, 32, [2, 0, 0, 255])]);
    assert_eq!(upload.retired_at_upload, [Vec::<usize>::new(), Vec::new()],
        "the old eval's texture is still alive when the new image uploads");
    assert_eq!(upload.retired.borrow().as_slice(), &[1], "the old texture retires after upload");
}

#[test]
fn image_slots_are_rebuilt_and_fit_both_dimensions() {
    let key = row("host", "row");
    let (mut model, mut owner, mut upload) = (ReplModel::default(), Owner::default(), Upload::default());
    start(&mut model, &key, 1);
    output(&mut model, &key, 1, "before\n");
    frame(&mut model, &key, 1, image(200, 100, 1));
    output(&mut model, &key, 1, "after\n");
    owner.refresh_images(&key, model.entries(), &mut upload);
    for (w, h, dw, dh, rows) in [(400.0, 400.0, 200.0, 100.0, 7),
        (96.0, 400.0, 80.0, 40.0, 3), (400.0, 80.0, 64.0, 32.0, 2)] {
        owner.commit_layout(rect(w, h), (0, 100));
        let (lines, _) = build(&mut owner, &model, &key, 0);
        assert_eq!(pictures(&paint(&owner)), [(1, 28.0, 62.0, dw, dh)]);
        let displayed = text(&lines);
        assert_eq!(displayed.len(), rows + 4);
        assert!(displayed[2..2 + rows].iter().all(String::is_empty));
        assert_eq!(displayed[2 + rows], "after");
    }
    owner.commit_layout(rect(32.0, 80.0), (0, 100));
    assert!(text(&build(&mut owner, &model, &key, 0).0)[2].starts_with("[image · image/png"));
    assert!(paint(&owner).is_empty(), "degenerate width rebuild replaces the slots");
    // A build can see image data before refresh; first-frame zero geometry uses a caption too.
    let mut first = Owner::default();
    first.refresh_images(&key, model.entries(), &mut upload);
    assert!(text(&build(&mut first, &model, &key, 0).0)[2].starts_with("[image ·"));
    owner.commit_layout(rect(400.0, 400.0), (0, 100));
    build(&mut owner, &model, &key, 0);
    assert!(!paint(&owner).is_empty());
    let empty = row("host", "empty");
    let (_, effects) = model.restore_row(&empty);
    owner.apply_effects(&effects); // Anchor-only reset leaves the old slots for build to replace.
    build(&mut owner, &model, &empty, 0);
    assert!(paint(&owner).is_empty());
}

#[test]
fn pixel_geometry_survives_row_restore_until_the_next_layout() {
    let (a, b) = (row("host", "a"), row("host", "b"));
    let (mut model, mut owner, mut upload) = (ReplModel::default(), Owner::default(), Upload::default());
    owner.commit_layout(rect(96.0, 400.0), (0, 100));
    start(&mut model, &a, 1);
    finish(&mut model, &a, 1);
    model.capture_row(a);
    let (_, effects) = model.restore_row(&b);
    owner.apply_effects(&effects);
    start(&mut model, &b, 1);
    frame(&mut model, &b, 1, image(200, 100, 1));
    owner.refresh_images(&b, model.entries(), &mut upload);
    assert_eq!(build(&mut owner, &model, &b, 0).0.len(), 5);
    assert_eq!(pictures(&paint(&owner)), [(1, 28.0, 46.0, 80.0, 40.0)]);
    owner.commit_layout(rect(400.0, 80.0), (0, 100));
    assert_eq!(build(&mut owner, &model, &b, 0).0.len(), 4);
    assert_eq!(pictures(&paint(&owner)), [(1, 28.0, 46.0, 64.0, 32.0)]);
}

#[test]
fn drawer_layout_returns_the_clamped_half_open_window() {
    use ratatui::layout::Rect;
    let mut model = ReplModel::default();
    // A build that asked for far more than the log holds: the layout clamps it, and the commit is given both.
    let built = 999;
    let layout = Owner::drawer_layout(Rect::new(3, 4, 20, 10), 3, 20, built);
    assert_eq!(layout.scrollback, Rect::new(3, 4, 20, 7));
    assert_eq!(layout.input, Rect::new(3, 11, 20, 3));
    assert_eq!((layout.scroll, layout.window), (13, (0, 7)));
    model.commit_rendered_scroll(built, layout.scroll);
    assert_eq!(model.scroll(), 13);
    let layout = Owner::drawer_layout(Rect::new(0, 0, 20, 10), 2, 20, 4);
    assert_eq!((layout.scroll, layout.window), (4, (8, 16)));
    for (height, input, back) in [(0, 0, 0), (1, 1, 0), (2, 1, 1), (4, 3, 1)] {
        let layout = Owner::drawer_layout(Rect::new(0, 0, 20, height), 100, 0, 999);
        assert_eq!((layout.input.height, layout.scrollback.height), (input, back));
        assert_eq!((layout.scroll, layout.window), (0, (0, 0)));
    }
    let layout = Owner::drawer_layout(Rect::new(0, 0, 20, 10), 3, 65_548, 999);
    assert_eq!((layout.scroll, layout.window), (5, (65_536, 65_543)), "keep the existing u16 conversion");
    for width in [0, 1] {
        let layout = Owner::drawer_layout(Rect::new(0, 0, width, 2), 100, 3, 0);
        assert_eq!(layout.input.width, width);
        assert_eq!(layout.scrollback.width, width);
        assert_eq!(layout.window.1, 3);
        assert!(layout.window.0 <= layout.window.1);
    }
}

#[test]
fn the_build_anchor_preserves_the_view_as_the_tail_changes() {
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    for id in 1..=256 { start(&mut model, &key, id); finish(&mut model, &key, id); }
    let (lines, mut scroll) = build(&mut owner, &model, &key, 12);
    let reading = text(&lines)[lines.len() - scroll as usize - 1].clone();
    output(&mut model, &key, 256, "tail-a\ntail-b\n");
    let (lines, next) = build(&mut owner, &model, &key, scroll);
    assert_eq!(next, 14);
    assert_eq!(text(&lines)[lines.len() - next as usize - 1], reading);
    scroll = next;
    start(&mut model, &key, 257); // Front trimmed while tail grew: total delta is one, tail delta two.
    let (lines, next) = build(&mut owner, &model, &key, scroll);
    assert_eq!(next, 16);
    assert_eq!(text(&lines)[lines.len() - next as usize - 1], reading);
    finish(&mut model, &key, 257);
    assert_eq!(build(&mut owner, &model, &key, next).1, 15);
    let effects = model.clear_scrollback();
    owner.apply_effects(&effects);
    start(&mut model, &key, 999);
    assert_eq!(build(&mut owner, &model, &key, 15).1, 15, "vanished anchor reseeds");
    output(&mut model, &key, 999, &"row\n".repeat(65_536));
    assert_eq!(build(&mut owner, &model, &key, u16::MAX - 1).1, u16::MAX);
    output(&mut model, &key, 999, "live\n");
    assert_eq!(build(&mut owner, &model, &key, 0).1, 0);

    // A completed local id can later be announced externally with the same numeric id.
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    model.edit_draft(DraftEdit::Replace("local".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    model.apply_eval_completion(key.0.clone(), id, &key, 0, vec![ReplFrame::Stdout { text: "older\n".into() }]);
    start(&mut model, &key, 9);
    start(&mut model, &key, id);
    build(&mut owner, &model, &key, 7);
    start(&mut model, &key, 10);
    assert_eq!(build(&mut owner, &model, &key, 7).1, 9, "last matching numeric id anchors the tail");
}

#[test]
fn geometry_changes_reseed_the_anchor() {
    let key = row("host", "row");
    for (w, h, cw, ch) in [(200.0, 400.0, 8.0, 16.0), (400.0, 200.0, 8.0, 16.0),
        (400.0, 400.0, 40.0, 16.0), (400.0, 400.0, 8.0, 20.0)] {
        let (mut model, mut owner, mut upload) = (ReplModel::default(), Owner::default(), Upload::default());
        start(&mut model, &key, 1);
        frame(&mut model, &key, 1, image(400, 400, 1));
        owner.refresh_images(&key, model.entries(), &mut upload);
        owner.commit_layout(rect(400.0, 400.0), (0, 100));
        build(&mut owner, &model, &key, 10);
        owner.commit_layout(rect(w, h), (0, 100));
        assert_eq!(owner.build(&key, model.entries(), false, cw, ch, 10).1, 10);
        output(&mut model, &key, 1, "new\n");
        assert_eq!(owner.build(&key, model.entries(), false, cw, ch, 10).1, 11);
    }
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    start(&mut model, &key, 1);
    owner.commit_layout(rect(400.0, 400.0), (0, 100));
    build(&mut owner, &model, &key, 10);
    output(&mut model, &key, 1, "new\n");
    owner.commit_layout(ScreenRect { x: 200.0, y: 300.0, ..rect(400.0, 400.0) }, (0, 100));
    assert_eq!(build(&mut owner, &model, &key, 10).1, 11, "position is excluded from the key");
    finish(&mut model, &key, 1);
    let effects = model.clear_scrollback();
    owner.apply_effects(&effects);
    assert!(build(&mut owner, &model, &key, 10).0.is_empty());
    start(&mut model, &key, 1);
    assert_eq!(build(&mut owner, &model, &key, 10).1, 10, "empty build removes its anchor");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    start(&mut model, &key, 1);
    owner.commit_layout(rect(400.0, 0.0), (0, 100));
    build(&mut owner, &model, &key, 7);
    output(&mut model, &key, 1, "new\n");
    owner.commit_layout(rect(400.0, -0.0), (0, 100));
    assert_eq!(build(&mut owner, &model, &key, 7).1, 7, "raw geometry bits distinguish signed zero");
}

#[test]
fn clear_fresh_run_and_restore_keep_their_presentation_rules() {
    let key = row("host", "row");
    let (mut model, mut owner, mut upload) = (ReplModel::default(), Owner::default(), Upload::default());
    start(&mut model, &key, 9);
    frame(&mut model, &key, 9, image(32, 32, 1));
    owner.commit_layout(rect(200.0, 200.0), (0, 100));
    owner.refresh_images(&key, model.entries(), &mut upload);
    build(&mut owner, &model, &key, 10);
    model.capture_row(key.clone());
    let saved = model.entries().to_vec();
    let effects = model.clear_scrollback();
    owner.apply_effects(&effects);
    assert!(paint(&owner).is_empty());
    // Reproject the immutable pre-clear record: the clear effect kept geometry and anchor.
    assert_eq!(owner.build(&key, &saved, false, 8.0, 16.0, 10).1, 9);
    owner.refresh_images(&key, &saved, &mut upload);
    assert_eq!(owner.build(&key, &saved, false, 8.0, 16.0, 9).1, 10);
    assert_eq!(pictures(&paint(&owner)), [(2, 28.0, 46.0, 32.0, 32.0)]);
    build(&mut owner, &model, &key, 0); // Actual empty log clears the anchor on build.
    let (found, effects) = model.restore_row(&key);
    assert!(found);
    owner.apply_effects(&effects);
    build(&mut owner, &model, &key, 10);
    output(&mut model, &key, 9, "one\ntwo\n");
    assert_eq!(build(&mut owner, &model, &key, 10).1, 12);
    let (_, effects) = model.restore_row(&key);
    owner.apply_effects(&effects);
    assert_eq!(build(&mut owner, &model, &key, 10).1, 10, "found restore resets the anchor");
    owner.refresh_images(&key, model.entries(), &mut upload);
    assert_eq!(upload.calls.len(), 2, "restore retained the cached texture");

    let plan = model.begin_file_run(target(&key), "demo.jl".into(), true);
    let fresh_id = plan.submission.eval_id();
    let effects = model.complete_submission(plan.submission, Ok(()));
    owner.apply_effects(&effects);
    assert_eq!(pictures(&paint(&owner))[0].0, 2, "fresh run leaves images and slots until normal upkeep");
    owner.refresh_images(&key, model.entries(), &mut upload);
    assert!(upload.retired.borrow().contains(&2));
    frame(&mut model, &key, fresh_id, image(32, 32, 2));
    owner.refresh_images(&key, model.entries(), &mut upload);
    build(&mut owner, &model, &key, 10);
    finish(&mut model, &key, fresh_id);
    let missing = row("host", "first-visit");
    let (found, effects) = model.restore_row(&missing);
    assert!(!found);
    owner.apply_effects(&effects);
    start(&mut model, &missing, fresh_id);
    assert_eq!(build(&mut owner, &model, &missing, 10).1, 10, "first visit also resets the anchor");
}

#[test]
fn painting_uses_the_visible_window_and_restores_the_scissor() {
    let key = row("host", "row");
    let (mut model, mut owner, mut upload) = (ReplModel::default(), Owner::default(), Upload::default());
    start(&mut model, &key, 1);
    frame(&mut model, &key, 1, image(32, 48, 1)); // slot [1,4)
    frame(&mut model, &key, 1, image(16, 16, 2)); // slot [4,5)
    owner.refresh_images(&key, model.entries(), &mut upload);
    owner.commit_layout(rect(200.0, 200.0), (0, 100));
    build(&mut owner, &model, &key, 0);
    owner.commit_layout(rect(200.0, 32.0), (2, 4));
    assert_eq!(paint(&owner), [Paint::Scissor(20, 30, 200, 32),
        Paint::Image(1, (28.0, 14.0, 32.0, 48.0), (1000, 800)), Paint::Scissor(0, 0, 1000, 800)]);
    owner.commit_layout(rect(200.0, 32.0), (4, 5));
    assert_eq!(pictures(&paint(&owner)), [(2, 28.0, 30.0, 16.0, 16.0)]);
    owner.commit_layout(rect(200.0, 200.0), (0, 6));
    assert_eq!(pictures(&paint(&owner)), [(1, 28.0, 46.0, 32.0, 48.0), (2, 28.0, 94.0, 16.0, 16.0)]);
    let mut painter = Painter::default();
    owner.paint(false, 8.0, 16.0, (1000, 800), &mut painter).unwrap();
    assert!(painter.events.is_empty());
    for (w, h) in [(0.0, 20.0), (20.0, 0.0)] {
        owner.commit_layout(rect(w, h), (0, 6));
        assert!(paint(&owner).is_empty());
    }
    owner.commit_layout(ScreenRect { x: -10.0, y: 790.0, ..rect(200.0, 200.0) }, (0, 6));
    // The area runs from -10 to 190: the scissor is the part of it on the surface, not its width from the surface's edge.
    assert_eq!(paint(&owner)[0], Paint::Scissor(0, 790, 190, 10));
    let mut painter = Painter { fail: true, ..Default::default() };
    assert_eq!(owner.paint(true, 8.0, 16.0, (1000, 800), &mut painter).unwrap_err().to_string(), "paint failed");
    assert_eq!(painter.events.len(), 2, "failure propagates immediately, before success-only scissor restoration");
}

#[test]
fn repl_text_keeps_cell_rows_before_shaping() {
    use crate::ui::render::text::{set_buffer_rich_text, set_buffer_text};
    use glyphon::{Attrs, Buffer, FontSystem, Metrics, Shaping};
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    model.edit_draft(DraftEdit::Replace("codeA\rcodeB\r\ncodeC\tcodeD\u{1b}[31m\u{2029}codeE".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    output(&mut model, &key, id, "abc\rxy\r\nstdoutA\tstdoutB\u{1b}[31m\u{2029}stdoutC");
    frame(&mut model, &key, id, ReplFrame::Stderr { text: "stderrA\r\nstderrB".into() });
    frame(&mut model, &key, id, ReplFrame::Value { mime: "mimeA\nmimeB".into(), text: "valueA\u{2029}valueB".into() });
    frame(&mut model, &key, id, ReplFrame::Error {
        message: "errorA\r\nerrorB".into(),
        stacktrace: vec![serde_json::from_value(serde_json::json!({
            "fn": "fnA\nfnB", "file": "fileA\tfileB", "line": 7
        })).unwrap()],
    });
    frame(&mut model, &key, id, ReplFrame::Image { mime: "imageA\nimageB".into(), data_base64: String::new(), bytes: 0 });
    frame(&mut model, &key, 99, ReplFrame::Started {
        run_id: "external".into(), origin: "originA\roriginB".into(), display: "displayA\tdisplayB".into(),
    });
    let lines = build(&mut owner, &model, &key, 0).0;
    let rows = text(&lines);
    for wanted in ["julia> codeA ⏎ codeB", "       codeC codeD ⏎ codeE", "xyc", "stdoutA stdoutB", "stdoutC",
        "stderrA", "stderrB", "[mimeA ⏎ mimeB] valueA", "valueB", "ERROR: errorA", "errorB",
        "    at fnA ⏎ fnB (fileA fileB:7)", "[image · imageA ⏎ imageB · 0 bytes]",
        "⟨originA ⏎ originB ▸ displayA displayB⟩"] {
        assert!(rows.iter().any(|row| row == wanted), "missing {wanted:?}: {rows:?}");
    }
    let mut fonts = FontSystem::new();
    for line in lines {
        for span in &line.spans {
            assert!(!span.content.contains(|c: char| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')));
        }
        let plain_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let mut plain = Buffer::new(&mut fonts, Metrics::new(14.0, 18.0));
        set_buffer_text(&mut plain, &mut fonts, &plain_text, Attrs::new(), Shaping::Advanced);
        let mut rich = Buffer::new(&mut fonts, Metrics::new(14.0, 18.0));
        set_buffer_rich_text(&mut rich, &mut fonts, line.spans.iter().map(|s| (s.content.as_ref(), Attrs::new())), Attrs::new(), Shaping::Advanced);
        for buffer in [&mut plain, &mut rich] {
            buffer.shape_until_scroll(&mut fonts, false);
            assert_eq!(buffer.lines.len(), 1);
            assert_eq!(buffer.lines[0].text(), plain_text);
        }
    }
}

/// The first line whose text is `wanted`, as its spans' (text, foreground, bold) triples.
fn styled(lines: &[RtLine<'static>], wanted: &str) -> Vec<(String, Option<Color>, bool)> {
    let line = lines.iter().find(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>() == wanted)
        .unwrap_or_else(|| panic!("no line {wanted:?} in {:?}", text(lines)));
    line.spans.iter()
        .map(|s| (s.content.to_string(), s.style.fg, s.style.add_modifier.contains(ratatui::style::Modifier::BOLD)))
        .collect()
}

#[test]
fn each_prompt_mode_has_julias_color_and_weight() {
    // Julia 1.13: REPL.jl's `LineEditREPL` gives `julia>` :green and `pkg>` :blue, and LineEdit.jl `write_prompt` prints
    // the prompt text bold and the typed text in the terminal's own color, not bold.
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    model.edit_draft(DraftEdit::Replace("1 + 1".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    model.complete_submission(plan.submission, Ok(()));
    model.edit_draft(DraftEdit::Typed("]"));
    model.edit_draft(DraftEdit::Replace("status".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    model.complete_submission(plan.submission, Ok(()));
    let lines = build(&mut owner, &model, &key, 0).0;
    assert_eq!(styled(&lines, "julia> 1 + 1"),
        [("julia> ".into(), Some(Color::Green), true), ("1 + 1".into(), None, false)]);
    assert_eq!(styled(&lines, "pkg> status"),
        [("pkg> ".into(), Some(Color::Blue), true), ("status".into(), None, false)]);
}

#[test]
fn an_errors_parts_have_julias_colors() {
    // Julia 1.13: `display_error` prints `ERROR: ` bold :light_red (client.jl `default_color_error`) and the message
    // in the terminal's own color; a stack frame's function is bold and its `@`, location and punctuation :light_black
    // (ANSI 8), neither dim.
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    model.edit_draft(DraftEdit::Replace("f(1)".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    frame(&mut model, &key, id, ReplFrame::Error {
        message: "boom\nsecond".into(),
        stacktrace: ["f", "top-level scope", ""].iter().map(|name| serde_json::from_value(
            serde_json::json!({"fn": name, "file": "a.jl", "line": 3})).unwrap()).collect(),
    });
    let lines = build(&mut owner, &model, &key, 0).0;
    assert_eq!(styled(&lines, "ERROR: boom"),
        [("ERROR: ".into(), Some(Color::LightRed), true), ("boom".into(), None, false)]);
    assert_eq!(styled(&lines, "second"), [("second".into(), None, false)]);
    assert_eq!(styled(&lines, "    at f (a.jl:3)"), [
        ("    at ".into(), Some(Color::DarkGray), false), ("f".into(), None, true),
        (" (".into(), Some(Color::DarkGray), false), ("a.jl:3".into(), Some(Color::DarkGray), false),
        (")".into(), Some(Color::DarkGray), false),
    ]);
    // Julia prints `top-level scope` and an unnamed frame plain and every other function name bold
    // (`Base/stacktraces.jl` `show_spec_linfo`).
    let plain = ("top-level scope".to_string(), None, false);
    assert_eq!(styled(&lines, "    at top-level scope (a.jl:3)")[1], plain);
    assert_eq!(styled(&lines, "    at  (a.jl:3)")[1], (String::new(), None, false));
    assert!(lines.iter().flat_map(|l| &l.spans).filter(|s| s.content.contains("a.jl") || s.content == "    at ")
        .all(|s| !s.style.add_modifier.contains(ratatui::style::Modifier::DIM)), "a frame is not dim");
}

#[test]
fn output_and_values_are_in_the_terminals_own_color() {
    // Julia prints stdout, stderr and a shown value (`default_color_answer` is :normal) in the terminal's own color; only
    // a program that colors its own text, as the logging macros do, has another.
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    output(&mut model, &key, id, "out");
    frame(&mut model, &key, id, ReplFrame::Stderr { text: "err".into() });
    frame(&mut model, &key, id, ReplFrame::Value { mime: "text/plain".into(), text: "42".into() });
    let lines = build(&mut owner, &model, &key, 0).0;
    for wanted in ["out", "42"] {
        assert_eq!(styled(&lines, wanted), [(wanted.into(), None, false)]);
    }
    // The one deliberate departure: stderr keeps a quiet gray (ANSI 7), told from stdout, an error and a warning.
    assert_eq!(styled(&lines, "err"), [("err".into(), Some(Color::Gray), false)]);
}

#[test]
fn the_windows_link_back_note_is_told_from_program_output() {
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    finish(&mut model, &key, id);
    model.reopen_runs(&"host".into(), &key, vec![id]);
    frame(&mut model, &key, id, ReplFrame::Stderr { text: "from the program".into() });
    let lines = build(&mut owner, &model, &key, 0).0;
    let note = "the connection to the daemon is back, and this eval is still running";
    assert_eq!(styled(&lines, note), [(note.into(), Some(Color::LightMagenta), false)]);
    assert_eq!(styled(&lines, "from the program"), [("from the program".into(), Some(Color::Gray), false)]);
}

#[test]
fn the_error_label_is_inline_at_every_pane_width() {
    // Julia prints `ERROR: ` and the message on one row. The pane clips what is too long and never wraps, so the label
    // costs a long first line 7 cells; row counts must not depend on the pane's width.
    let key = row("host", "row");
    for width in [0.0, 100.0, 400.0, 2000.0] {
        let (mut model, mut owner) = (ReplModel::default(), Owner::default());
        owner.commit_layout(rect(width, 400.0), (0, 100));
        model.edit_draft(DraftEdit::Replace("x".into()));
        let plan = model.begin_eval(target(&key)).unwrap();
        let id = plan.submission.eval_id();
        model.complete_submission(plan.submission, Ok(()));
        for len in [5, 12, 13, 14, 45, 60] {
            frame(&mut model, &key, id, ReplFrame::Error { message: "m".repeat(len), stacktrace: vec![] });
        }
        let rows = text(&build(&mut owner, &model, &key, 0).0);
        let expected: Vec<String> = [5, 12, 13, 14, 45, 60].iter().map(|n| format!("ERROR: {}", "m".repeat(*n))).collect();
        assert_eq!(rows[1..7], expected[..], "pane {width} px wide");
    }
}

#[test]
fn the_live_prompt_has_julias_color_and_weight_focused_and_not() {
    let spans = |input: &str, pkg, focused| -> Vec<(String, Option<Color>, bool)> {
        let lines = super::input_lines(input, pkg, focused);
        lines.iter().flat_map(|l| l.spans.iter())
            .map(|s| (s.content.to_string(), s.style.fg, s.style.add_modifier.contains(ratatui::style::Modifier::BOLD)))
            .collect()
    };
    assert_eq!(spans("1+1", false, true), [
        ("julia> ".into(), Some(Color::Green), true), ("1+1".into(), None, false),
        ("\u{2588}".into(), Some(Color::Green), false)]);
    assert_eq!(spans("", true, true), [
        ("pkg> ".into(), Some(Color::Blue), true), (String::new(), None, false),
        ("\u{2588}".into(), Some(Color::Blue), false)]);
    assert_eq!(spans("a\nb", false, false), [
        ("julia> ".into(), Some(Color::DarkGray), true), ("a".into(), None, false),
        ("       ".into(), None, false), ("b".into(), None, false)]);
}

/// The text of `source` without its test modules and comment lines and, when `from` is given, from the first line
/// holding it.
fn production_text(source: &str, from: Option<&str>) -> String {
    let source = sot_platform::test_scan::without_test_modules(source);
    match from {
        Some(marker) => source[source.find(marker).unwrap()..].to_string(),
        None => source,
    }
}

#[test]
fn a_color_at_a_use_site_is_not_in_the_drawers_table() {
    // The REPL drawer's colors are named once, in `colors.rs`; a literal at a use site (the scrollback, the live prompt
    // and its cursor) is a second owner.
    let scrollback = production_text(include_str!("presentation.rs"), None);
    let live = production_text(include_str!("../../chrome/view.rs"), Some("fn repl_drawer_body"));
    let live = &live[..live.find("fn render_drawer_widgets").unwrap()];
    for (name, source) in [("presentation.rs", scrollback.as_str()), ("view.rs repl_drawer_body", live)] {
        for needle in ["Color::", "Modifier::"] {
            let hits: Vec<&str> = source.lines().filter(|l| l.contains(needle) && !l.trim_start().starts_with("//")).collect();
            assert!(hits.is_empty(), "{name} names {needle} at its use site: {hits:?}");
        }
    }
}

#[test]
fn an_error_with_no_message_still_says_error() {
    // Julia's `display_error` prints `ERROR: ` and the message after it, however short.
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    frame(&mut model, &key, id, ReplFrame::Error { message: String::new(), stacktrace: vec![] });
    let lines = build(&mut owner, &model, &key, 0).0;
    assert_eq!(styled(&lines, "ERROR: "), [("ERROR: ".into(), Some(Color::LightRed), true), (String::new(), None, false)]);
}

/// One finished eval whose stdout holds `count` lines, `line NN` and then `pad` x's, in a pane `cols` cells wide.
fn long_log(cols: usize, count: usize, pad: usize) -> (ReplModel, Owner, WsKey) {
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    let out: String = (0..count).map(|i| format!("line {i:02} {}\n", "x".repeat(pad))).collect();
    output(&mut model, &key, id, &out);
    finish(&mut model, &key, id);
    owner.set_cols(cols);
    (model, owner, key)
}

/// The number of the `line NN` that row `at` belongs to: its own or the nearest above it that starts one.
fn line_of(rows: &[String], at: usize) -> usize {
    let start = (0..=at).rev().find(|&i| rows[i].starts_with("line ")).unwrap();
    rows[start][5..7].parse().unwrap()
}

#[test]
fn a_long_line_shows_whole_over_several_rows() {
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    owner.set_cols(20);
    model.edit_draft(DraftEdit::Replace("let long_name = 1 + 2 + 3 + 4 + 5".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    output(&mut model, &key, id, &format!("{}\n", "o".repeat(50)));
    frame(&mut model, &key, id, ReplFrame::Stderr { text: "e".repeat(45) });
    frame(&mut model, &key, id, ReplFrame::Value { mime: "text/plain".into(), text: "v".repeat(30) });
    frame(&mut model, &key, id, ReplFrame::Error { message: "m".repeat(50), stacktrace: vec![] });
    finish(&mut model, &key, id);
    let rows = text(&build(&mut owner, &model, &key, 0).0);
    assert!(rows.iter().all(|r| r.chars().count() <= 20), "{rows:?}");
    // Typed code: the continuation rows sit under the code, not under the prompt.
    assert_eq!(rows[..3], ["julia> let long_name", "        = 1 + 2 + 3 ", "       + 4 + 5"]);
    let joined = |c: char| rows.iter().filter(|r| r.starts_with(c)).cloned().collect::<String>();
    assert_eq!(joined('o'), "o".repeat(50));
    assert_eq!(rows.iter().filter(|r| r.starts_with('e')).cloned().collect::<String>(), "e".repeat(45));
    assert_eq!(joined('v'), "v".repeat(30));
    let at = rows.iter().position(|r| r.starts_with("ERROR: ")).unwrap();
    assert_eq!(rows[at..at + 3].concat().replacen("ERROR: ", "", 1), "m".repeat(50));
}

#[test]
fn a_resize_rewraps_and_a_scrolled_up_reader_stays_on_the_same_text() {
    let (model, mut owner, key) = long_log(20, 30, 40);
    let scroll = 31;
    let rows = text(&build(&mut owner, &model, &key, scroll).0);
    let bottom = rows.len() - 1 - scroll as usize;
    let before = line_of(&rows, bottom);
    for cols in [35, 12, 50, 7] {
        owner.set_cols(cols);
        let (lines, now) = build(&mut owner, &model, &key, scroll);
        let rows = text(&lines);
        assert_eq!(line_of(&rows, rows.len() - 1 - now as usize), before, "at {cols} columns the bottom row is line {before}");
        let _ = build(&mut owner, &model, &key, now);
    }
}

#[test]
fn the_width_arriving_does_not_move_a_scrolled_up_reader() {
    // The first frame has no width and is drawn unwrapped; the width then arrives.
    let (model, mut owner, key) = long_log(0, 30, 40);
    let rows = text(&build(&mut owner, &model, &key, 0).0);
    let scroll = 12;
    let before = line_of(&rows, rows.len() - 1 - scroll as usize);
    assert_eq!(text(&build(&mut owner, &model, &key, scroll).0).len(), rows.len(), "unwrapped without a width");
    assert!(owner.set_cols(20));
    assert!(!owner.set_cols(0), "a width of 0 keeps the one known");
    let (lines, now) = build(&mut owner, &model, &key, scroll);
    let rows = text(&lines);
    assert!(rows.len() > 60);
    assert_eq!(line_of(&rows, rows.len() - 1 - now as usize), before);
}
