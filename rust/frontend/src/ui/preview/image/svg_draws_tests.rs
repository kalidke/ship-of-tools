use super::*;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A draw that holds each job until released or stopped. A job labeled `end...` ends at a bound at once, one labeled
/// `ok...` is drawn at once; the others are held.
#[derive(Default)]
struct Stand {
    seen: Mutex<Vec<String>>,
    released: AtomicBool,
    now: AtomicUsize,
    most: AtomicUsize,
}

impl Stand {
    fn draw(&self, jobs: Vec<Job>, stop: &Stop) -> Vec<Outcome> {
        let mut outcomes = Vec::new();
        for job in jobs {
            let label = String::from_utf8(job.svg.to_vec()).unwrap();
            self.seen.lock().unwrap().push(label.clone());
            self.most.fetch_max(self.now.fetch_add(1, SeqCst) + 1, SeqCst);
            let begun = Instant::now();
            assert_ne!(label, "boom", "a draw that panics");
            let held = !label.starts_with("end") && !label.starts_with("ok");
            while held && !self.released.load(SeqCst) && !stop.is_stopped() && begun.elapsed() < Duration::from_secs(20) {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.now.fetch_sub(1, SeqCst);
            outcomes.push(if stop.is_stopped() {
                Outcome::Stale
            } else if label.starts_with("end") {
                Outcome::Ended("took past 10 s".into())
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
    Job { size: (1, 1), svg: Arc::from(label.as_bytes()) }
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
    assert!(got[..20].iter().enumerate().all(|(n, (key, outcome))| *key == format!("a{n}") && *outcome == Outcome::Stale), "{got:?}");
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
    let got = outcomes(&mut draws, 2);
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
