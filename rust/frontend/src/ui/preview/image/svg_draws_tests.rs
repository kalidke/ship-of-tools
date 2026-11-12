use super::*;
use super::super::svg_wire::Ask;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A draw that holds each job until released or stopped. A job labeled `end...` ends at a bound at once, one labeled
/// `ok...` is drawn at once; the others are held.
#[derive(Default)]
struct Stand {
    seen: Mutex<Vec<String>>,
    /// The size of each batch the draw was given.
    calls: Mutex<Vec<usize>>,
    released: AtomicBool,
    now: AtomicUsize,
    most: AtomicUsize,
}

impl Stand {
    fn draw(&self, jobs: Vec<Job>, stop: &Stop) -> Vec<Outcome> {
        let mut outcomes = Vec::new();
        self.calls.lock().unwrap().push(jobs.len());
        for job in jobs {
            let label = String::from_utf8(job.svg.to_vec()).unwrap();
            self.seen.lock().unwrap().push(label.clone());
            self.most.fetch_max(self.now.fetch_add(1, SeqCst) + 1, SeqCst);
            let begun = Instant::now();
            assert_ne!(label, "boom", "a draw that panics");
            let held = !label.starts_with("end") && !label.starts_with("ok") && !label.starts_with("refused");
            while held && !self.released.load(SeqCst) && !stop.is_stopped() && begun.elapsed() < Duration::from_secs(20) {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.now.fetch_sub(1, SeqCst);
            outcomes.push(if stop.is_stopped() {
                Outcome::Stale
            } else if label.starts_with("end") {
                Outcome::Ended("took past 10 s".into())
            } else if label.starts_with("refused") {
                Outcome::Refused("svg parse failed".into())
            } else {
                Outcome::Drawn { w: 1, h: 1, rgba: vec![0; 4] }
            });
        }
        outcomes
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn release(&self) {
        self.released.store(true, SeqCst);
    }
}

fn job(label: &str) -> Job {
    Job { ask: Ask::Stretch(1, 1), svg: Arc::from(label.as_bytes()) }
}

fn draws_over(stand: &Arc<Stand>, cap: usize) -> (SvgDraws, Arc<AtomicUsize>) {
    let (wakes, stand) = (Arc::new(AtomicUsize::new(0)), stand.clone());
    let draw: Draw = Arc::new(move |jobs, stop| stand.draw(jobs, stop));
    let on_wake = wakes.clone();
    (SvgDraws::new(cap, move || { on_wake.fetch_add(1, SeqCst); }, draw), wakes)
}

/// Polls `until` for at most 10 s, with a message that says what never happened.
fn within(what: &str, mut until: impl FnMut() -> bool) {
    let begun = Instant::now();
    while !until() {
        assert!(begun.elapsed() < Duration::from_secs(10), "never happened: {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The next `count` outcomes, as `(key, outcome)`.
fn outcomes(draws: &mut SvgDraws, count: usize) -> Vec<(String, Outcome)> {
    let mut got = Vec::new();
    within(&format!("{count} outcomes"), || {
        draws.drain(|done| got.push((done.key, done.outcome)));
        got.len() >= count
    });
    got
}

#[test]
fn a_new_generation_starts_after_at_most_the_stale_run_is_killed() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 1 << 20);
    draws.advance(1);
    for n in 0..20 {
        draws.submit(format!("a{n}"), 1, job(&format!("a{n}"))).unwrap();
    }
    within("the first draw to start", || stand.seen() == ["a0"]);
    draws.advance(2);
    draws.submit("b".into(), 2, job("b")).unwrap();
    within("the new generation's draw to start", || stand.seen().last().is_some_and(|label| label == "b"));
    assert_eq!(stand.seen(), ["a0", "b"], "the waiting 19 were never drawn");
    stand.release();
    let got = outcomes(&mut draws, 21);
    // The stopped draw's own outcome comes when it returns, after the 19 flushed at the advance.
    let stale: Vec<_> = got.iter().filter(|(_, outcome)| *outcome == Outcome::Stale).map(|(key, _)| key.clone()).collect();
    assert_eq!(stale.len(), 20, "{got:?}");
    assert!((0..20).all(|n| stale.contains(&format!("a{n}"))), "{got:?}");
    assert!(matches!(&got[20], (key, Outcome::Drawn { .. }) if key == "b"), "{got:?}");
}

#[test]
fn a_stale_job_is_skipped_before_it_starts() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 1 << 20);
    draws.advance(1);
    draws.submit("a".into(), 1, job("a")).unwrap();
    draws.submit("b".into(), 1, job("b")).unwrap();
    within("the first draw to start", || stand.seen() == ["a"]);
    draws.advance(2);
    let mut got = outcomes(&mut draws, 2);
    got.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(got, [("a".to_owned(), Outcome::Stale), ("b".to_owned(), Outcome::Stale)]);
    assert_eq!(stand.seen(), ["a"], "b was never given to the draw");
}

#[test]
fn one_draw_runs_at_a_time() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 1 << 20);
    draws.advance(1);
    for key in ["a", "b", "c"] {
        draws.submit(key.into(), 1, job(key)).unwrap();
    }
    within("the first draw to start", || stand.seen() == ["a"]);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(stand.seen(), ["a"], "the others wait while one is held");
    stand.release();
    outcomes(&mut draws, 3);
    assert_eq!(stand.most.load(SeqCst), 1);
    assert_eq!(stand.seen(), ["a", "b", "c"]);
}

#[test]
fn a_submit_past_the_queued_bytes_is_refused() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 10);
    draws.advance(1);
    draws.submit("a".into(), 1, job("aaaaaa")).unwrap();
    within("the first draw to start", || stand.seen() == ["aaaaaa"]);
    draws.submit("b".into(), 1, job("bbbbbb")).unwrap();
    assert_eq!(draws.submit("c".into(), 1, job("cccccc")), Err("too many pictures waiting"));
    stand.release();
    let keys: Vec<_> = outcomes(&mut draws, 2).into_iter().map(|(key, _)| key).collect();
    assert_eq!(keys, ["a", "b"]);
    // What was refused left nothing counted: the room is back once the queue is empty.
    assert_eq!(draws.submit("d".into(), 1, job("okdddd")), Ok(()));
}

#[test]
fn after_two_strikes_a_generation_draws_no_more() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 1 << 20);
    draws.advance(1);
    for key in ["end0", "end1", "ok2"] {
        draws.submit(key.into(), 1, job(key)).unwrap();
    }
    let got = outcomes(&mut draws, 3);
    assert!(matches!(&got[0].1, Outcome::Ended(_)) && matches!(&got[1].1, Outcome::Ended(_)), "{got:?}");
    assert_eq!(got[2], ("ok2".to_owned(), Outcome::Refused("an earlier picture in this file was refused".into())));
    assert_eq!(stand.seen(), ["end0", "end1"], "the third was refused without a draw");
    // A later generation starts clean.
    draws.advance(2);
    draws.submit("ok3".into(), 2, job("ok3")).unwrap();
    assert!(matches!(outcomes(&mut draws, 1)[0].1, Outcome::Drawn { .. }));
}

#[test]
fn an_outcome_reaches_the_drain_and_wakes_once() {
    let stand = Arc::new(Stand::default());
    let (mut draws, wakes) = draws_over(&stand, 1 << 20);
    draws.advance(1);
    draws.submit("ok".into(), 1, job("ok")).unwrap();
    within("the wake", || wakes.load(SeqCst) > 0);
    let mut got = Vec::new();
    draws.drain(|done| got.push((done.key, done.generation, done.outcome)));
    assert_eq!(got, [("ok".to_owned(), 1, Outcome::Drawn { w: 1, h: 1, rgba: vec![0; 4] })]);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(wakes.load(SeqCst), 1, "one outcome, one wake");
}

#[test]
fn a_draw_that_panics_refuses_its_jobs_and_the_worker_goes_on() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 1 << 20);
    draws.advance(1);
    draws.submit("boom".into(), 1, job("boom")).unwrap();
    draws.submit("ok".into(), 1, job("ok")).unwrap();
    let got = outcomes(&mut draws, 2);
    assert_eq!(got[0], ("boom".to_owned(), Outcome::Refused("cannot start the drawing".into())));
    assert!(matches!(got[1].1, Outcome::Drawn { .. }), "{got:?}");
}

#[test]
fn a_stale_generations_queue_does_not_refuse_the_current_one() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 10);
    draws.advance(1);
    draws.submit("a".into(), 1, job("a")).unwrap();
    within("the first draw to start", || stand.seen() == ["a"]);
    draws.submit("b".into(), 1, job("bbbbbbbbb")).unwrap();
    draws.advance(2);
    assert_eq!(draws.submit("c".into(), 2, job("okc")), Ok(()), "the new source's first picture");
}

#[test]
fn a_job_of_a_newer_generation_than_the_frames_is_drawn_and_an_older_one_is_stale() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 1 << 20);
    draws.advance(1);
    draws.submit("new".into(), 2, job("okn")).unwrap();
    draws.submit("old".into(), 1, job("oko")).unwrap();
    let got = outcomes(&mut draws, 2);
    let of = |key: &str| &got.iter().find(|(k, _)| k == key).unwrap().1;
    assert!(matches!(of("new"), Outcome::Drawn { .. }), "{got:?}");
    assert_eq!(*of("old"), Outcome::Stale);
    assert_eq!(stand.seen(), ["okn"], "the older job was never given to the draw");
}

#[test]
fn the_first_submit_before_any_advance_is_drawn() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 1 << 20);
    draws.submit("ok".into(), 7, job("ok")).unwrap();
    assert!(matches!(outcomes(&mut draws, 1)[0].1, Outcome::Drawn { .. }));
}

#[test]
fn only_a_bounds_end_is_a_strike() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 1 << 20);
    draws.advance(1);
    for key in ["refused0", "refused1", "ok"] {
        draws.submit(key.into(), 1, job(key)).unwrap();
    }
    let got = outcomes(&mut draws, 3);
    assert!(matches!(got[0].1, Outcome::Refused(_)) && matches!(got[1].1, Outcome::Refused(_)), "{got:?}");
    assert!(matches!(got[2].1, Outcome::Drawn { .. }), "two refusals are not two strikes: {got:?}");
}

#[test]
fn a_draw_that_outlives_its_generation_is_stale_whatever_it_says() {
    let (gate, started) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
    let (held, begun) = (gate.clone(), started.clone());
    let draw: Draw = Arc::new(move |jobs: Vec<Job>, _stop: &Stop| {
        begun.fetch_add(1, SeqCst);
        while !held.load(SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        jobs.iter().map(|_| Outcome::Drawn { w: 1, h: 1, rgba: vec![0; 4] }).collect()
    });
    let mut draws = SvgDraws::new(1 << 20, || {}, draw);
    draws.advance(1);
    draws.submit("a".into(), 1, job("a")).unwrap();
    within("the draw to start", || started.load(SeqCst) == 1);
    draws.advance(2);
    gate.store(true, SeqCst);
    assert_eq!(outcomes(&mut draws, 1)[0].1, Outcome::Stale, "the draw ignored its stop and drew");
}

#[test]
fn dropping_the_draws_stops_the_draw_and_ends_the_worker() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 1 << 20);
    draws.advance(1);
    draws.submit("a".into(), 1, job("a")).unwrap();
    within("the first draw to start", || stand.seen() == ["a"]);
    drop(draws);
    // The held draw saw its stop, and the worker, which held the draw function, is gone with it.
    within("the held draw to end", || stand.now.load(SeqCst) == 0);
    within("the worker to end", || Arc::strong_count(&stand) == 1);
}

#[test]
fn the_window_never_waits_behind_a_slow_wake() {
    // The worker refuses a struck job and its third wake (this one) is held for 1.5 s; the window's own advance,
    // which takes the worker's lock, must not wait for it.
    let (stand, calls, gate) = (Arc::new(Stand::default()), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
    let draw: Draw = { let stand = stand.clone(); Arc::new(move |jobs, stop| stand.draw(jobs, stop)) };
    let (held, counted) = (gate.clone(), calls.clone());
    let mut draws = SvgDraws::new(1 << 20, move || {
        if counted.fetch_add(1, SeqCst) >= 2 {
            while !held.load(SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }, draw);
    draws.advance(1);
    for key in ["end0", "end1", "ok2"] {
        draws.submit(key.into(), 1, job(key)).unwrap();
    }
    within("the refusal's wake to be held", || calls.load(SeqCst) == 3);
    let opener = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        gate.store(true, SeqCst);
    });
    let begun = Instant::now();
    draws.advance(2);
    assert!(begun.elapsed() < Duration::from_millis(1000), "advance waited {:?} behind a wake", begun.elapsed());
    opener.join().unwrap();
}

fn batch(labels: &[&str]) -> Vec<(String, Job)> {
    labels.iter().map(|label| (label.to_string(), job(label))).collect()
}

#[test]
fn the_waiting_jobs_of_a_generation_that_batch_are_drawn_by_one_draw() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 1 << 20);
    draws.advance(1);
    draws.submit("a".into(), 1, job("a")).unwrap();
    within("the first draw to start", || stand.seen() == ["a"]);
    // Behind the held draw: a batch, a job that does not batch, and a second batch.
    draws.submit_batch(1, batch(&["ok1", "ok2", "ok3"])).unwrap();
    draws.submit("ok4".into(), 1, job("ok4")).unwrap();
    draws.submit_batch(1, batch(&["ok5", "ok6"])).unwrap();
    stand.release();
    let got = outcomes(&mut draws, 7);
    assert_eq!(*stand.calls.lock().unwrap(), [1, 5, 1], "one child for every waiting batched job, in order: {got:?}");
    assert_eq!(stand.seen(), ["a", "ok1", "ok2", "ok3", "ok5", "ok6", "ok4"]);
    assert!(got.iter().all(|(_, outcome)| matches!(outcome, Outcome::Drawn { .. })), "{got:?}");
}

#[test]
fn a_batch_that_ends_early_refuses_the_rest_in_its_words_and_strikes_once() {
    let words = "the drawing ended (exit status: 73)";
    // One child per call: the first job labeled `end` and every job after it end with the child.
    let draw: Draw = Arc::new(move |jobs: Vec<Job>, _stop: &Stop| {
        let mut over = false;
        jobs.iter()
            .map(|job| {
                over |= job.svg.starts_with(b"end");
                if over { Outcome::Ended(words.into()) } else { Outcome::Drawn { w: 1, h: 1, rgba: vec![0; 4] } }
            })
            .collect()
    });
    let mut draws = SvgDraws::new(1 << 20, || {}, draw);
    draws.advance(1);
    draws.submit_batch(1, batch(&["ok0", "end1", "ok2", "ok3"])).unwrap();
    let got = outcomes(&mut draws, 4);
    assert!(matches!(got[0].1, Outcome::Drawn { .. }), "{got:?}");
    assert!(got[1..].iter().all(|(_, outcome)| *outcome == Outcome::Ended(words.into())), "the rest end in the child's words: {got:?}");
    // Three jobs ended, but one child did: one strike, so the next draw is made.
    draws.submit("ok4".into(), 1, job("ok4")).unwrap();
    assert!(matches!(outcomes(&mut draws, 1)[0].1, Outcome::Drawn { .. }), "a batch is one strike, not one per job");
    draws.submit_batch(1, batch(&["end5", "ok6"])).unwrap();
    outcomes(&mut draws, 2);
    draws.submit_batch(1, batch(&["ok7", "ok8"])).unwrap();
    let got = outcomes(&mut draws, 2);
    let refusal = Outcome::Refused("an earlier picture in this file was refused".into());
    assert!(got.iter().all(|(_, outcome)| *outcome == refusal), "the second strike refuses the next batch whole: {got:?}");
}

#[test]
fn a_batch_past_the_queued_bytes_is_refused_whole() {
    let stand = Arc::new(Stand::default());
    let (mut draws, _) = draws_over(&stand, 10);
    draws.advance(1);
    draws.submit("a".into(), 1, job("a")).unwrap();
    within("the first draw to start", || stand.seen() == ["a"]);
    assert_eq!(draws.submit_batch(1, batch(&["okaaaa", "okbbbb", "okcccc"])), Err("too many pictures waiting"));
    // None of it was queued or counted: the room is whole.
    assert_eq!(draws.submit_batch(1, batch(&["okdd", "okee"])), Ok(()));
}
