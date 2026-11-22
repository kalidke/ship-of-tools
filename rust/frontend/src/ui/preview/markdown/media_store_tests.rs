//! A math snippet's raster is drawn by the worker: asked once per generation, a refusal terminal for it, a departed
//! generation's outcome changing nothing. The draw is a stand-in that holds each job until released or stopped, answers
//! `refused` for a job that says so, and counts the jobs it is given.

use super::*;
use crate::ui::preview::image::figure_store::FigureStore;
use crate::ui::preview::image::svg_preview::{settle_frame, SvgPreview};
use crate::ui::preview::image::svg_run::{Outcome, Stop};

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Bench {
    media: MarkdownMedia,
    draws: SvgDraws,
    figures: FigureStore,
    preview: SvgPreview,
    given: Arc<AtomicUsize>,
    /// The size of each batch the draw was given.
    calls: Arc<Mutex<Vec<usize>>>,
    release: Arc<AtomicBool>,
}

fn key(latex: &str) -> (String, bool) { (latex.to_owned(), false) }

impl Bench {
    fn new() -> Self { Self::with_cap(1 << 20) }

    /// A bench whose worker queues at most `cap` bytes of input.
    fn with_cap(cap: usize) -> Self {
        let (given, release) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let (count, held, calls) = (given.clone(), release.clone(), Arc::new(Mutex::new(Vec::new())));
        let sizes = calls.clone();
        let draw = Arc::new(move |jobs: Vec<Job>, stop: &Stop| {
            sizes.lock().unwrap().push(jobs.len());
            let begun = Instant::now();
            jobs.into_iter()
                .map(|job| {
                    count.fetch_add(1, SeqCst);
                    let refused = job.svg.starts_with(b"refused");
                    while !refused && !held.load(SeqCst) && !stop.is_stopped() && begun.elapsed() < Duration::from_secs(20) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    let Ask::Stretch(w, h) = job.ask else { panic!("math is stretched") };
                    match () {
                        _ if stop.is_stopped() => Outcome::Stale,
                        _ if refused => Outcome::Refused("svg parse failed".into()),
                        _ => Outcome::Drawn { w, h, rgba: vec![0; 4] },
                    }
                })
                .collect()
        });
        Self {
            media: MarkdownMedia::default(),
            draws: SvgDraws::new(cap, || {}, draw),
            figures: FigureStore::default(),
            preview: SvgPreview::default(),
            given,
            calls,
            release,
        }
    }

    /// A reply for `latex` whose SVG is `svg`.
    fn reply(&mut self, latex: &str, svg: &str) {
        let entry = MathSvg { svg_bytes: svg.into(), ex: 8.0, width_ex: None, height_ex: None, vertical_align_ex: None, rasterised: None, asked: None };
        self.media.install_math(key(latex), entry);
    }

    /// One frame as the window runs it: the worker's outcomes settle, then the paint asks for what it shows (`keys`,
    /// each at `size`).
    fn frame(&mut self, generation: u64, keys: &[&str], size: (u32, u32)) -> Vec<((String, bool), Settled)> {
        let frame = settle_frame(&mut self.draws, &mut self.figures, &mut self.preview, &mut self.media, generation);
        let wanted = keys.iter().filter(|k| self.media.math_to_rasterise(&key(k), generation).is_some()).map(|k| (key(k), size)).collect();
        self.media.request_rasters(&mut self.draws, generation, wanted);
        frame.math
    }

    /// Frames until a math outcome settles; exactly one does.
    fn settled(&mut self, generation: u64, keys: &[&str]) -> Settled {
        let begun = Instant::now();
        loop {
            let mut got = self.frame(generation, keys, (12, 8));
            if let Some((_, settled)) = got.pop() {
                assert!(got.is_empty(), "one outcome settles, once");
                return settled;
            }
            assert!(begun.elapsed() < Duration::from_secs(10), "nothing settled");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn given(&self) -> usize { self.given.load(SeqCst) }

    /// The size of each batch the draw was given: one entry per child.
    fn calls(&self) -> Vec<usize> { self.calls.lock().unwrap().clone() }

    /// Lets the worker finish what it has started.
    fn settle_worker(&self) { std::thread::sleep(Duration::from_millis(100)); }
}

#[test]
fn the_key_at_the_worker_round_trips_and_no_figure_has_it() {
    for k in [("x^2".to_owned(), true), ("a/b".to_owned(), false), (String::new(), false), ("d".to_owned(), true)] {
        assert_eq!(math_key(&job_key(&k)), Some(k.clone()));
        assert!(is_math_job(&job_key(&k)));
    }
    assert!(!is_math_job("figures/a.svg") && !is_math_job("://preview"));
}

#[test]
fn a_math_snippet_is_submitted_once() {
    let mut bench = Bench::new();
    bench.reply("x", "<svg/>");
    for _ in 0..3 {
        bench.frame(1, &["x"], (12, 8));
    }
    bench.release.store(true, SeqCst);
    let Settled::Drawn { w, h, .. } = bench.settled(1, &["x"]) else { panic!("not drawn") };
    assert_eq!((w, h), (12, 8), "drawn onto the size it was asked at");
    for _ in 0..3 {
        bench.frame(1, &["x"], (24, 16));
    }
    bench.settle_worker();
    assert_eq!(bench.given(), 1, "asked once, over six frames and a resize");
}

#[test]
fn a_refused_math_snippet_is_not_submitted_on_later_frames() {
    let mut bench = Bench::new();
    bench.reply("x", "refused");
    assert!(matches!(bench.settled(1, &["x"]), Settled::Failed(_)));
    for _ in 0..2 {
        bench.frame(1, &["x"], (12, 8));
    }
    bench.frame(1, &["x"], (24, 16));
    bench.settle_worker();
    assert_eq!(bench.given(), 1, "a refusal is terminal for the snippet under this generation");
    // A fresh reply replaces the entry, and the new one is asked.
    bench.reply("x", "refused");
    bench.frame(1, &["x"], (12, 8));
    bench.settle_worker();
    assert_eq!(bench.given(), 2);
}

#[test]
fn the_snippets_of_a_frame_go_to_the_worker_together() {
    let mut bench = Bench::new();
    for latex in ["a", "b", "c"] {
        bench.reply(latex, "<svg/>");
    }
    bench.frame(1, &["a", "b", "c", "a"], (12, 8));
    bench.release.store(true, SeqCst);
    let mut drawn = 0;
    let begun = Instant::now();
    while drawn < 3 {
        drawn += bench.frame(1, &["a", "b", "c"], (12, 8)).len();
        assert!(begun.elapsed() < Duration::from_secs(10), "{drawn} drawn");
    }
    assert_eq!(bench.given(), 3, "each snippet once, though one is shown twice");
    assert_eq!(bench.calls(), [3], "one draw, so one child, for the frame's three snippets");
}

#[test]
fn a_batch_refused_at_submit_is_not_asked_again() {
    // A job fills the queue behind a held draw, so the frame's snippet does not fit and is refused. It stays asked: when
    // room comes, no frame sends it, and the worker is given the two jobs and nothing else.
    let mut bench = Bench::with_cap(10);
    bench.draws.submit("h1".into(), 1, Job { ask: Ask::Stretch(1, 1), svg: Arc::from(&b"hold1"[..]) }).unwrap();
    while bench.given() < 1 {
        std::thread::sleep(Duration::from_millis(1));
    }
    bench.draws.submit("h2".into(), 1, Job { ask: Ask::Stretch(1, 1), svg: Arc::from(&b"hold2"[..]) }).unwrap();
    bench.reply("x", "<svg/>");
    for _ in 0..3 {
        bench.frame(1, &["x"], (12, 8));
    }
    bench.release.store(true, SeqCst);
    while bench.given() < 2 {
        std::thread::sleep(Duration::from_millis(1));
    }
    for _ in 0..3 {
        bench.frame(1, &["x"], (12, 8));
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(bench.given(), 2, "the refused snippet was asked again once there was room");
}

#[test]
fn a_departed_generations_outcome_does_not_undo_a_new_ask() {
    let mut bench = Bench::new();
    bench.reply("x", "<svg/>");
    bench.frame(1, &["x"], (12, 8));
    while bench.given() < 1 {
        std::thread::sleep(Duration::from_millis(1));
    }
    // The pane's source changes: the new generation asks at once, and the old job's `Stale` is still to be drained.
    let wanted = vec![(key("x"), (12, 8))];
    bench.media.request_rasters(&mut bench.draws, 2, wanted);
    let late = bench.frame(2, &["x"], (12, 8));
    assert!(late.is_empty(), "the departed generation's outcome settles nothing: {:?}", late.len());
    bench.settle_worker();
    assert_eq!(bench.given(), 2, "the new generation's ask stands and is not repeated");
    bench.release.store(true, SeqCst);
    assert!(matches!(bench.settled(2, &["x"]), Settled::Drawn { .. }));
}

#[test]
fn a_stale_snippet_is_asked_again_under_the_current_generation() {
    let mut bench = Bench::new();
    bench.reply("x", "<svg/>");
    bench.frame(1, &["x"], (12, 8));
    while bench.given() < 1 {
        std::thread::sleep(Duration::from_millis(1));
    }
    // The generation advances with no snippet asking: the held draw is stopped and its job comes back `Stale`.
    assert!(matches!(bench.settled(2, &[]), Settled::Unhandled), "a stale job is unhandled again");
    bench.release.store(true, SeqCst);
    assert!(matches!(bench.settled(2, &["x"]), Settled::Drawn { .. }));
    assert_eq!(bench.given(), 2);
}

fn table_set(fs: &mut cosmic_text::FontSystem, texts: &[&str], font_px: f32, line_h_px: f32) -> Vec<TableBufferEntry> {
    texts.iter().map(|text| super::super::media::table_buffer(fs, (*text).to_owned(), font_px, line_h_px)).collect()
}

fn sources(texts: &[&str], font_px: f32, line_h_px: f32) -> Vec<(String, f32, f32)> {
    texts.iter().map(|text| ((*text).to_owned(), font_px, line_h_px)).collect()
}

#[test]
fn a_set_left_is_taken_back_for_the_same_sources_and_metrics_only() {
    let mut fs = cosmic_text::FontSystem::new();
    let mut media = MarkdownMedia::default();
    let (a, b) = (["│ a │\n", "│ b │\n"], ["│ other │\n"]);
    media.replace_tables(table_set(&mut fs, &a, 12.0, 15.0));
    media.scroll_tables_right(40.0);
    media.replace_tables(table_set(&mut fs, &b, 12.0, 15.0));
    assert!(media.tables_match(&sources(&b, 12.0, 15.0)));
    assert!(!media.tables_match(&sources(&a, 12.0, 15.0)), "the shown set is the newer one");
    // Each input of the key, changed alone, takes nothing back.
    assert!(!media.take_parked(&sources(&["│ a │\n", "│ c │\n"], 12.0, 15.0)), "one table's text");
    assert!(!media.take_parked(&sources(&["│ a │\n"], 12.0, 15.0)), "the count");
    assert!(!media.take_parked(&sources(&["│ b │\n", "│ a │\n"], 12.0, 15.0)), "the order");
    assert!(!media.take_parked(&sources(&a, 13.0, 15.0)), "the font size");
    assert!(!media.take_parked(&sources(&a, 12.0, 16.0)), "the line height");
    assert!(media.tables_match(&sources(&b, 12.0, 15.0)), "a refusal leaves the shown set alone");
    // The same key takes its set back, at the left edge, and keeps the one it replaces.
    assert!(media.take_parked(&sources(&a, 12.0, 15.0)));
    assert!(media.tables_match(&sources(&a, 12.0, 15.0)));
    assert_eq!(media.table_scroll_px(), 0.0);
    assert!(media.take_parked(&sources(&b, 12.0, 15.0)), "the set it replaced is kept in turn");
    // A clear keeps the set it empties, and a set is in one place only: shown, it is no longer kept.
    media.clear_tables();
    assert!(media.tables().is_empty());
    assert!(media.take_parked(&sources(&b, 12.0, 15.0)));
    assert!(media.take_parked(&sources(&a, 12.0, 15.0)), "both sets were kept");
    assert!(!media.take_parked(&sources(&a, 12.0, 15.0)), "a shown set is not kept as well");
}

#[test]
fn the_sets_kept_stay_within_the_budget_oldest_first() {
    let mut fs = cosmic_text::FontSystem::new();
    let mut media = MarkdownMedia::default();
    let text = ["│ 0123456789 │\n"];
    let each = text[0].len();
    // Three fit, a fourth pushes the oldest out; the budget counts bytes of rendered table text.
    let budget = each * 3;
    for n in 0..4 {
        let set = table_set(&mut fs, &[&format!("{}{n}", text[0])], 12.0, 15.0);
        media.park(set, budget);
    }
    let kept: Vec<usize> = media.parked.iter().map(|set| set.iter().map(TableBufferEntry::charge).sum()).collect();
    assert!(kept.iter().sum::<usize>() <= budget, "{kept:?}");
    assert_eq!(kept.len(), 2, "each set is one byte over `each`, so two fit in three: {kept:?}");
    // The newest are the ones kept.
    assert!(media.parked[0][0].built_from(&format!("{}3", text[0]), 12.0, 15.0));
    // A set over the budget by itself is not kept and drops none of the others.
    let big = table_set(&mut fs, &[&"x".repeat(budget + 1)], 12.0, 15.0);
    media.park(big, budget);
    assert_eq!(media.parked.len(), 2);
}

#[test]
fn a_set_is_charged_its_rendered_bytes_whatever_it_holds() {
    // A table with one long cell pads every row to it, so a set of mostly spaces is the worst case for memory (520
    // resident bytes per byte charged, against 76 to 355 for text and borders, measured): the charge is the byte count.
    let mut fs = cosmic_text::FontSystem::new();
    let padded = format!("│{}│\n", " ".repeat(498)).repeat(20);
    let set = table_set(&mut fs, &[&padded], 12.0, 15.0);
    assert_eq!(set[0].charge(), padded.len());
    let glyphs: usize = set[0].buffer.layout_runs().map(|run| run.glyphs.len()).sum();
    assert_eq!(glyphs, 20 * 500, "every padding character is a shaped glyph the charge stands for");
}

#[test]
fn only_the_rows_in_view_are_shaped_and_a_kept_set_comes_back_as_it_was() {
    use super::super::media::table_entry;
    use crate::ui::render::quad::ScreenRect;
    let mut fs = cosmic_text::FontSystem::new();
    let mut media = MarkdownMedia::default();
    // Three tables of ten rows of 15 px, placed one under another from y = 0.
    let texts: Vec<String> = ["a", "b", "c"].iter().map(|c| format!("│ {c} │\n").repeat(10)).collect();
    media.replace_tables(texts.iter().map(|text| table_entry(&mut fs, text.clone(), 12.0, 15.0)).collect());
    let places: Vec<(usize, ScreenRect)> = (0..3).map(|t| (t, ScreenRect { x: 0.0, y: 150.0 * t as f32, w: 100.0, h: 150.0 })).collect();
    let shaped = |media: &MarkdownMedia| -> Vec<Vec<usize>> {
        media.tables().iter().map(|entry| {
            entry.buffer.lines.iter().enumerate().filter(|(_, line)| line.layout_opt().is_some()).map(|(row, _)| row).collect()
        }).collect()
    };
    let none: Vec<Vec<usize>> = vec![vec![]; 3];
    assert_eq!(shaped(&media), none, "a new set shapes nothing");
    // A pane from y = 120 to 200 holds rows 8 and 9 of the first table and rows 0 to 2 of the second whole; row 3
    // ends past its foot, so it is not drawn.
    let pane = ScreenRect { x: 0.0, y: 120.0, w: 100.0, h: 80.0 };
    assert_eq!(media.show_rows(&mut fs, &places, pane, false), (true, 0), "a deferred frame leaves the rows in view waiting");
    assert_eq!(shaped(&media), none);
    assert_eq!(media.tables()[0].drawn_top(0.0, &(8..10)), None, "a table with a row in view not shaped is not drawn");
    assert_eq!(media.show_rows(&mut fs, &places, pane, true), (false, 5));
    assert_eq!(shaped(&media), [vec![8, 9], vec![0, 1, 2], Vec::<usize>::new()]);
    assert_eq!(media.tables()[0].rows_in(places[0].1, pane), 8..10);
    assert_eq!(media.tables()[0].drawn_top(0.0, &(8..10)), Some(120.0), "drawn from the first row in view, at its place");
    assert_eq!(media.tables()[1].drawn_top(150.0, &(0..3)), Some(150.0));
    // A row shaped once is not shaped again: a 15 px scroll shapes the one row that comes into view.
    let shaping = |media: &MarkdownMedia| media.tables()[1].buffer.lines[0].shape_opt().as_ref().map(|line| line.spans.as_ptr() as usize);
    let before = shaping(&media);
    assert_eq!(media.show_rows(&mut fs, &places, ScreenRect { y: 135.0, ..pane }, true), (false, 1));
    assert_eq!(shaped(&media), [vec![8, 9], vec![0, 1, 2, 3], Vec::<usize>::new()]);
    assert_eq!(shaping(&media), before);
    // The key and the charge are a table's text and metrics, shaped or not: a set kept with some rows shaped comes back
    // with them, and the rest are shaped as they come into view.
    media.clear_tables();
    assert_eq!(media.parked[0].iter().map(TableBufferEntry::charge).sum::<usize>(), texts.concat().len());
    let keys: Vec<&str> = texts.iter().map(String::as_str).collect();
    assert!(media.take_parked(&sources(&keys, 12.0, 15.0)));
    assert_eq!(shaped(&media), [vec![8, 9], vec![0, 1, 2, 3], Vec::<usize>::new()], "a kept set comes back with the rows it had shaped");
}

#[test]
fn the_rows_beyond_the_view_are_shaped_nearest_first_until_the_budget_has_passed() {
    use super::super::media::table_entry;
    use crate::ui::render::quad::ScreenRect;
    use std::time::Duration;
    let mut fs = cosmic_text::FontSystem::new();
    // Two tables of ten rows of 15 px. Row r of the first is padded to 33 r + 1 characters, up to about 300; the first
    // is placed at y = 0, the second has no place this frame.
    let first: String = (0..10).map(|r| format!("│ {:<width$}│\n", "x", width = 1 + 33 * r)).collect();
    let texts = [first, "│ b │\n".repeat(10)];
    let set = |fs: &mut cosmic_text::FontSystem| {
        let mut media = MarkdownMedia::default();
        media.replace_tables(texts.iter().map(|text| table_entry(fs, text.clone(), 12.0, 15.0)).collect());
        media
    };
    let places = [(0, ScreenRect { x: 0.0, y: 0.0, w: 100.0, h: 150.0 })];
    let pane = ScreenRect { x: 0.0, y: 60.0, w: 100.0, h: 30.0 };
    let shaped = |media: &MarkdownMedia| -> Vec<(usize, usize)> {
        media.tables().iter().enumerate().flat_map(|(table, entry)| {
            entry.buffer.lines.iter().enumerate().filter(|(_, line)| line.layout_opt().is_some()).map(move |(row, _)| (table, row))
        }).collect()
    };
    let mut media = set(&mut fs);
    assert_eq!(media.show_rows(&mut fs, &places, pane, true), (false, 2), "rows 4 and 5 are in view");
    // A budget of zero shapes one row a call, whatever its text: the two touching the view (3 and 6, the lower index
    // first), then outward a row at a time on both sides, then the table with no place, until the set is whole.
    let mut order = Vec::new();
    while media.has_unshaped() && order.len() < 20 {
        let before = shaped(&media);
        media.shape_beyond(&mut fs, &places, pane, Duration::ZERO);
        let new: Vec<(usize, usize)> = shaped(&media).into_iter().filter(|row| !before.contains(row)).collect();
        assert_eq!(new.len(), 1, "a call with no time shaped {new:?}");
        order.extend(new);
    }
    let want: Vec<(usize, usize)> = [3, 6, 2, 7, 1, 8, 0, 9].into_iter().map(|row| (0, row)).chain((0..10).map(|row| (1, row))).collect();
    assert_eq!(order, want);
    media.shape_beyond(&mut fs, &places, pane, Duration::ZERO);
    assert_eq!(shaped(&media).len(), 20, "a whole set shapes nothing");
    // With no limit one call shapes every row left.
    let mut media = set(&mut fs);
    media.show_rows(&mut fs, &places, pane, true);
    media.shape_beyond(&mut fs, &places, pane, Duration::MAX);
    assert!(!media.has_unshaped(), "a budget with no limit left rows");
}

#[test]
fn the_real_budget_keeps_a_set_at_it_and_drops_one_past_it() {
    use super::super::media::unshaped;
    // What the contract says of the constant: a set of the diagnosed file's size (140,198 bytes of table text) is kept,
    // and the whole budget at the worst measured factor (520 resident bytes per byte) stays near 100 MB.
    assert!(PARKED_TABLE_BYTES >= 140_198);
    assert!(PARKED_TABLE_BYTES * 520 <= 110 << 20);
    let mut media = MarkdownMedia::default();
    // Entries that hold text and no shaping, so the test takes no time to build a set of the budget's size.
    media.replace_tables(vec![unshaped("x".repeat(PARKED_TABLE_BYTES), 12.0, 15.0)]);
    media.clear_tables();
    assert_eq!(media.parked.len(), 1, "a set at the budget is kept");
    media.replace_tables(vec![unshaped("y".repeat(PARKED_TABLE_BYTES + 1), 12.0, 15.0)]);
    media.clear_tables();
    assert_eq!(media.parked.len(), 1, "a set one byte past it is not kept, and drops none that is");
    assert!(media.parked[0][0].built_from(&"x".repeat(PARKED_TABLE_BYTES), 12.0, 15.0));
}
