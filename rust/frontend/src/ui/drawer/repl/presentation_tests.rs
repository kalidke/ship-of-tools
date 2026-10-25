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
    let layout = Owner::drawer_layout(Rect::new(3, 4, 20, 10), 3, 20, 999);
    assert_eq!(layout.scrollback, Rect::new(3, 4, 20, 7));
    assert_eq!(layout.input, Rect::new(3, 11, 20, 3));
    assert_eq!((layout.scroll, layout.window), (13, (0, 7)));
    model.commit_rendered_scroll(layout.scroll);
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
    assert_eq!(paint(&owner)[0], Paint::Scissor(0, 790, 200, 10));
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
        "stderrA", "stderrB", "[mimeA ⏎ mimeB] valueA", "valueB", "errorA", "errorB",
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
