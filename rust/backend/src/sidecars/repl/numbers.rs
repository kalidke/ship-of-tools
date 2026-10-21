//! A REPL run's number. The daemon numbers every run on every REPL from one counter (`next`), so no two runs share a
//! number in any window. A window that numbered a run itself (its eval and its `r`/`R`, from its own count) keeps that
//! number for the run (`CallerNumbers`, one per window, which the client roster keeps across the window's connections:
//! `clients_window.rs`): each connection of the window is written the run's frames under the window's number and is
//! never announced the run, and every other connection gets the daemon's number after the run's `started` frame. A run
//! the daemon refused is written to its window not at all: the reply already said why, unless a hello had named the run. `Repl::submit` marks each
//! of a window's runs running once the REPL's queue accepts the run, and the supervisor marks it ended before it
//! publishes the run's `done` (`accepted`, `end_then`), so each hello reply names the window's runs still running,
//! those waiting for the queue included (`running`, `HelloRes::repl_running`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::broadcast;

use super::ReplFrameMsg;

/// The next run number. Numbers start at 2^40, above any window's own count, and stay under 2^53, which a JSON or `jq`
/// reader holds exactly (ADR 0033 item 7).
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 40);

/// A number no other run of this daemon has.
pub(crate) fn next() -> u64 {
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// How many of its own runs a window remembers. Past this many, a run already `done` goes first, oldest first, then
/// the oldest: only a window with this many runs in flight at once loses one.
const REMEMBERED: usize = 256;

/// One of a window's own runs: the number the window gave it (`None` once the daemon refused it unnamed), whether a
/// hello has named it, and how far it has gone. The run is kept past its end because each connection of the window writes its `done`, an older connection
/// still winding down included, and each must find the run.
#[derive(Clone, Copy)]
struct Own {
    number: Option<u64>,
    /// A hello named the run while it was still `Numbered`: the reply of a refusal then went to a connection that
    /// may be gone, so the window that was named the run is written its reason and `done`.
    named: bool,
    stage: Stage,
}

/// How far a window's run has gone, as `Repl::submit` and the REPL's supervisor mark it. A hello names a `Numbered`
/// or a `Running` run, never an `Ended` one: its `done` is still to be published, after the hello's connection subscribed to the frame bus, so the window that reopens the
/// run's entry is written its end.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Numbered, not yet accepted: the REPL may still refuse it (a full queue, a child that will not start).
    Numbered,
    /// Accepted into the REPL's queue (`accepted`), written to the child or waiting for it to start; its `done` is not
    /// yet published.
    Running,
    /// Its `done` is published or about to be (`end_then`, marked first), or the daemon refused it.
    Ended,
}

/// One window's own runs, by the daemon's number.
#[derive(Clone, Default)]
pub(crate) struct CallerNumbers(Arc<Mutex<BTreeMap<u64, Own>>>);

impl CallerNumbers {
    /// Numbers a run this window submits. `own` is the number the window gave it, if it gave one. A window's number
    /// names one run: a run it numbered the same before is forgotten (a window relaunched under the same `instance`
    /// counts from 1 again, `scripts/launch-sot.ps1`), and its later frames reach the window as any other run's.
    pub(crate) fn number(&self, own: Option<u64>) -> u64 {
        let number = next();
        if let Some(own) = own {
            let mut runs = self.runs();
            runs.retain(|_, run| run.number != Some(own));
            runs.insert(number, Own { number: Some(own), named: false, stage: Stage::Numbered });
            if runs.len() > REMEMBERED {
                let ended = runs.iter().find(|(_, run)| run.stage == Stage::Ended);
                let gone = ended.or(runs.iter().next()).map(|(n, _)| *n);
                if let Some(n) = gone {
                    runs.remove(&n);
                }
            }
        }
        number
    }

    /// The daemon refused this window's run `number` (its queue was full, or it could not start a REPL): the reply
    /// tells the window why, so none of the run's frames is written to it, unless a hello named the run first: that
    /// window never had the reply, and is written the run's reason and `done`.
    pub(crate) fn refused(&self, number: u64) {
        if let Some(run) = self.runs().get_mut(&number) {
            if !run.named {
                run.number = None;
            }
            run.stage = Stage::Ended;
        }
    }

    /// The REPL's queue accepted this window's run `number`: the supervisor owns it from here and closes it out
    /// (`end_then`) however its child ends, so its `done` is still to come. A run already ended stays ended: the
    /// supervisor may finish it before the submitter marks it.
    pub(crate) fn accepted(&self, number: u64) {
        if let Some(run) = self.runs().get_mut(&number) {
            if run.stage == Stage::Numbered {
                run.stage = Stage::Running;
            }
        }
    }

    /// This window's run `number` has ended: it is marked ended, then `publish` puts the run's `done` on the frame bus,
    /// so a hello read after the `done` was published never names the run.
    pub(crate) fn end_then(&self, number: u64, publish: impl FnOnce()) {
        self.mark(number, Stage::Ended);
        publish();
    }

    /// The window's own numbers of its runs still running, oldest first: what a hello reply names. A run still
    /// waiting for the REPL's queue is named too (a window that redials during the REPL's start, up to about 15 s, would
    /// never be told of a run accepted after its hello); it is marked named, so a refusal reaches the window. `subscribed`
    /// is the reader's own subscription to the frame bus, taken first, so each run named has its `done` still to come on it.
    pub(crate) fn running(&self, _subscribed: &broadcast::Receiver<ReplFrameMsg>) -> Vec<u64> {
        let mut runs = self.runs();
        let mut named = Vec::new();
        for run in runs.values_mut().filter(|run| run.stage != Stage::Ended) {
            run.named |= run.stage == Stage::Numbered;
            named.extend(run.number);
        }
        named
    }

    fn mark(&self, number: u64, stage: Stage) {
        if let Some(run) = self.runs().get_mut(&number) {
            run.stage = stage;
        }
    }

    /// A frame as a connection of this window is written it: a run of its own under its own number, and never its
    /// `started` (its own entry already stands for the run), nor any frame of a run the daemon refused it; every other
    /// run as the daemon numbered it.
    pub(crate) fn outgoing(&self, mut msg: ReplFrameMsg) -> Option<ReplFrameMsg> {
        let Some(run) = self.runs().get(&msg.eval_id).copied() else {
            return Some(msg);
        };
        let own = run.number?;
        match msg.frame.get("kind").and_then(|k| k.as_str()) {
            Some("started") => return None,
            Some("done") => {
                if let Some(id) = msg.frame.get_mut("eval_id") {
                    *id = own.into();
                }
            }
            _ => {}
        }
        msg.eval_id = own;
        Some(msg)
    }

    /// The daemon's numbers for the runs an interrupt names in this window's numbers. A number that is none of its
    /// own runs stays as it is: an announced run is named by the daemon's number.
    pub(crate) fn daemon_numbers(&self, named: &[u64]) -> Vec<u64> {
        let runs = self.runs();
        let mine = |n: u64| runs.iter().find(|(_, run)| run.number == Some(n)).map(|(number, _)| *number);
        named.iter().map(|n| mine(*n).unwrap_or(*n)).collect()
    }

    fn runs(&self) -> MutexGuard<'_, BTreeMap<u64, Own>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
#[path = "numbers_tests.rs"]
mod tests;
