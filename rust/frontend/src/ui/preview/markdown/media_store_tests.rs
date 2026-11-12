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
