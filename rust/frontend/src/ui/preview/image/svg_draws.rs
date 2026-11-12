//! The drawing worker: the one thread that owns the window's SVG draws. Each job carries the generation of the preview
//! pane's source (`InstalledPreviewSource::generation`). The queue holds only the current generation: `advance` (the
//! window's thread, which never waits on the worker) hands every waiting job of an older one back as `Outcome::Stale`
//! at once and stops the running draw, so a hostile file's queue neither holds memory nor counts against the next
//! file's `QUEUED_BYTES`. The worker draws one batch at a time on its own thread and, after `SVG_STRIKES` draws of one
//! generation that ended at a bound, refuses the rest of it without a child. Outcomes go back on a channel the window
//! drains at the top of each frame (an outcome of a generation no longer current is `Stale` there), each of the
//! worker's followed by one wake. The draw is a function the worker is given (`draw_in_own_child` in the window), so
//! its tests use a stand-in.

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

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

/// What the window's thread and the worker share, under one lock. The queue holds the current generation alone, and a
/// running draw is always of it, so `advance` stops whatever runs.
#[derive(Default)]
struct Held {
    queue: VecDeque<Entry>,
    /// The bytes of SVG in `queue`.
    waiting: usize,
    running: Option<Stop>,
    quit: bool,
}

#[derive(Default)]
struct Shared {
    held: Mutex<Held>,
    ready: Condvar,
}

impl Shared {
    fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The window's end of the worker, owned by the State. The worker starts at the first submit; dropping this stops its
/// draw and ends it without waiting for it.
pub(crate) struct SvgDraws {
    draw: Draw,
    wake: Wake,
    cap: usize,
    /// The newest generation told to it, which is the pane's.
    generation: u64,
    shared: Arc<Shared>,
    done: (Sender<Done>, Receiver<Done>),
    started: bool,
}

impl SvgDraws {
    pub(crate) fn new(cap: usize, wake: impl Fn() + Send + Sync + 'static, draw: Draw) -> Self {
        Self { draw, wake: Arc::new(wake), cap, generation: 0, shared: Arc::default(), done: mpsc::channel(), started: false }
    }

    /// The pane's source is now `generation`, if that is newer: the jobs waiting for an older one are `Stale` at once
    /// and a draw running for it is stopped.
    pub(crate) fn advance(&mut self, generation: u64) {
        if generation <= self.generation {
            return;
        }
        self.generation = generation;
        let mut held = self.shared.held();
        held.waiting = 0;
        for entry in held.queue.drain(..) {
            let _ = self.done.0.send(Done { key: entry.key, generation: entry.generation, outcome: Outcome::Stale });
        }
        if let Some(stop) = &held.running {
            stop.stop();
        }
    }

    /// Queues `job` for `key` under `generation` (the pane's, which a submitter has just read, so a newer one than the
    /// frame's last `advance` becomes current here), or says why not. A job of an older generation is `Stale` at once.
    /// Each key is submitted once per generation by its owner; a refusal is terminal for it.
    #[cfg_attr(not(test), expect(dead_code, reason = "the figures, the first submitters, are the next landing"))]
    pub(crate) fn submit(&mut self, key: String, generation: u64, job: Job) -> Result<(), &'static str> {
        self.advance(generation);
        if generation < self.generation {
            let _ = self.done.0.send(Done { key, generation, outcome: Outcome::Stale });
            return Ok(());
        }
        self.start()?;
        let len = job.svg.len();
        let mut held = self.shared.held();
        if held.waiting.saturating_add(len) > self.cap {
            return Err(TOO_MANY);
        }
        held.waiting += len;
        held.queue.push_back(Entry { key, generation, job });
        drop(held);
        self.shared.ready.notify_one();
        Ok(())
    }

    fn start(&mut self) -> Result<(), &'static str> {
        if !self.started {
            let worker = Worker { shared: self.shared.clone(), done: self.done.0.clone(), wake: self.wake.clone(), draw: self.draw.clone() };
            std::thread::Builder::new().spawn(move || worker.run()).map_err(|_| NO_START)?;
            self.started = true;
        }
        Ok(())
    }

    /// Every outcome that has arrived, in order, the top of each frame; one of a generation that is no longer the
    /// pane's is `Stale`, whatever its draw said.
    pub(crate) fn drain(&mut self, mut each: impl FnMut(Done)) {
        while let Ok(mut done) = self.done.1.try_recv() {
            if done.generation != self.generation {
                done.outcome = Outcome::Stale;
            }
            each(done);
        }
    }
}

impl Drop for SvgDraws {
    fn drop(&mut self) {
        let mut held = self.shared.held();
        held.quit = true;
        if let Some(stop) = &held.running {
            stop.stop();
        }
        drop(held);
        self.shared.ready.notify_all();
    }
}

struct Worker {
    shared: Arc<Shared>,
    done: Sender<Done>,
    wake: Wake,
    draw: Draw,
}

impl Worker {
    fn run(self) {
        // The generation that has struck, and how many of its draws ended at a bound.
        let mut strikes = (0u64, 0u32);
        loop {
            let (entry, stop, struck) = {
                let mut held = self.shared.held();
                loop {
                    if held.quit {
                        return;
                    }
                    if let Some(entry) = held.queue.pop_front() {
                        held.waiting -= entry.job.svg.len();
                        let struck = strikes.0 == entry.generation && strikes.1 >= SVG_STRIKES;
                        let stop = Stop::default();
                        if !struck {
                            held.running = Some(stop.clone());
                        }
                        break (entry, stop, struck);
                    }
                    held = self.shared.ready.wait(held).unwrap_or_else(PoisonError::into_inner);
                }
            };
            // The lock is never held across a call out (the wake), so the window's thread never waits behind one.
            if struck {
                self.report(entry.key, entry.generation, Outcome::Refused(AFTER_STRIKES.to_owned()));
                continue;
            }
            // A batch of one: a later landing pops the jobs of one generation that wait together.
            let (generation, keys, jobs) = (entry.generation, vec![entry.key], vec![entry.job]);
            // A draw that panics answers with no outcomes, which are refused below.
            let mut outcomes = catch_unwind(AssertUnwindSafe(|| (self.draw)(jobs, &stop))).unwrap_or_default().into_iter();
            self.shared.held().running = None;
            for key in keys {
                let outcome = outcomes.next().unwrap_or_else(|| Outcome::Refused(NO_START.to_owned()));
                if matches!(outcome, Outcome::Ended(_)) {
                    strikes = (generation, if strikes.0 == generation { strikes.1 + 1 } else { 1 });
                }
                self.report(key, generation, outcome);
            }
        }
    }

    fn report(&self, key: String, generation: u64, outcome: Outcome) {
        let _ = self.done.send(Done { key, generation, outcome });
        (self.wake)();
    }
}

#[cfg(test)]
#[path = "svg_draws_tests.rs"]
mod tests;
