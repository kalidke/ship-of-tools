use super::*;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::time::{Duration, Instant};

use super::super::svg_run::Stop;

const FIT: Ask = Ask::Fit { pane: (800, 600), cap: 16384 };

/// A stand-in draw: it counts the jobs it is given, holds each until released or stopped, answers `refused` for a job
/// that says so, and otherwise draws a picture whose width is the job's first byte.
struct Stand {
    given: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
}

impl Stand {
    fn new() -> Self {
        Self { given: Arc::default(), release: Arc::default() }
    }

    fn draws(&self) -> SvgDraws {
        let (given, release) = (self.given.clone(), self.release.clone());
        let draw = Arc::new(move |jobs: Vec<Job>, stop: &Stop| {
            let begun = Instant::now();
            jobs.into_iter()
                .map(|job| {
                    given.fetch_add(1, SeqCst);
                    let refused = job.svg.starts_with(b"refused");
                    while !refused && !release.load(SeqCst) && !stop.is_stopped() && begun.elapsed() < Duration::from_secs(20) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    if stop.is_stopped() {
                        Outcome::Stale
                    } else if refused {
                        Outcome::Refused("svg parse failed".into())
                    } else {
                        Outcome::Drawn { w: u32::from(job.svg[0]), h: 1, rgba: vec![0; 4 * usize::from(job.svg[0])] }
                    }
                })
                .collect()
        });
        SvgDraws::new(1 << 20, || {}, draw)
    }

    fn given(&self) -> usize {
        self.given.load(SeqCst)
    }

    fn release(&self) {
        self.release.store(true, SeqCst);
    }
}

/// The window's frame steps, as `settle_frame` runs them, until the preview or a figure settles.
fn frames(draws: &mut SvgDraws, figures: &mut FigureStore, preview: &mut SvgPreview, generation: u64) -> Frame {
    let begun = Instant::now();
    loop {
        let frame = settle_frame(draws, figures, preview, generation);
        if frame.preview.is_some() || !frame.figures.is_empty() {
            return frame;
        }
        assert!(begun.elapsed() < Duration::from_secs(10), "nothing settled");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn picture_width(step: Option<Step>) -> u32 {
    match step {
        Some(Step::Picture(picture)) => picture.w,
        Some(Step::Refused(why)) => panic!("refused: {why}"),
        Some(Step::Pending | Step::Held) | None => panic!("no picture"),
    }
}

#[test]
fn a_current_preview_installs_its_raster_and_a_stale_one_does_not() {
    let stand = Stand::new();
    let (mut draws, mut figures, mut preview) = (stand.draws(), FigureStore::default(), SvgPreview::default());
    assert!(matches!(preview.show(&mut draws, 1, b"\x05", FIT), Step::Pending), "the text stays until the picture is drawn");
    stand.release();
    assert_eq!(picture_width(frames(&mut draws, &mut figures, &mut preview, 1).preview), 5);

    // The pane moves on while a job is held: its outcome is stale and installs nothing, and the new source is drawn.
    let stand = Stand::new();
    let (mut draws, mut preview) = (stand.draws(), SvgPreview::default());
    assert!(matches!(preview.show(&mut draws, 1, b"\x05", FIT), Step::Pending));
    let begun = Instant::now();
    while stand.given() == 0 && begun.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(settle_frame(&mut draws, &mut figures, &mut preview, 2).preview.is_none(), "the departed job is not shown");
    stand.release();
    assert!(matches!(preview.show(&mut draws, 2, b"\x06", FIT), Step::Pending));
    assert_eq!(picture_width(frames(&mut draws, &mut figures, &mut preview, 2).preview), 6, "the new source, never the old");
}

#[test]
fn an_equal_preview_is_installed_from_the_cache_without_a_draw() {
    let stand = Stand::new();
    stand.release();
    let (mut draws, mut figures, mut preview) = (stand.draws(), FigureStore::default(), SvgPreview::default());
    assert!(matches!(preview.show(&mut draws, 1, b"\x07", FIT), Step::Pending));
    assert_eq!(picture_width(frames(&mut draws, &mut figures, &mut preview, 1).preview), 7);
    assert_eq!(stand.given(), 1);
    // The same bytes and ask under a new generation (a row switch back, a text-scale change): no child.
    let Step::Picture(again) = preview.show(&mut draws, 2, b"\x07", FIT) else { panic!("not from the cache") };
    assert_eq!(again.w, 7);
    assert_eq!(stand.given(), 1, "an equal preview draws no second time");
    // Another ask is another picture.
    let other = Ask::Fit { pane: (400, 300), cap: 16384 };
    assert!(matches!(preview.show(&mut draws, 2, b"\x07", other), Step::Held), "its picture stays up meanwhile");
    assert_eq!(picture_width(frames(&mut draws, &mut figures, &mut preview, 2).preview), 7);
    assert_eq!(stand.given(), 2);
}

#[test]
fn two_outcomes_are_kept_and_the_oldest_goes() {
    let stand = Stand::new();
    stand.release();
    let (mut draws, mut figures, mut preview) = (stand.draws(), FigureStore::default(), SvgPreview::default());
    for (generation, byte) in [(1, 1u8), (2, 2), (3, 3)] {
        preview.show(&mut draws, generation, &[byte], FIT);
        picture_width(frames(&mut draws, &mut figures, &mut preview, generation).preview);
    }
    assert_eq!(stand.given(), 3);
    assert!(matches!(preview.show(&mut draws, 4, &[3], FIT), Step::Picture(_)));
    assert!(matches!(preview.show(&mut draws, 5, &[2], FIT), Step::Picture(_)));
    assert_eq!(stand.given(), 3, "the last two are kept");
    assert!(matches!(preview.show(&mut draws, 6, &[1], FIT), Step::Pending), "the first was dropped: drawn again");
}

#[test]
fn a_refused_preview_is_not_drawn_again_on_return() {
    let stand = Stand::new();
    let (mut draws, mut figures, mut preview) = (stand.draws(), FigureStore::default(), SvgPreview::default());
    assert!(matches!(preview.show(&mut draws, 1, b"refused", FIT), Step::Pending));
    let frame = frames(&mut draws, &mut figures, &mut preview, 1);
    assert!(matches!(frame.preview, Some(Step::Refused(why)) if why == "svg parse failed"));
    assert_eq!(stand.given(), 1);
    // Back on the file under a new generation, and again: the words, and no child.
    for generation in [2, 3] {
        assert!(matches!(preview.show(&mut draws, generation, b"refused", FIT), Step::Refused(why) if why == "svg parse failed"));
    }
    assert_eq!(stand.given(), 1, "a refused file costs one child");
}

#[test]
fn a_late_outcome_of_a_departed_generation_does_not_undo_the_job_in_flight() {
    let stand = Stand::new();
    let (mut draws, mut figures, mut preview) = (stand.draws(), FigureStore::default(), SvgPreview::default());
    assert!(matches!(preview.show(&mut draws, 2, b"\x04", FIT), Step::Pending));
    for outcome in [Outcome::Refused("x".into()), Outcome::Stale] {
        assert!(preview.settle(Done { key: PREVIEW_KEY.into(), generation: 1, outcome }).is_none());
    }
    assert!(matches!(preview.show(&mut draws, 2, b"\x04", FIT), Step::Pending), "still in flight, not resubmitted");
    stand.release();
    assert_eq!(picture_width(frames(&mut draws, &mut figures, &mut preview, 2).preview), 4);
    assert_eq!(stand.given(), 1, "one job drew it");
}

/// Jobs the worker has run once every job queued before this call has: a sentinel goes through the one worker behind
/// them, so no wait stands in for "nothing else was queued".
fn jobs_run_through(stand: &Stand, draws: &mut SvgDraws, generation: u64) -> usize {
    let job = Job { ask: Ask::Stretch(1, 1), svg: Arc::from(&b"\x01"[..]) };
    stand.release();
    draws.submit("sentinel".into(), generation, job).expect("queued");
    let begun = Instant::now();
    let mut seen = false;
    while !seen {
        assert!(begun.elapsed() < Duration::from_secs(10), "the sentinel never came back");
        draws.drain(|done| seen |= done.key == "sentinel");
        std::thread::sleep(Duration::from_millis(1));
    }
    stand.given() - 1
}

#[test]
fn a_second_request_of_one_generation_starts_no_second_job() {
    let stand = Stand::new();
    stand.release();
    let (mut draws, mut figures, mut preview) = (stand.draws(), FigureStore::default(), SvgPreview::default());
    for _ in 0..3 {
        assert!(matches!(preview.show(&mut draws, 1, b"\x05", FIT), Step::Pending));
    }
    assert_eq!(picture_width(frames(&mut draws, &mut figures, &mut preview, 1).preview), 5);
    assert_eq!(jobs_run_through(&stand, &mut draws, 1), 1, "three requests of one generation drew once");
}

#[test]
fn a_refused_file_is_not_drawn_again_at_another_size() {
    let stand = Stand::new();
    let (mut draws, mut figures, mut preview) = (stand.draws(), FigureStore::default(), SvgPreview::default());
    preview.show(&mut draws, 1, b"refused", FIT);
    assert!(matches!(frames(&mut draws, &mut figures, &mut preview, 1).preview, Some(Step::Refused(_))));
    let other = Ask::Fit { pane: (400, 300), cap: 16384 };
    assert!(matches!(preview.show(&mut draws, 1, b"refused", other), Step::Refused(why) if why == "svg parse failed"));
    assert_eq!(jobs_run_through(&stand, &mut draws, 1), 1, "the refusal belongs to the file, whatever the size");
}

#[test]
fn a_picture_stays_up_while_its_redraw_is_with_the_worker_and_never_across_sources() {
    let stand = Stand::new();
    let (mut draws, mut figures, mut preview) = (stand.draws(), FigureStore::default(), SvgPreview::default());
    assert!(matches!(preview.show(&mut draws, 1, b"\x05", FIT), Step::Pending), "nothing is up yet");
    stand.release();
    assert_eq!(picture_width(frames(&mut draws, &mut figures, &mut preview, 1).preview), 5);
    // The same source asked at another size: its picture stays until the new one replaces it.
    let other = Ask::Fit { pane: (400, 300), cap: 16384 };
    assert!(matches!(preview.show(&mut draws, 1, b"\x05", other), Step::Held));
    assert!(matches!(preview.show(&mut draws, 1, b"\x05", other), Step::Held), "and again while it is with the worker");
    assert_eq!(picture_width(frames(&mut draws, &mut figures, &mut preview, 1).preview), 5);
    // Another source: the picture up belongs to the first, so this one is pending, never held.
    assert!(matches!(preview.show(&mut draws, 2, b"\x06", FIT), Step::Pending));
    // An upload that failed leaves nothing up to hold.
    assert_eq!(picture_width(frames(&mut draws, &mut figures, &mut preview, 2).preview), 6);
    preview.not_installed();
    assert!(matches!(preview.show(&mut draws, 2, b"\x06", other), Step::Pending));
}

#[test]
fn files_of_one_length_are_not_taken_for_each_other() {
    let stand = Stand::new();
    stand.release();
    let (mut draws, mut figures, mut preview) = (stand.draws(), FigureStore::default(), SvgPreview::default());
    preview.show(&mut draws, 1, b"\x05a", FIT);
    assert_eq!(picture_width(frames(&mut draws, &mut figures, &mut preview, 1).preview), 5);
    assert!(matches!(preview.show(&mut draws, 2, b"\x05b", FIT), Step::Pending), "other bytes, same length: drawn");
    assert!(matches!(preview.show(&mut draws, 3, b"\x05a", FIT), Step::Picture(_)), "the same bytes: kept");
}

#[test]
fn a_figure_and_the_preview_each_settle_through_the_one_drain() {
    let stand = Stand::new();
    stand.release();
    let (mut draws, mut figures, mut preview) = (stand.draws(), FigureStore::default(), SvgPreview::default());
    figures.mark_pending("a.svg".into());
    figures.route_reply(&mut draws, 1, "a.svg".into(), "image/svg+xml", vec![9]);
    assert!(matches!(preview.show(&mut draws, 1, b"\x03", FIT), Step::Pending));
    let (mut figure, mut shown) = (None, None);
    let begun = Instant::now();
    while figure.is_none() || shown.is_none() {
        assert!(begun.elapsed() < Duration::from_secs(10), "both settle");
        let frame = settle_frame(&mut draws, &mut figures, &mut preview, 1);
        figure = figure.or(frame.figures.into_iter().next());
        shown = shown.or(frame.preview);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(figure, Some((url, Settled::Drawn { w: 9, .. })) if url == "a.svg"));
    assert_eq!(picture_width(shown), 3);
}

#[test]
fn a_preview_the_worker_will_not_take_is_refused_at_once() {
    let mut draws = SvgDraws::new(1, || {}, Arc::new(|_, _| Vec::new()));
    let mut preview = SvgPreview::default();
    assert!(matches!(preview.show(&mut draws, 1, b"<svg/>", FIT), Step::Refused(_)), "past a cap of one byte");
    assert!(matches!(preview.show(&mut draws, 1, b"<svg/>", FIT), Step::Refused(_)), "and terminal for the file");
}
