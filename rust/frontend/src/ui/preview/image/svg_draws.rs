//! The drawing worker: the one thread that owns the window's SVG draws. Each job carries the generation of the preview
//! pane's source (`InstalledPreviewSource::generation`); the worker draws one batch at a time, skips a job whose
//! generation is not current when its turn comes, stops a running draw whose generation goes stale (that job is
//! `Outcome::Stale`, and its owner submits it again under the new generation), refuses past `QUEUED_BYTES` of input
//! waiting, and after `SVG_STRIKES` draws of one generation that ended at a bound refuses the rest of it without a
//! child. Outcomes go back on a channel the window drains at the top of each frame, each followed by one wake. The draw
//! is a function the worker is given (`draw_in_own_child` in the window), so its tests use a stand-in.

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use sot_protocol::ir::PREVIEW_BINARY_CAP;

use super::svg_run::{Job, Outcome, Stop};

/// Input waiting at once: what the child would take in one request.
pub(crate) const QUEUED_BYTES: usize = PREVIEW_BINARY_CAP;
/// Draws of one generation that may end at a bound before the rest of it is refused.
pub(crate) const SVG_STRIKES: u32 = 2;
const TOO_MANY: &str = "too many pictures waiting";
const AFTER_STRIKES: &str = "an earlier picture in this file was refused";
const NO_START: &str = "cannot start the drawing";

/// One job's end, for the owner of `key` under `generation`.
#[cfg_attr(not(test), expect(dead_code, reason = "the figures, the first to read an outcome, are the next landing"))]
pub(crate) struct Done {
    pub key: String,
    pub generation: u64,
    pub outcome: Outcome,
}

/// One draw of a batch of jobs of one generation (a batch of one for now), and its off switch.
pub(crate) type Draw = Arc<dyn Fn(Vec<Job>, &Stop) -> Vec<Outcome> + Send + Sync>;
type Wake = Arc<dyn Fn() + Send + Sync>;

struct Entry {
    key: String,
    generation: u64,
    job: Job,
}

enum Msg {
    Job(Entry),
    Advance(u64),
    Drew(Vec<Outcome>),
    Quit,
}

/// The window's end of the worker, owned by the State. The worker starts at the first submit; dropping this ends it
/// without waiting for it.
pub(crate) struct SvgDraws {
    draw: Draw,
    wake: Wake,
    cap: usize,
    generation: u64,
    waiting: Arc<AtomicUsize>,
    done: (Sender<Done>, Receiver<Done>),
    inbox: Option<Sender<Msg>>,
}

impl SvgDraws {
    pub(crate) fn new(cap: usize, wake: impl Fn() + Send + Sync + 'static, draw: Draw) -> Self {
        Self { draw, wake: Arc::new(wake), cap, generation: 0, waiting: Arc::default(), done: mpsc::channel(), inbox: None }
    }

    /// The pane's source is now `generation`; a draw of any other is stopped and every waiting job of it skipped.
    pub(crate) fn advance(&mut self, generation: u64) {
        if generation != self.generation {
            self.generation = generation;
            if let Some(inbox) = &self.inbox {
                let _ = inbox.send(Msg::Advance(generation));
            }
        }
    }

    /// Queues `job` for `key` under `generation`, or says why not. Each key is submitted once per generation by its
    /// owner; a refusal is terminal for it.
    #[cfg_attr(not(test), expect(dead_code, reason = "the figures, the first submitters, are the next landing"))]
    pub(crate) fn submit(&mut self, key: String, generation: u64, job: Job) -> Result<(), &'static str> {
        let len = job.svg.len();
        if self.waiting.load(Relaxed).saturating_add(len) > self.cap {
            return Err(TOO_MANY);
        }
        let inbox = self.started().ok_or(NO_START)?;
        self.waiting.fetch_add(len, Relaxed);
        inbox.send(Msg::Job(Entry { key, generation, job })).map_err(|_| NO_START)
    }

    fn started(&mut self) -> Option<Sender<Msg>> {
        if self.inbox.is_none() {
            let (tx, rx) = mpsc::channel();
            let worker = Worker {
                generation: self.generation,
                queue: VecDeque::new(),
                running: None,
                strikes: (0, 0),
                waiting: self.waiting.clone(),
                done: self.done.0.clone(),
                wake: self.wake.clone(),
                draw: self.draw.clone(),
                inbox: tx.clone(),
            };
            std::thread::Builder::new().spawn(move || worker.run(rx)).ok()?;
            self.inbox = Some(tx);
        }
        self.inbox.clone()
    }

    /// Every outcome that has arrived, in order; the top of each frame.
    pub(crate) fn drain(&mut self, mut each: impl FnMut(Done)) {
        while let Ok(done) = self.done.1.try_recv() {
            each(done);
        }
    }
}

impl Drop for SvgDraws {
    fn drop(&mut self) {
        if let Some(inbox) = &self.inbox {
            let _ = inbox.send(Msg::Quit);
        }
    }
}

/// The batch being drawn: its generation, the keys of its jobs in order, and its off switch.
struct Running {
    generation: u64,
    keys: Vec<String>,
    stop: Stop,
}

struct Worker {
    generation: u64,
    queue: VecDeque<Entry>,
    running: Option<Running>,
    /// The generation that has struck, and how many of its draws ended at a bound.
    strikes: (u64, u32),
    waiting: Arc<AtomicUsize>,
    done: Sender<Done>,
    wake: Wake,
    draw: Draw,
    /// The helper's way back: the worker waits on its inbox alone, so a new generation and a finished draw wake it alike.
    inbox: Sender<Msg>,
}

impl Worker {
    fn run(mut self, inbox: Receiver<Msg>) {
        while let Ok(msg) = inbox.recv() {
            match msg {
                Msg::Job(entry) => self.queue.push_back(entry),
                Msg::Advance(generation) => {
                    self.generation = generation;
                    if let Some(running) = self.running.as_ref().filter(|running| running.generation != generation) {
                        running.stop.stop();
                    }
                }
                Msg::Drew(outcomes) => self.settle(outcomes),
                Msg::Quit => {
                    if let Some(running) = &self.running {
                        running.stop.stop();
                    }
                    return;
                }
            }
            self.pump();
        }
    }

    /// Starts the next job that is current and not refused, giving each other its outcome at once.
    fn pump(&mut self) {
        while self.running.is_none() {
            let Some(entry) = self.queue.pop_front() else { return };
            self.waiting.fetch_sub(entry.job.svg.len(), Relaxed);
            if entry.generation != self.generation {
                self.report(entry.key, entry.generation, Outcome::Stale);
            } else if self.strikes.0 == entry.generation && self.strikes.1 >= SVG_STRIKES {
                self.report(entry.key, entry.generation, Outcome::Refused(AFTER_STRIKES.to_owned()));
            } else {
                self.begin(vec![entry]);
            }
        }
    }

    /// Draws `batch`, jobs of one generation, on a thread of its own, so the worker stays awake to a new generation.
    fn begin(&mut self, batch: Vec<Entry>) {
        let generation = batch[0].generation;
        let (keys, jobs): (Vec<_>, Vec<_>) = batch.into_iter().map(|entry| (entry.key, entry.job)).unzip();
        let (stop, draw, inbox) = (Stop::default(), self.draw.clone(), self.inbox.clone());
        let helper_stop = stop.clone();
        // A draw that panics, or a thread the OS refuses, answers with no outcomes, which `settle` refuses.
        let spawned = std::thread::Builder::new().spawn(move || {
            let outcomes = catch_unwind(AssertUnwindSafe(|| draw(jobs, &helper_stop))).unwrap_or_default();
            let _ = inbox.send(Msg::Drew(outcomes));
        });
        if spawned.is_err() {
            let _ = self.inbox.send(Msg::Drew(Vec::new()));
        }
        self.running = Some(Running { generation, keys, stop });
    }

    fn settle(&mut self, outcomes: Vec<Outcome>) {
        let Some(Running { generation, keys, .. }) = self.running.take() else { return };
        let mut outcomes = outcomes.into_iter();
        for key in keys {
            let outcome = outcomes.next().unwrap_or_else(|| Outcome::Refused(NO_START.to_owned()));
            self.report(key, generation, outcome);
        }
    }

    /// An outcome of a generation that is no longer current is `Stale`, whatever the draw said; one that ended at a
    /// bound is a strike against its generation.
    fn report(&mut self, key: String, generation: u64, outcome: Outcome) {
        let outcome = if generation != self.generation {
            Outcome::Stale
        } else {
            if matches!(outcome, Outcome::Ended(_)) {
                self.strikes = (generation, if self.strikes.0 == generation { self.strikes.1 + 1 } else { 1 });
            }
            outcome
        };
        let _ = self.done.send(Done { key, generation, outcome });
        (self.wake)();
    }
}

#[cfg(test)]
#[path = "svg_draws_tests.rs"]
mod tests;
