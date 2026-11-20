use super::{colors, sgr, wrap::Wrap, ReplImagePainter, ReplImageUpload, ReplPresentation};
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
    owner.commit_layout(rect(400.0, 400.0), (0, 100), 0);
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
    owner.commit_layout(rect(400.0, 400.0), (0, 100), 0);
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
        owner.commit_layout(rect(w, h), (0, 100), 0);
        let (lines, _) = build(&mut owner, &model, &key, 0);
        assert_eq!(pictures(&paint(&owner)), [(1, 28.0, 62.0, dw, dh)]);
        let displayed = text(&lines);
        assert_eq!(displayed.len(), rows + 4);
        assert!(displayed[2..2 + rows].iter().all(String::is_empty));
        assert_eq!(displayed[2 + rows], "after");
    }
    owner.commit_layout(rect(32.0, 80.0), (0, 100), 0);
    assert!(text(&build(&mut owner, &model, &key, 0).0)[2].starts_with("[image · image/png"));
    assert!(paint(&owner).is_empty(), "degenerate width rebuild replaces the slots");
    // A build can see image data before refresh; first-frame zero geometry uses a caption too.
    let mut first = Owner::default();
    first.refresh_images(&key, model.entries(), &mut upload);
    assert!(text(&build(&mut first, &model, &key, 0).0)[2].starts_with("[image ·"));
    owner.commit_layout(rect(400.0, 400.0), (0, 100), 0);
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
    owner.commit_layout(rect(96.0, 400.0), (0, 100), 0);
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
    owner.commit_layout(rect(400.0, 80.0), (0, 100), 0);
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
        owner.commit_layout(rect(400.0, 400.0), (0, 100), 0);
        build(&mut owner, &model, &key, 10);
        owner.commit_layout(rect(w, h), (0, 100), 0);
        assert_eq!(owner.build(&key, model.entries(), false, cw, ch, 10).1, 10);
        output(&mut model, &key, 1, "new\n");
        assert_eq!(owner.build(&key, model.entries(), false, cw, ch, 10).1, 11);
    }
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    start(&mut model, &key, 1);
    owner.commit_layout(rect(400.0, 400.0), (0, 100), 0);
    build(&mut owner, &model, &key, 10);
    output(&mut model, &key, 1, "new\n");
    owner.commit_layout(ScreenRect { x: 200.0, y: 300.0, ..rect(400.0, 400.0) }, (0, 100), 0);
    assert_eq!(build(&mut owner, &model, &key, 10).1, 11, "position is excluded from the key");
    finish(&mut model, &key, 1);
    let effects = model.clear_scrollback();
    owner.apply_effects(&effects);
    assert!(build(&mut owner, &model, &key, 10).0.is_empty());
    start(&mut model, &key, 1);
    assert_eq!(build(&mut owner, &model, &key, 10).1, 10, "empty build removes its anchor");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    start(&mut model, &key, 1);
    owner.commit_layout(rect(400.0, 0.0), (0, 100), 0);
    build(&mut owner, &model, &key, 7);
    output(&mut model, &key, 1, "new\n");
    owner.commit_layout(rect(400.0, -0.0), (0, 100), 0);
    assert_eq!(build(&mut owner, &model, &key, 7).1, 7, "raw geometry bits distinguish signed zero");
}

#[test]
fn clear_fresh_run_and_restore_keep_their_presentation_rules() {
    let key = row("host", "row");
    let (mut model, mut owner, mut upload) = (ReplModel::default(), Owner::default(), Upload::default());
    start(&mut model, &key, 9);
    frame(&mut model, &key, 9, image(32, 32, 1));
    owner.commit_layout(rect(200.0, 200.0), (0, 100), 0);
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
    owner.commit_layout(rect(200.0, 200.0), (0, 100), 0);
    build(&mut owner, &model, &key, 0);
    owner.commit_layout(rect(200.0, 32.0), (2, 4), 0);
    assert_eq!(paint(&owner), [Paint::Scissor(20, 30, 200, 32),
        Paint::Image(1, (28.0, 14.0, 32.0, 48.0), (1000, 800)), Paint::Scissor(0, 0, 1000, 800)]);
    owner.commit_layout(rect(200.0, 32.0), (4, 5), 0);
    assert_eq!(pictures(&paint(&owner)), [(2, 28.0, 30.0, 16.0, 16.0)]);
    owner.commit_layout(rect(200.0, 200.0), (0, 6), 0);
    assert_eq!(pictures(&paint(&owner)), [(1, 28.0, 46.0, 32.0, 48.0), (2, 28.0, 94.0, 16.0, 16.0)]);
    let mut painter = Painter::default();
    owner.paint(false, 8.0, 16.0, (1000, 800), &mut painter).unwrap();
    assert!(painter.events.is_empty());
    for (w, h) in [(0.0, 20.0), (20.0, 0.0)] {
        owner.commit_layout(rect(w, h), (0, 6), 0);
        assert!(paint(&owner).is_empty());
    }
    owner.commit_layout(ScreenRect { x: -10.0, y: 790.0, ..rect(200.0, 200.0) }, (0, 6), 0);
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
        (" (a.jl:3)".into(), Some(Color::DarkGray), false),
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
        owner.commit_layout(rect(width, 400.0), (0, 100), 0);
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
        let lines = super::input_lines(input, pkg, focused, 0);
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

#[test]
fn a_rows_wrapped_frames_are_not_another_rows_with_the_same_ids() {
    // Two rows whose logs hold the same eval id and frame index, with text of the same length: each row draws its own.
    let (a, b) = (row("host", "a"), row("host", "b"));
    let mut owner = Owner::default();
    owner.set_cols(30);
    let log = |ch: char| vec![ReplEntry {
        eval_id: 7, code: "x".into(), frames: vec![ReplFrame::Stdout { text: ch.to_string().repeat(40) }],
        elapsed_ms: 1, in_flight: false, pkg_mode: false, origin: None, own_frames: Vec::new(),
    }];
    let rows_a = text(&owner.build(&a, &log('a'), false, 8.0, 16.0, 0).0);
    let rows_b = text(&owner.build(&b, &log('b'), false, 8.0, 16.0, 0).0);
    assert!(rows_a.iter().any(|r| r.starts_with('a')) && !rows_a.iter().any(|r| r.starts_with('b')), "{rows_a:?}");
    assert!(rows_b.iter().any(|r| r.starts_with('b')) && !rows_b.iter().any(|r| r.starts_with('a')), "{rows_b:?}");
}

#[test]
fn a_resize_keeps_the_reader_in_the_frame_they_were_reading() {
    // One eval whose output arrives as three frames of numbered lines: the reader is in the first, and a resize,
    // which finds the anchor's line among the frames, does not take them to the same line number in another frame.
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    owner.set_cols(20);
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    for f in 0..3 {
        let out: String = (0..20).map(|i| format!("f{f} line {i:02} {}\n", "x".repeat(20))).collect();
        output(&mut model, &key, id, &out);
    }
    finish(&mut model, &key, id);
    let scroll = 100;
    let rows = text(&build(&mut owner, &model, &key, scroll).0);
    // The logical line the bottom row belongs to: the nearest row at or above it that starts one.
    let bottom = |rows: &[String], s: u16| {
        let at = rows.len() - 1 - s as usize;
        rows[..=at].iter().rev().find(|r| r.starts_with('f')).unwrap()[..10].to_string()
    };
    let before = bottom(&rows, scroll);
    assert!(before.starts_with("f0"), "the reader is in the first frame: {before:?}");
    owner.set_cols(35);
    let (lines, now) = build(&mut owner, &model, &key, scroll);
    assert_eq!(bottom(&text(&lines), now), before, "the same line of the same frame");
}

#[test]
fn the_layout_commit_carries_the_width_the_next_build_wraps_for() {
    // `commit_layout` is the one call that gives the presentation its width: with it the next build wraps and the
    // caller is told to ask for that frame; the same width again asks for nothing.
    let (model, mut owner, key) = long_log(0, 3, 40);
    assert_eq!(text(&build(&mut owner, &model, &key, 0).0).len(), 4, "no width yet: one row a line, and the prompt");
    assert!(owner.commit_layout(rect(400.0, 400.0), (0, 5), 20));
    assert!(!owner.commit_layout(rect(400.0, 400.0), (0, 5), 20));
    assert!(text(&build(&mut owner, &model, &key, 0).0).len() > 4);
}

#[test]
fn a_row_switch_forgets_the_view_anchor_of_the_row_left() {
    let (model, mut owner, key) = long_log(20, 30, 40);
    build(&mut owner, &model, &key, 20);
    assert!(owner.view_anchor.is_some());
    owner.apply_effects(&super::super::ReplEffects { reset_build_anchor: true, ..Default::default() });
    assert!(owner.view_anchor.is_none());
}

/// Every row the drawer builds goes through the one cut: a log with one row of each kind, in a pane narrower than every
/// one of them, draws no row wider than the pane and loses no text. The stack frame's row is the one named exception.
#[test]
fn every_kind_of_row_is_cut_by_the_one_wrap_and_loses_nothing() {
    
    let entry = |code: &str, frames: Vec<ReplFrame>| ReplEntry {
        eval_id: 1, code: code.into(), frames, elapsed_ms: 0, in_flight: false, pkg_mode: false, origin: None,
        own_frames: Vec::new(),
    };
    let origin = ReplEntry { origin: Some("session ▸ run /home/someone/analysis.jl".into()), ..entry("", vec![]) };
    let elapsed = ReplEntry { elapsed_ms: 123_456, ..entry("1", vec![]) };
    let running = ReplEntry { in_flight: true, ..entry("1", vec![]) };
    let notice = ReplEntry {
        own_frames: vec![0], ..entry("1", vec![ReplFrame::Stderr { text: "the connection is back, and this eval is still running".into() }])
    };
    let browser = ReplFrame::Browser { url: "http://127.0.0.1:54321/page".into(), open: false, fe: None };
    let browser_text = format!("↗ interactive figure · {}", crate::browser_open::origin_of("http://127.0.0.1:54321/page"));
    let image = ReplFrame::Image { mime: "image/png".into(), data_base64: String::new(), bytes: 12345 };
    let kinds: Vec<(&str, ReplEntry, bool, String)> = vec![
        ("origin marker", origin, false, "⟨session ▸ run /home/someone/analysis.jl⟩".into()),
        ("typed code", entry("let a_long_name = 1 + 2", vec![]), false, "julia> let a_long_name = 1 + 2".into()),
        ("stdout", entry("1", vec![ReplFrame::Stdout { text: "some long standard output".into() }]), false, "some long standard output".into()),
        ("stderr", entry("1", vec![ReplFrame::Stderr { text: "some long error output".into() }]), false, "some long error output".into()),
        ("value", entry("1", vec![ReplFrame::Value { mime: "text/plain".into(), text: "a long shown value".into() }]), false, "a long shown value".into()),
        ("error message", entry("1", vec![ReplFrame::Error { message: "a long error message".into(), stacktrace: vec![] }]), false, "ERROR: a long error message".into()),
        ("window's own note", notice, false, "the connection is back, and this eval is still running".into()),
        ("browser link", entry("1", vec![browser]), false, browser_text),
        ("image caption", entry("1", vec![image]), false, "[image · image/png · 12345 bytes]".into()),
        ("starting note", running.clone(), true, "(julia starting — precompiling this workspace's environment; a first run can take minutes…)".into()),
        ("running note", running, false, "(running…)".into()),
        ("elapsed note", elapsed, false, "  (123456 ms)".into()),
    ];
    for (name, entry, starting, expected) in kinds {
        let mut owner = Owner::default();
        owner.set_cols(8);
        let rows = text(&owner.build(&row("host", "row"), &[entry], starting, 8.0, 16.0, 0).0);
        assert!(rows.iter().all(|r| Span::raw(r.clone()).width() <= 8), "{name}: a row is wider than the pane: {rows:?}");
        let blob: String = rows.concat().chars().filter(|c| *c != ' ').collect();
        let want: String = expected.chars().filter(|c| *c != ' ').collect();
        assert!(blob.contains(&want), "{name}: text lost: {rows:?}");
    }
}

#[test]
fn the_cursor_stays_in_a_pane_no_wider_than_the_prompt() {
    for cols in 1..=10 {
        let rows = super::input_lines("", false, true, cols);
        let last = rows.last().unwrap();
        assert!(last.spans.last().unwrap().content == "\u{2588}", "{cols}: the last row ends in the cursor");
        let before: usize = last.spans[..last.spans.len() - 1].iter().map(|s| s.width()).sum();
        assert!(before < cols.max(8), "{cols}: the cursor sits inside the pane (or the prompt's own row): {before}");
    }
    let rows = super::input_lines("", false, true, 6);
    assert_eq!(text(&rows), ["julia> ", "     █"], "a 6-cell pane: the cursor in its last cell");
}

/// The scrollback's rows for one eval's frames, in a pane `cols` cells wide.
fn frames_rows(cols: usize, frames: Vec<ReplFrame>) -> Vec<RtLine<'static>> {
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    owner.set_cols(cols);
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    for f in frames {
        frame(&mut model, &key, id, f);
    }
    finish(&mut model, &key, id);
    build(&mut owner, &model, &key, 0).0
}

#[test]
fn a_programs_colors_reach_the_cells_through_the_terminal_drawers_map() {
    let lines = frames_rows(80, vec![
        ReplFrame::Stdout { text: "plain \u{1b}[1;32mgreen\u{1b}[0m end\n".into() },
        ReplFrame::Stderr { text: "\u{1b}[33m\u{1b}[1mWarning: \u{1b}[22m\u{1b}[39mthe text".into() },
        ReplFrame::Value { mime: "text/plain".into(), text: "\u{1b}[34mblue\u{1b}[39m".into() },
        ReplFrame::Error { message: "bad \u{1b}[36mtype\u{1b}[39m".into(), stacktrace: vec![] },
    ]);
    assert_eq!(text(&lines)[1..5], ["plain green end", "Warning: the text", "blue", "ERROR: bad type"]);
    assert_eq!(styled(&lines, "plain green end"),
        [("plain ".into(), None, false), ("green".into(), Some(Color::Green), true), (" end".into(), None, false)]);
    // stderr's own gray is what a reset or a default color returns to.
    assert_eq!(styled(&lines, "Warning: the text"),
        [("Warning: ".into(), Some(Color::Yellow), true), ("the text".into(), Some(Color::Gray), false)]);
    assert_eq!(styled(&lines, "blue"), [("blue".into(), Some(Color::Blue), false)]);
    assert_eq!(styled(&lines, "ERROR: bad type")[1..], [("bad ".into(), None, false), ("type".into(), Some(Color::Cyan), false)]);
}

#[test]
fn a_color_carries_from_one_frame_of_a_stream_to_the_next_and_not_across_streams_or_evals() {
    let lines = frames_rows(80, vec![
        ReplFrame::Stdout { text: "\u{1b}[31mone\n".into() },
        ReplFrame::Stderr { text: "other\n".into() },
        ReplFrame::Stdout { text: "two\u{1b}[39m\n".into() },
        ReplFrame::Stdout { text: "three".into() },
    ]);
    assert_eq!(styled(&lines, "one")[0].1, Some(Color::Red));
    assert_eq!(styled(&lines, "other")[0].1, Some(Color::Gray));
    assert_eq!(styled(&lines, "two")[0].1, Some(Color::Red));
    assert_eq!(styled(&lines, "three")[0].1, None);
    // A color never closed ends with its eval: the next eval, the typed code and the window's own note are plain.
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    for (i, out) in ["\u{1b}[31mopen", "next"].into_iter().enumerate() {
        model.edit_draft(DraftEdit::Replace(format!("code{i}")));
        let plan = model.begin_eval(target(&key)).unwrap();
        let id = plan.submission.eval_id();
        model.complete_submission(plan.submission, Ok(()));
        output(&mut model, &key, id, out);
        finish(&mut model, &key, id);
    }
    let lines = build(&mut owner, &model, &key, 0).0;
    assert_eq!(styled(&lines, "open")[0].1, Some(Color::Red));
    assert_eq!(styled(&lines, "next"), [("next".into(), None, false)]);
    assert!(styled(&lines, "julia> code1").iter().all(|s| s.1 != Some(Color::Red)));
}

#[test]
fn the_windows_own_note_starts_from_no_color_and_leaves_none() {
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    frame(&mut model, &key, id, ReplFrame::Stderr { text: "\u{1b}[31mleft open".into() });
    let entries = model.entries().to_vec();
    let mut entry = entries[0].clone();
    entry.frames.push(ReplFrame::Stderr { text: "the connection is back".into() });
    entry.own_frames = vec![1];
    entry.frames.push(ReplFrame::Stderr { text: "after".into() });
    let lines = owner.build(&key, &[entry], false, 8.0, 16.0, 0).0;
    let note = styled(&lines, "the connection is back");
    assert_eq!(note[0].1, Some(Color::LightMagenta), "{note:?}");
    assert_eq!(styled(&lines, "after")[0].1, Some(Color::Red), "the program's color goes on past the note");
}

#[test]
fn a_cursor_move_or_a_link_in_a_programs_text_leaves_only_its_words() {
    let lines = frames_rows(80, vec![ReplFrame::Stdout { text: "a\u{1b}[2J\u{1b}[1Ab \u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\\n".into() }]);
    assert_eq!(text(&lines)[1], "ab link");
}

#[test]
fn a_stack_frames_row_wraps_under_its_at_and_its_styles_survive_the_cut() {
    let at = serde_json::from_value(serde_json::json!({"fn": "f", "file": format!("/{}.jl", "p".repeat(30)), "line": 3})).unwrap();
    let lines = frames_rows(20, vec![ReplFrame::Error { message: "m".into(), stacktrace: vec![at] }]);
    let rows = text(&lines);
    assert!(rows.iter().all(|r| r.chars().count() <= 20), "{rows:?}");
    let at_row = rows.iter().position(|r| r.starts_with("    at f")).unwrap();
    assert!(rows[at_row + 1].starts_with("    ") && !rows[at_row + 1].starts_with("     "), "{rows:?}");
    let whole: String = rows[at_row..at_row + 3].iter().enumerate()
        .map(|(i, r)| if i == 0 { r.clone() } else { r[4..].to_string() }).collect();
    assert_eq!(whole, format!("    at f (/{}.jl:3)", "p".repeat(30)));
    assert_eq!(styled(&lines, "    at f (/ppppppppp")[1], ("f".into(), None, true), "the function keeps its bold");
}

#[test]
fn a_resize_keeps_a_reader_in_a_stack_trace_on_the_frame_they_were_reading() {
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    owner.set_cols(20);
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    let stack: Vec<_> = (0..12).map(|i| serde_json::from_value(serde_json::json!({"fn": format!("fn{i:02}"), "file": format!("/{}.jl", "q".repeat(30)), "line": i})).unwrap()).collect();
    frame(&mut model, &key, id, ReplFrame::Error { message: "m".into(), stacktrace: stack });
    finish(&mut model, &key, id);
    let scroll = 8;
    let (lines, now) = build(&mut owner, &model, &key, scroll);
    let rows = text(&lines);
    let bottom = |rows: &[String], s: u16| rows[..=rows.len() - 1 - s as usize].iter().rev().find(|r| r.starts_with("    at fn")).unwrap()[..10].to_string();
    let before = bottom(&rows, now);
    owner.set_cols(60);
    let (lines, now) = build(&mut owner, &model, &key, scroll);
    assert_eq!(bottom(&text(&lines), now), before);
}

/// The six notes only the drawer writes keep their style through the cut, on every row they take.
#[test]
fn the_drawers_own_notes_keep_their_colors_after_the_cut() {
    let entry = |code: &str| ReplEntry {
        eval_id: 1, code: code.into(), frames: vec![], elapsed_ms: 0, in_flight: false, pkg_mode: false, origin: None,
        own_frames: Vec::new(),
    };
    let browser = ReplFrame::Browser { url: "http://127.0.0.1:54321/page".into(), open: false, fe: None };
    let image = ReplFrame::Image { mime: "image/png".into(), data_base64: String::new(), bytes: 12345 };
    let notes = [
        ("origin marker", ReplEntry { origin: Some("session ▸ run /home/someone/analysis.jl".into()), ..entry("") }, false, colors::origin()),
        ("browser link", ReplEntry { frames: vec![browser], ..entry("1") }, false, colors::browser_link()),
        ("image caption", ReplEntry { frames: vec![image], ..entry("1") }, false, colors::image_note()),
        ("starting note", ReplEntry { in_flight: true, ..entry("1") }, true, colors::starting()),
        ("running note", ReplEntry { in_flight: true, ..entry("1") }, false, colors::running()),
        ("elapsed note", ReplEntry { elapsed_ms: 123_456, ..entry("1") }, false, colors::elapsed()),
    ];
    for (name, entry, starting, style) in notes {
        let mut owner = Owner::default();
        owner.set_cols(8);
        let lines = owner.build(&row("host", "row"), &[entry.clone()], starting, 8.0, 16.0, 0).0;
        // The typed code's row (and an origin marker has none) is not the note.
        let skip = usize::from(entry.origin.is_none());
        let note_rows = &lines[skip..];
        assert!(note_rows.len() >= 2, "{name}: the note is cut: {:?}", text(&lines));
        for line in note_rows {
            for span in line.spans.iter().filter(|s| !s.content.trim().is_empty()) {
                assert_eq!(span.style, style, "{name}: {:?}", span.content);
            }
        }
    }
}

/// One line of the manual's list of what the drawer does not do with a program's output, each with the text it is about
/// and what is drawn (`docs/src/guide/repl.md`, and the limits in `ui/CLAUDE.md`, say the same items).
#[test]
fn the_limits_the_manual_lists_are_what_the_drawer_draws() {
    let so = |t: &str| ReplFrame::Stdout { text: t.into() };
    let out = |frames: Vec<ReplFrame>| text(&frames_rows(80, frames)).into_iter().skip(1).collect::<Vec<_>>();
    // A tab is one space, so a repaint from the start of the line lands where a terminal's would not.
    assert_eq!(out(vec![so("a\tb\rX\n")]), ["X b"]);
    // A backspace, a bell and the other control characters are a space each.
    assert_eq!(out(vec![so("a\u{8}b\u{7}c\u{7f}d\n")]), ["a b c d"]);
    // Moving the cursor anywhere but to the start of the line does nothing: up shows the redraw as a new row, another column
    // writes after what is there.
    assert_eq!(out(vec![so("one\ntwo\n\u{1b}[1A\r\u{1b}[2KONE\n")]), ["one", "two", "ONE"]);
    assert_eq!(out(vec![so("abc\u{1b}[5Gd\n")]), ["abcd"]);
    // The move is ignored, so the text goes where the cursor was: over the start of the line after a `\r`.
    assert_eq!(out(vec![so("abcdef\r\u{1b}[4GX\n")]), ["Xbcdef"]);
    assert_eq!(out(vec![so("Progress:  10%\r\u{1b}[11G42%\n")]), ["42%gress:  10%"]);
    // A wide character is one place, so writing over it lands a place off; an accent written as its own mark is one of its own.
    assert_eq!(out(vec![so("\u{4e2d}\u{4e2d}\rab\n")]), ["ab"]);
    assert_eq!(out(vec![so("e\u{301}\r\u{4e2d}\n")]), ["\u{4e2d}\u{301}"]);
    // Erased cells are plain spaces, whatever background the program selected.
    let rows = frames_rows(80, vec![so("abcdef\rxy\u{1b}[42m\u{1b}[1K\n")]);
    assert_eq!(rows[1].spans.iter().map(|s| (s.content.to_string(), s.style.bg)).collect::<Vec<_>>(), [("   def".to_string(), None)]);
    // Underline, italic and hidden text are drawn as plain text.
    let rows = frames_rows(80, vec![so("\u{1b}[4mu\u{1b}[3mi\u{1b}[8mh\n")]);
    assert_eq!(rows[1].spans.iter().map(|s| (s.content.to_string(), s.style.add_modifier)).collect::<Vec<_>>(), [("uih".to_string(), Modifier::empty())]);
    // A line longer than 65,536 characters goes on in a row of its own, with no mark at the break.
    let long = out(vec![so(&format!("{}\n", "x".repeat(65_536 + 1)))]);
    let lens: Vec<usize> = long.iter().map(|r| r.chars().count()).collect();
    assert_eq!(lens.iter().sum::<usize>(), 65_537);
    assert_eq!(lens[lens.len() - 2..], [65_536 % 80, 1], "the 80-cell rows of the first 65,536 characters, then the one that is left over");
    // The start of a title or a link and a line break, its end a wait past the child's hold later: the break closed the line,
    // so the start is text and the end is a line of its own; the other order is one line, the title taken out. (Frames as
    // the real child sends them for `print("before \e]0;start\n"); sleep(0.3); print("end\a after\n")` and for
    // `print("before \e]0;start"); sleep(0.3); print("\nend\a after\n")`.)
    assert_eq!(out(vec![so("before "), so("\u{1b}]0;start\n"), so("end\u{7} after\n")]), ["before  ]0;start", "end  after"]);
    assert_eq!(out(vec![so("before "), so("\u{1b}]0;start"), so("\nend\u{7} after\n")]), ["before  after"]);
    assert_eq!(out(vec![so("before "), so("\u{1b}]8;;http://x\n"), so("end\u{1b}\\ after\n")]), ["before  ]8;;http://x", "end after"]);
    assert_eq!(out(vec![so("before "), so("\u{1b}]8;;http://x"), so("\nend\u{1b}\\ after\n")]), ["before  after"]);
    // A program's writes are one line until a line break: a return to the start of the line and an erase act across them.
    assert_eq!(out(vec![so("a"), so("b\n")]), ["ab"]);
    assert_eq!(out(vec![so("10 left"), so("\r9 left\u{1b}[K\n")]), ["9 left"]);
}

/// The text of the rows of one eval's frames, the log's own rows left off.
fn output_rows(cols: usize, frames: Vec<ReplFrame>) -> Vec<String> {
    let rows = text(&frames_rows(cols, frames));
    rows[1..].to_vec()
}

fn out(s: &str) -> ReplFrame {
    ReplFrame::Stdout { text: s.into() }
}

fn err(s: &str) -> ReplFrame {
    ReplFrame::Stderr { text: s.into() }
}

#[test]
fn the_writes_of_one_line_share_one_row() {
    // `print("a"); print("b")`, then a `Pkg.status`-shaped line written in pieces, then a repaint.
    assert_eq!(output_rows(80, vec![out("a"), out("b"), out("\n"), out("c")]), ["ab", "c"]);
    assert_eq!(
        output_rows(80, vec![out("Status "), out("`/p/Project.toml`"), out("\n"), out("  [abc] "), out("Pkg v1\n")]),
        ["Status `/p/Project.toml`", "  [abc] Pkg v1"]
    );
    let bar = (0..100).map(|i| out(&format!("\rProgress: {i:3}%"))).chain([out("\n")]).collect();
    assert_eq!(output_rows(80, bar), ["Progress:  99%"]);
    // The bar's rows are still cut by the pane: the whole line wraps, not each write.
    assert_eq!(output_rows(4, vec![out("ab"), out("cd"), out("ef")]), ["abcd", "ef"]);
}

#[test]
fn an_open_line_ends_at_any_other_frame_and_runs_are_per_stream() {
    // The other stream, a note, or a value between two writes ends the line; both streams keep their own open line.
    assert_eq!(output_rows(80, vec![out("a"), err("E"), out("b")]), ["a", "E", "b"]);
    assert_eq!(output_rows(80, vec![out("a"), ReplFrame::Value { mime: "text/plain".into(), text: "v".into() }, out("b")]), ["a", "v", "b"]);
    assert_eq!(output_rows(80, vec![err("x"), err("y\n"), out("z")]), ["xy", "z"]);
}

#[test]
fn a_color_carries_through_the_writes_of_an_open_line_and_a_resize_keeps_the_reader() {
    let lines = frames_rows(80, vec![out("\u{1b}[31ma"), out("b\u{1b}[39m"), out("c")]);
    assert_eq!(styled(&lines, "abc"), [("ab".into(), Some(Color::Red), false), ("c".into(), None, false)]);
    // A line of many writes is one logical line: a reader scrolled up in it keeps their place through a resize.
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    owner.set_cols(10);
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    for i in 0..30 {
        output(&mut model, &key, id, &format!("w{i:02}-------"));
    }
    output(&mut model, &key, id, "\nlast\n");
    finish(&mut model, &key, id);
    let (lines, now) = build(&mut owner, &model, &key, 5);
    let rows = text(&lines);
    let bottom = rows.len() - 1 - now as usize;
    let before = rows[bottom].clone();
    owner.set_cols(40);
    let (lines, now) = build(&mut owner, &model, &key, 5);
    let rows = text(&lines);
    let logical = rows[..=rows.len() - 1 - now as usize].iter().rev().find(|r| r.starts_with('w')).unwrap().clone();
    assert!(before.starts_with('w') || before == "last" || before.starts_with('-'), "{before}");
    assert!(logical.starts_with("w00"), "the one logical line of all the writes: {logical:?}");
}

#[test]
fn a_line_longer_than_the_cap_goes_on_in_a_row_of_its_own() {
    // 70 KiB written 1 KiB at a time with no newline: the reader ends the line where it reaches 64 KiB, as if the program had,
    // so the line a redraw cuts again is never longer than that.
    let frames: Vec<ReplFrame> = (0..70).map(|_| out(&"x".repeat(1024))).collect();
    let rows = output_rows(1_000_000, frames);
    assert_eq!(rows.len(), 2, "{:?}", rows.iter().map(String::len).collect::<Vec<_>>());
    assert_eq!(rows.iter().map(String::len).sum::<usize>(), 70 * 1024);
    assert_eq!(rows[0].len(), 64 * 1024);
}

#[test]
fn a_closed_line_is_never_joined_to_what_follows_so_its_rows_stay_cached() {
    // Joining past a newline would change no row, and make every frame of a long log one run that is wrapped again
    // whenever a write arrives: a run ends at the first frame whose text ends a line.
    let frames = [out("a"), out("b\n"), out("c"), out("d\r\n"), out("e")];
    assert_eq!([0, 1, 2, 3, 4].map(|at| super::open_run(&frames, &[], at)), [2, 2, 4, 4, 5]);
    assert_eq!(super::open_run(&frames, &[1], 0), 1, "a note is its own frame");
}

/// One eval of the writes `writes`, drawn after each one, as a person watching a program run sees it: the bytes read in all
/// by the readers of its runs, and the rows at the end.
fn drawn_after_each(writes: &[String]) -> (usize, Vec<String>) {
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    owner.set_cols(60);
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    let mut rows = Vec::new();
    for w in writes {
        output(&mut model, &key, id, w);
        rows = text(&build(&mut owner, &model, &key, 0).0);
    }
    (owner.wrap.bytes_read(), rows)
}

#[test]
fn a_line_that_grows_a_write_at_a_time_costs_what_each_write_adds() {
    // The work, counted as bytes read: every byte is read once, whatever the redraws after each write. A
    // `\r` and 3,000 colored pairs written a pair at a time took 210 s of the UI thread when each redraw read the line again
    // (the counts do not depend on how many: these are a third of that, to keep a debug build quick).
    let total = |w: &[String]| w.iter().map(String::len).sum::<usize>();
    let pairs: Vec<String> = std::iter::once("\r".to_string()).chain((0..1000).map(|_| "\u{1b}[31mx\u{1b}[32my".to_string())).collect();
    let (cells, rows) = drawn_after_each(&pairs);
    assert!(cells == total(&pairs), "{cells} bytes read for {} bytes", total(&pairs));
    // The eval is still running: its first row is the code, its last the running note, between them the line, cut at 60.
    assert_eq!(rows[1..rows.len() - 1].concat(), "xy".repeat(1000));
    let bar: Vec<String> = (0..2000).map(|i| format!("\rProgress: {:3}%", i % 100)).collect();
    let (cells, rows) = drawn_after_each(&bar);
    assert!(cells == total(&bar), "{cells} bytes read for {} bytes", total(&bar));
    assert_eq!(rows[1], "Progress:  99%");
    // Without a `\r` the line is text appended: the same count, and the rows are the line.
    let plain: Vec<String> = (0..1000).map(|i| format!("w{i:04} ")).collect();
    let (cells, rows) = drawn_after_each(&plain);
    assert!(cells == total(&plain), "{cells} bytes read for {} bytes", total(&plain));
    assert_eq!(rows[1..rows.len() - 1].concat(), plain.concat());
}

#[test]
fn a_run_no_write_was_added_to_is_not_read_or_cut_again() {
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    owner.set_cols(20);
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    output(&mut model, &key, id, "one \u{1b}[31mred");
    output(&mut model, &key, id, " line");
    finish(&mut model, &key, id);
    let first = build(&mut owner, &model, &key, 0).0;
    let read = owner.wrap.bytes_read();
    for _ in 0..5 {
        assert_eq!(text(&build(&mut owner, &model, &key, 0).0), text(&first));
    }
    assert_eq!(owner.wrap.bytes_read(), read, "a redraw that changes nothing reads nothing");
    // A new width cuts what was read; it reads nothing again.
    owner.set_cols(8);
    assert_eq!(text(&build(&mut owner, &model, &key, 0).0)[1..3], ["one red ", "line"]);
    assert_eq!(owner.wrap.bytes_read(), read);
}

#[test]
fn text_after_a_stray_string_opener_and_the_text_before_it_are_drawn() {
    // Held text is text with every line in it: the child's own test frame, a line before the opener, a value, an error.
    let rows = |frames: Vec<ReplFrame>| output_rows(80, frames);
    assert_eq!(rows(vec![out("\u{1b}Xnever ends\nline 2\n")]), [" Xnever ends", "line 2"]);
    assert_eq!(rows(vec![out("start "), out("\u{1b}]0;t\nline 1\nline 2\n")]), ["start  ]0;t", "line 1", "line 2"]);
    for opener in ["\u{1b}]", "\u{1b}P", "\u{1b}X", "\u{1b}^", "\u{1b}_"] {
        let shown = opener.replace('\u{1b}', " ");
        assert_eq!(rows(vec![out(&format!("a {opener}\nline 1\nline 2\n"))]), [format!("a {shown}"), "line 1".into(), "line 2".into()], "{opener:?}");
        let value = ReplFrame::Value { mime: "text/plain".into(), text: format!("v1{opener}x\nv2\nv3") };
        assert_eq!(rows(vec![value]), [format!("v1{shown}x"), "v2".into(), "v3".into()], "{opener:?}");
        let error = ReplFrame::Error { message: format!("e1{opener}x\ne2\ne3"), stacktrace: vec![] };
        assert_eq!(rows(vec![error]), [format!("ERROR: e1{shown}x"), "e2".into(), "e3".into()], "{opener:?}");
    }
    // Twenty ticks after a lone opener: none of them is lost.
    let ticks: Vec<ReplFrame> = std::iter::once(out("\u{1b}P")).chain((0..20).map(|i| out(&format!("tick {i}\n")))).collect();
    let drawn = rows(ticks);
    assert_eq!(drawn[0], " Ptick 0", "the opener, as text, with the first tick on its line");
    assert_eq!(drawn[1..], (1..20).map(|i| format!("tick {i}")).collect::<Vec<_>>()[..]);
}

#[test]
fn a_run_fed_in_pieces_and_drawn_after_each_is_the_run_fed_whole() {
    // Seeded random texts of words, newlines, `\r`, SGR, erase in line, every opener and terminator, wide glyphs: the rows of
    // writes that arrive one at a time, with a redraw after each, are the rows of the same writes drawn once, and the same at
    // another width. A few thousand cases.
    let parts = [
        "word", "ab", " ", "\n", "\r", "\t", "\u{1b}[31m", "\u{1b}[0m", "\u{1b}[K", "\u{1b}[1K", "\u{1b}[2K", "\u{1b}[1G", "\u{1b}]", "\u{1b}P", "\u{1b}X", "\u{1b}^", "\u{1b}_",
        "\u{1b}[", "\u{1b}", "\u{7}", "\u{1b}\\", "\u{4e2d}", "\u{e9}", "1;", "m", "long long long words that wrap",
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    let rows_of = |wrap: &mut Wrap, texts: &[&str]| -> Vec<(String, Option<Color>)> {
        let ((done, tail), _) = wrap.run((row("host", "row"), 7, 0), texts, colors::plain(), sgr::Sgr::default());
        done.0.iter().chain(&tail.0).flat_map(|l| l.spans.iter().map(|s| (s.content.to_string(), s.style.fg)).chain([("\n".to_string(), None)])).collect()
    };
    for case in 0..2500 {
        let writes: Vec<String> = (0..1 + next(5)).map(|_| (0..1 + next(8)).map(|_| parts[next(parts.len())]).collect()).collect();
        let texts: Vec<&str> = writes.iter().map(String::as_str).collect();
        for cols in [7, 40] {
            let mut whole = Wrap::default();
            whole.cols = cols;
            whole.begin();
            let want = rows_of(&mut whole, &texts);
            let mut piecemeal = Wrap::default();
            piecemeal.cols = cols;
            let mut got = Vec::new();
            for n in 1..=texts.len() {
                piecemeal.begin();
                got = rows_of(&mut piecemeal, &texts[..n]);
                piecemeal.end();
            }
            assert_eq!(got, want, "case {case}, {cols} columns: {writes:?}");
        }
    }
}

#[test]
fn held_text_that_reaches_the_line_cap_loses_no_line() {
    // The line is at 65,534 bytes when the opener comes: the cap ends it inside the held text, and the lines after it are there.
    let rows = output_rows(1_000_000, vec![out(&"x".repeat(65_534)), out("\u{1b}]ab\ncd\n")]);
    assert_eq!(rows.len(), 3, "{:?}", rows.iter().map(String::len).collect::<Vec<_>>());
    assert_eq!(rows[0], format!("{} ]", "x".repeat(65_534)));
    assert_eq!(rows[1..], ["ab", "cd"]);
}

#[test]
fn a_shorter_repaint_leaves_no_tail_of_the_longer_one() {
    // What every `\r` progress meter sends after its repaint, through the window: the writes of one line, an erase after each.
    assert_eq!(output_rows(80, vec![out("\r10 left\u{1b}[K"), out("\r9 left\u{1b}[K\n")]), ["9 left"]);
    assert_eq!(output_rows(80, vec![out("\rprocessing long_file_name.jl"), out("\r\u{1b}[2Kdone\n")]), ["done"]);
    let bar: Vec<ReplFrame> = (0..60).rev().map(|i| out(&format!("\rprocessing {}\u{1b}[K", "x".repeat(i)))).chain([out("\n")]).collect();
    assert_eq!(output_rows(80, bar), ["processing "]);
}

#[test]
fn a_reader_in_the_tail_of_a_run_stays_on_their_line_through_a_resize() {
    // One run: five finished lines, then the lines a stray string opener holds (the tail's lines) and the open line. A reader
    // whose bottom row is the first of a tail line is still there after the width changes: the tail's starts are counted
    // from where its rows begin, after those of the finished lines.
    let key = row("host", "row");
    let (mut model, mut owner) = (ReplModel::default(), Owner::default());
    owner.set_cols(20);
    model.edit_draft(DraftEdit::Replace("x".into()));
    let plan = model.begin_eval(target(&key)).unwrap();
    let id = plan.submission.eval_id();
    model.complete_submission(plan.submission, Ok(()));
    output(&mut model, &key, id, &format!("{}\u{1b}]0;t\nheld first line, a long one\nheld second line, another long one\nopen line, still being written", "finished\n".repeat(5)));
    for target in ["held second", "open line", "held first"] {
        owner.set_cols(20);
        let rows = text(&build(&mut owner, &model, &key, 0).0);
        let at = rows.iter().position(|r| r.starts_with(target)).unwrap();
        let scroll = (rows.len() - 1 - at) as u16;
        let (lines, now) = build(&mut owner, &model, &key, scroll);
        assert!(text(&lines)[lines.len() - 1 - now as usize].starts_with(target), "before the resize: {target}");
        owner.set_cols(11);
        let (lines, now) = build(&mut owner, &model, &key, scroll);
        let rows = text(&lines);
        assert!(rows[rows.len() - 1 - now as usize].starts_with(&target[..4]), "after the resize the reader is on `{target}`: {:?}", &rows[rows.len() - 1 - now as usize]);
    }
}
