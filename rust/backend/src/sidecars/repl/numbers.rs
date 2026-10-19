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
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(eval_id: u64, frame: serde_json::Value) -> ReplFrameMsg {
        ReplFrameMsg { eval_id, workspace_id: Some("row".into()), frame, opener: Default::default() }
    }

    fn kind_and_id(m: Option<ReplFrameMsg>) -> Option<(u64, String)> {
        m.map(|m| (m.eval_id, m.frame["kind"].as_str().unwrap_or("").to_string()))
    }

    /// A frame-bus subscription to read `running` against.
    fn bus() -> broadcast::Receiver<ReplFrameMsg> {
        broadcast::channel(1).1
    }

    fn done(n: u64) -> ReplFrameMsg {
        msg(n, json!({ "kind": "done", "eval_id": n, "elapsed_ms": 1 }))
    }

    /// Two windows that both call their run 1 get distinct daemon numbers; each is written its own run under 1,
    /// without its `started`, and the other's run under the daemon's number, `started` first.
    #[test]
    fn each_window_sees_its_own_run_under_its_own_number_and_the_others_under_the_daemons() {
        let (a, b) = (CallerNumbers::default(), CallerNumbers::default());
        let (na, nb) = (a.number(Some(1)), b.number(Some(1)));
        assert!(na >= 1 << 40 && nb >= 1 << 40 && na != nb, "{na} {nb}");
        let started = |n| msg(n, json!({ "kind": "started", "run_id": format!("exec-{n}"), "origin": "window", "display": "x" }));
        assert_eq!(kind_and_id(a.outgoing(started(na))), None, "a window is never announced its own run");
        assert_eq!(kind_and_id(b.outgoing(started(na))), Some((na, "started".into())));
        let out = msg(na, json!({ "kind": "stdout", "text": "from a" }));
        assert_eq!(kind_and_id(a.outgoing(out.clone())), Some((1, "stdout".into())));
        assert_eq!(kind_and_id(b.outgoing(out)), Some((na, "stdout".into())));
        let mine = a.outgoing(done(na)).expect("a's own done");
        assert_eq!((mine.eval_id, mine.frame["eval_id"].clone()), (1, json!(1)), "the done names the window's number too");
        assert_eq!(b.outgoing(done(na)).expect("b sees a's done").eval_id, na);
        assert_eq!(kind_and_id(b.outgoing(msg(nb, json!({ "kind": "stdout", "text": "from b" })))), Some((1, "stdout".into())));
    }

    /// Each connection of a window writes the run's `done`: one that writes it first (a new connection, or an old one
    /// winding down) leaves the run for the others, so each writes it under the window's number.
    #[test]
    fn every_connection_of_a_window_writes_its_runs_done_under_its_number() {
        let window = CallerNumbers::default();
        let (old, new) = (window.clone(), window.clone());
        let n = window.number(Some(3));
        for conn in [&old, &new] {
            let written = conn.outgoing(done(n)).expect("the done");
            assert_eq!((written.eval_id, written.frame["eval_id"].clone()), (3, json!(3)));
        }
    }

    /// A window's number names one run: a later run it numbers the same (a window relaunched under the same instance
    /// counts from 1 again) takes the number, and the earlier run's frames reach it under the daemon's number.
    #[test]
    fn a_windows_number_names_one_run() {
        let window = CallerNumbers::default();
        let before = window.number(Some(1));
        let after = window.number(Some(1));
        let out = |n| msg(n, json!({ "kind": "stdout", "text": "x" }));
        assert_eq!(kind_and_id(window.outgoing(out(after))), Some((1, "stdout".into())));
        assert_eq!(kind_and_id(window.outgoing(out(before))), Some((before, "stdout".into())));
        assert_eq!(window.daemon_numbers(&[1]), vec![after]);
    }

    /// A run the daemon refused its window is written to that window not at all, its `done` included (the reply said
    /// why), and to every other window as any run is.
    #[test]
    fn a_refused_run_is_written_to_its_window_not_at_all() {
        let (a, b) = (CallerNumbers::default(), CallerNumbers::default());
        let n = a.number(Some(4));
        a.refused(n);
        for frame in [
            json!({ "kind": "started", "run_id": format!("exec-{n}"), "origin": "window", "display": "x" }),
            json!({ "kind": "error", "message": "the REPL's queue is full", "stacktrace": [] }),
            json!({ "kind": "done", "eval_id": n, "elapsed_ms": 0 }),
            json!({ "kind": "stdout", "text": "after its done" }),
        ] {
            assert_eq!(kind_and_id(a.outgoing(msg(n, frame.clone()))), None, "{frame}");
            assert_eq!(b.outgoing(msg(n, frame)).map(|m| m.eval_id), Some(n));
        }
    }

    /// A hello names a window's run from its numbering to its end: not once refused (before a hello named it), not
    /// once ended, and never a run the window did not number.
    #[test]
    fn a_window_names_its_runs_only_while_they_run() {
        let (a, bus) = (CallerNumbers::default(), bus());
        let (one, two, three) = (a.number(Some(1)), a.number(Some(2)), a.number(Some(3)));
        let theirs = a.number(None);
        assert_eq!(a.running(&bus), [1, 2, 3], "numbered, not accepted: named, for a window that redials during the start");
        for n in [one, two, theirs] {
            a.accepted(n);
        }
        a.refused(three);
        a.accepted(three);
        assert_eq!(a.running(&bus), [1, 2], "accepted, not yet written to the child: its done still to come");
        a.end_then(one, || {});
        a.accepted(one);
        assert_eq!(a.running(&bus), [2], "ended, and accepting it late does not reopen it");
        assert_eq!(kind_and_id(a.outgoing(done(one))), Some((1, "done".into())), "an ended run is still the window's");
    }

    /// A hello that reads a run while it is still waiting for the REPL's queue names it: the window redials during the
    /// REPL's start and the run is accepted after. If the queue then refuses it, the reply went to the dropped
    /// connection, so the run's reason and `done` are written to the window that was named it.
    #[test]
    fn a_run_named_while_waiting_and_then_refused_is_written_its_end() {
        let (a, bus) = (CallerNumbers::default(), bus());
        let named = a.number(Some(1));
        assert_eq!(a.running(&bus), [1], "a numbered run is named");
        let unnamed = a.number(Some(2));
        a.refused(unnamed);
        assert_eq!(a.running(&bus), [1], "a run refused before any hello is named by none");
        a.refused(named);
        assert!(a.running(&bus).is_empty());
        let error = msg(named, json!({ "kind": "error", "message": "no REPL", "stacktrace": [] }));
        assert_eq!(kind_and_id(a.outgoing(error)), Some((1, "error".into())));
        assert_eq!(kind_and_id(a.outgoing(done(named))), Some((1, "done".into())));
        assert_eq!(kind_and_id(a.outgoing(done(unnamed))), None, "never named: the reply said why");
    }

    /// A run is marked ended before its `done` is published: from inside `publish` a hello already names it no more.
    #[test]
    fn a_run_is_marked_ended_before_its_done_is_published() {
        let (a, bus) = (CallerNumbers::default(), bus());
        let n = a.number(Some(1));
        a.accepted(n);
        assert_eq!(a.running(&bus), [1]);
        let mut named_at_publish = None;
        a.end_then(n, || named_at_publish = Some(a.running(&bus)));
        assert_eq!(named_at_publish, Some(vec![]), "the run was still named when its done was published");
    }

    /// An interrupt names a window's own runs by its numbers and announced runs by the daemon's; both reach the
    /// REPL as the daemon's numbers, and a number of no run stays as it is.
    #[test]
    fn an_interrupt_names_the_daemons_numbers() {
        let a = CallerNumbers::default();
        let mine = a.number(Some(7));
        let announced = next();
        assert_eq!(a.daemon_numbers(&[7, announced, 3]), vec![mine, announced, 3]);
    }

    /// A window keeps at most `REMEMBERED` of its runs: a finished one goes first, then the oldest.
    #[test]
    fn a_window_remembers_a_bounded_number_of_runs_finished_ones_going_first() {
        let a = CallerNumbers::default();
        let running = a.number(Some(1));
        let finished = a.number(Some(2));
        a.end_then(finished, || {});
        for own in 3..=(REMEMBERED as u64 + 1) {
            a.number(Some(own));
        }
        assert_eq!(a.runs().len(), REMEMBERED);
        assert!(!a.runs().contains_key(&finished), "the finished run went first");
        assert!(a.runs().contains_key(&running), "the run still going stays");
        a.number(Some(REMEMBERED as u64 + 2));
        assert!(!a.runs().contains_key(&running), "then the oldest");
        assert!(a.number(None) > running);
        assert_eq!(a.runs().len(), REMEMBERED, "a run the window did not number is not remembered");
    }
}
