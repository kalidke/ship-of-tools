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
    use super::super::super::figure_store::{FigureStore, Reply, Settled};
    use super::super::super::svg_draws::{Done, SvgDraws};
    use super::super::super::svg_run::{Job, Outcome, Stop};
    use super::super::super::svg_wire::Ask;
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const URL: &str = "figures/a.svg";
    const SVG: &str = "image/svg+xml";

    fn draws(release: &Arc<AtomicBool>) -> SvgDraws {
        let release = release.clone();
        let draw = Arc::new(move |jobs: Vec<Job>, stop: &Stop| {
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
                        let Ask::Stretch(w, h) = job.ask else { panic!("a figure is stretched") };
                        Outcome::Drawn { w, h, rgba: vec![0; 4] }
                    }
                })
                .collect()
        });
        SvgDraws::new(1 << 20, || {}, draw)
    }

    /// What the walk and the reply do: the URL goes pending, then the reply is routed.
    fn reply(figures: &mut FigureStore, draws: &mut SvgDraws, generation: u64, svg: &[u8]) -> Reply {
        figures.mark_pending(URL.into());
        figures.route_reply(draws, generation, URL.into(), SVG, svg.to_vec())
    }

    /// The frames until one settles a figure, as the window runs them; exactly one figure settles.
    fn frames(figures: &mut FigureStore, draws: &mut SvgDraws, generation: u64) -> Settled {
        let begun = Instant::now();
        loop {
            let mut settled = figures.settle_frame(draws, generation);
            if let Some((url, one)) = settled.pop() {
                assert!(settled.is_empty() && url == URL, "one figure settles, once");
                return one;
            }
            assert!(begun.elapsed() < Duration::from_secs(10), "no figure settled");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_figure_stays_pending_until_its_drawing_settles() {
        let release = Arc::new(AtomicBool::new(false));
        let mut draws = draws(&release);
        let mut figures = FigureStore::default();
        assert!(matches!(reply(&mut figures, &mut draws, 1, b"<svg/>"), Reply::Submitted), "an SVG is not decoded here");
        // The stand-in holds the job until released, so nothing has settled and nothing is installed.
        assert!(figures.settle_frame(&mut draws, 1).is_empty());
        assert!(figures.handled(URL), "a figure being drawn is not fetched again");
        assert!(figures.get(URL).is_none() && figures.metrics().is_empty(), "nothing is installed by the reply");
        release.store(true, SeqCst);
        let Settled::Drawn { w, h, .. } = frames(&mut figures, &mut draws, 1) else { panic!("not drawn") };
        assert_eq!((w, h), (800, 600), "the figure is drawn onto 800 by 600");
        assert!(figures.handled(URL), "pending until the window installs it");
    }

    #[test]
    fn a_raster_reply_settles_its_url_and_comes_back_to_decode() {
        let mut draws = draws(&Arc::new(AtomicBool::new(false)));
        let mut figures = FigureStore::default();
        figures.mark_pending(URL.into());
        let Reply::Raster { url, bytes } = figures.route_reply(&mut draws, 1, URL.into(), "image/png", vec![7]) else {
            panic!("a raster is for the window to decode")
        };
        assert_eq!((url.as_str(), bytes), (URL, vec![7]));
        assert!(!figures.handled(URL), "settled: the decode installs or fails it");
    }

    #[test]
    fn a_refused_figure_is_not_drawn_again_until_a_fresh_reply() {
        let release = Arc::new(AtomicBool::new(false));
        let mut draws = draws(&release);
        let mut figures = FigureStore::default();
        assert!(matches!(reply(&mut figures, &mut draws, 1, b"refused"), Reply::Submitted));
        assert!(matches!(frames(&mut figures, &mut draws, 1), Settled::Failed(why) if why == "svg parse failed"));
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
        let mut figures = FigureStore::default();
        assert!(matches!(reply(&mut figures, &mut draws, 1, b"<svg/>"), Reply::Submitted));
        // The pane moved on: the next frame tells the worker, whether the job was queued or running.
        assert!(matches!(frames(&mut figures, &mut draws, 2), Settled::Unhandled));
        assert!(!figures.handled(URL), "stale: unhandled, so the next walk fetches it");
        assert!(matches!(reply(&mut figures, &mut draws, 2, b"<svg/>"), Reply::Submitted));
        release.store(true, SeqCst);
        assert!(matches!(frames(&mut figures, &mut draws, 2), Settled::Drawn { .. }), "drawn under generation 2");
    }

    #[test]
    fn a_late_stale_after_a_resubmit_does_not_undo_the_pending_job() {
        let release = Arc::new(AtomicBool::new(false));
        let mut draws = draws(&release);
        let mut figures = FigureStore::default();
        assert!(matches!(reply(&mut figures, &mut draws, 2, b"<svg/>"), Reply::Submitted));
        // The departed generation's outcomes, handed to the store after the resubmit.
        for outcome in [Outcome::Stale, Outcome::Refused("x".into())] {
            let late = Done { key: URL.into(), generation: 1, outcome };
            assert!(matches!(figures.settle_drawing(late), Settled::Ignored));
        }
        assert!(figures.handled(URL) && figures.metrics().is_empty(), "still pending, not failed");
        release.store(true, SeqCst);
        assert!(matches!(frames(&mut figures, &mut draws, 2), Settled::Drawn { .. }), "generation 2 still draws");
    }

    #[test]
    fn a_figure_the_worker_will_not_take_fails_at_once() {
        let mut draws = SvgDraws::new(1, || {}, Arc::new(|_, _| Vec::new()));
        let mut figures = FigureStore::default();
        assert!(matches!(reply(&mut figures, &mut draws, 1, b"<svg/>"), Reply::Failed), "past a cap of one byte");
        assert!(figures.handled(URL) && figures.metrics().get(URL).is_some(), "failed, not pending");
    }
}
