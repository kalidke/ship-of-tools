use super::*;

// --- Bug 1: an early `figure.get` failure must reach a terminal state
// (not warn-and-drop), and a markdown reload must make a previously-
// failed figure URL retryable — but nothing SHORT of that reload (in
// particular, a cached-bytes reflow) may clear it, or the failure
// refires every frame forever (round-2 field report). ---

#[test]
fn fail_figure_clears_pending_and_marks_failed() {
    let mut pending: std::collections::HashSet<String> =
        ["figures/x.png".to_string()].into_iter().collect();
    let mut failed: std::collections::HashSet<String> = std::collections::HashSet::new();
    fail_figure(&mut pending, &mut failed, "figures/x.png".to_string());
    assert!(!pending.contains("figures/x.png"));
    assert!(failed.contains("figures/x.png"));
}

#[test]
fn failed_figure_stays_skipped_across_repeated_dispatch_until_seam_clears_it() {
    // This is the storm regression: `dispatch_pending_figures` calls
    // `figure_already_handled` on every walk, including the
    // cached-bytes reflow passes (apply_text_scale, the reflow,
    // workspace-switch restore) that `render_preview_source` also
    // serves. None of those may un-skip a failed url — only clearing
    // `failed` (which only the fresh IncomingEvt::Preview seam does)
    // may. Simulating N reflow passes with `failed` untouched must
    // keep skipping; only an explicit clear (the seam) reopens it.
    let pending: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut failed: std::collections::HashSet<String> = std::collections::HashSet::new();
    failed.insert("figures/dead.png".to_string());
    for _ in 0..5 {
        assert!(
            figure_already_handled(false, &pending, &failed, "figures/dead.png"),
            "a terminally-failed url must stay skipped across repeated reflow passes"
        );
    }
    failed.clear(); // the one thing a genuine markdown reload does
    assert!(!figure_already_handled(
        false,
        &pending,
        &failed,
        "figures/dead.png"
    ));
}

// --- An SVG figure is drawn by the worker: its URL stays pending until the outcome, a refusal is terminal until a
// fresh markdown reply, a stale job is fetched again, and an outcome of a departed generation changes nothing. The
// draw is a stand-in that holds each job until released or stopped and answers `refused` for a job that says so. ---

mod worker {
    use super::super::super::figure_store::{FigureStore, Settled};
    use super::super::super::svg_draws::{Done, SvgDraws};
    use super::super::super::svg_run::{Outcome, Stop};
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const URL: &str = "figures/a.svg";

    fn draws(release: &Arc<AtomicBool>) -> SvgDraws {
        let release = release.clone();
        let draw = Arc::new(move |jobs: Vec<super::super::super::svg_run::Job>, stop: &Stop| {
            let begun = Instant::now();
            jobs.into_iter()
                .map(|job| {
                    let refused = job.svg.starts_with(b"refused");
                    while !refused && !release.load(SeqCst) && !stop.is_stopped() && begun.elapsed() < Duration::from_secs(20) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    if stop.is_stopped() {
                        Outcome::Stale
                    } else if refused {
                        Outcome::Refused("svg parse failed".into())
                    } else {
                        Outcome::Drawn { w: job.size.0, h: job.size.1, rgba: vec![0; 4] }
                    }
                })
                .collect()
        });
        SvgDraws::new(1 << 20, || {}, draw)
    }

    /// The next outcome the window would read at the top of a frame.
    fn next(draws: &mut SvgDraws) -> Done {
        let begun = Instant::now();
        loop {
            let mut got = None;
            draws.drain(|done| got = got.take().or(Some(done)));
            if let Some(done) = got {
                return done;
            }
            assert!(begun.elapsed() < Duration::from_secs(10), "no outcome arrived");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn pending_store(draws: &mut SvgDraws, svg: &[u8], generation: u64) -> FigureStore {
        let mut figures = FigureStore::default();
        figures.mark_pending(URL.into());
        assert!(figures.submit_svg(draws, URL.into(), generation, svg.to_vec()));
        figures
    }

    #[test]
    fn a_figure_stays_pending_until_its_drawing_settles() {
        let release = Arc::new(AtomicBool::new(false));
        let mut draws = draws(&release);
        let mut figures = pending_store(&mut draws, b"<svg/>", 1);
        std::thread::sleep(Duration::from_millis(30));
        assert!(figures.handled(URL), "a figure being drawn is not fetched again");
        assert!(figures.get(URL).is_none() && figures.metrics().is_empty(), "nothing is installed by the reply");
        release.store(true, SeqCst);
        let Settled::Drawn { w, h, .. } = figures.settle_drawing(next(&mut draws)) else { panic!("not drawn") };
        assert_eq!((w, h), (800, 600), "the figure is drawn onto 800 by 600");
        assert!(figures.handled(URL), "pending until the window installs it");
    }

    #[test]
    fn a_refused_figure_is_not_drawn_again_until_a_fresh_reply() {
        let release = Arc::new(AtomicBool::new(false));
        let mut draws = draws(&release);
        let mut figures = pending_store(&mut draws, b"refused", 1);
        assert!(matches!(figures.settle_drawing(next(&mut draws)), Settled::Failed(why) if why == "svg parse failed"));
        for _ in 0..3 {
            assert!(figures.handled(URL), "a refusal is terminal");
        }
        assert_eq!(figures.metrics().get(URL).map(|m| m.height_px), Some(0.0), "reported as a collapsed figure");
        figures.clear_failed();
        assert!(!figures.handled(URL), "a fresh markdown reply tries it again");
    }

    #[test]
    fn a_stale_figure_is_fetched_again_under_the_new_generation() {
        let release = Arc::new(AtomicBool::new(false));
        let mut draws = draws(&release);
        let mut figures = pending_store(&mut draws, b"<svg/>", 1);
        std::thread::sleep(Duration::from_millis(30));
        draws.advance(2);
        assert!(matches!(figures.settle_drawing(next(&mut draws)), Settled::Unhandled));
        assert!(!figures.handled(URL), "stale: unhandled, so the next walk fetches it");
        figures.mark_pending(URL.into());
        assert!(figures.submit_svg(&mut draws, URL.into(), 2, b"<svg/>".to_vec()));
        release.store(true, SeqCst);
        assert!(matches!(figures.settle_drawing(next(&mut draws)), Settled::Drawn { .. }), "drawn under generation 2");
    }

    #[test]
    fn a_late_stale_after_a_resubmit_does_not_undo_the_pending_job() {
        let release = Arc::new(AtomicBool::new(false));
        let mut draws = draws(&release);
        let mut figures = pending_store(&mut draws, b"<svg/>", 2);
        let late = |outcome| Done { key: URL.into(), generation: 1, outcome };
        assert!(matches!(figures.settle_drawing(late(Outcome::Stale)), Settled::Ignored));
        assert!(matches!(figures.settle_drawing(late(Outcome::Refused("x".into()))), Settled::Ignored));
        assert!(figures.handled(URL) && figures.metrics().is_empty(), "still pending, not failed");
        release.store(true, SeqCst);
        assert!(matches!(figures.settle_drawing(next(&mut draws)), Settled::Drawn { .. }), "generation 2 still draws");
    }

    #[test]
    fn a_figure_the_worker_will_not_take_fails_at_once() {
        let mut draws = SvgDraws::new(1, || {}, Arc::new(|_, _| Vec::new()));
        let mut figures = FigureStore::default();
        figures.mark_pending(URL.into());
        assert!(!figures.submit_svg(&mut draws, URL.into(), 1, b"<svg/>".to_vec()), "two bytes past a cap of one");
        assert!(figures.handled(URL) && figures.metrics().get(URL).is_some(), "failed, not pending");
    }
}
