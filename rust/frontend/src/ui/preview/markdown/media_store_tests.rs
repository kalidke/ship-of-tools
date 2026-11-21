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
