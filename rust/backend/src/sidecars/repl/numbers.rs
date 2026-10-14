//! A REPL run's number. The daemon numbers every run on every REPL from one counter (`next`), so no two runs share a
//! number in any window. A caller that numbered a run itself (a window's eval and its `r`/`R`, from its own count)
//! keeps that number on its own connection (`CallerNumbers`): that connection is written the run's frames under the
//! caller's number and is never announced the run, and every other connection gets the daemon's number after the run's
//! `started` frame. A run the daemon refused is written to its caller not at all: the reply already said why.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use super::ReplFrameMsg;

/// The next run number. Numbers start at 2^40, above any window's own count, and stay under 2^53, which a JSON or `jq`
/// reader holds exactly (ADR 0033 item 7).
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 40);

/// A number no other run of this daemon has.
pub(crate) fn next() -> u64 {
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// How many of its own runs a connection remembers. A run leaves at its `done`, so only a run whose `done` the
/// connection missed (its frame bus lagged) stays; past this many, the oldest goes.
const REMEMBERED: usize = 256;

/// One connection's own runs: the daemon's number of each, and the number the connection gave it, or `None` once the
/// daemon refused the run.
#[derive(Clone, Default)]
pub(crate) struct CallerNumbers(Arc<Mutex<BTreeMap<u64, Option<u64>>>>);

impl CallerNumbers {
    /// Numbers a run this connection submits. `own` is the number the connection gave it, if it gave one.
    pub(crate) fn number(&self, own: Option<u64>) -> u64 {
        let number = next();
        if let Some(own) = own {
            let mut runs = self.runs();
            runs.insert(number, Some(own));
            while runs.len() > REMEMBERED {
                runs.pop_first();
            }
        }
        number
    }

    /// The daemon refused this connection's run `number` (its queue was full, or it could not start a REPL): the reply
    /// tells the connection why, so none of the run's frames is written to it.
    pub(crate) fn refused(&self, number: u64) {
        if let Some(own) = self.runs().get_mut(&number) {
            *own = None;
        }
    }

    /// A frame as this connection is written it: a run of its own under its own number, until the run's `done`, and
    /// never its `started` (its own entry already stands for the run), nor any frame of a run the daemon refused it;
    /// every other run as the daemon numbered it.
    pub(crate) fn outgoing(&self, mut msg: ReplFrameMsg) -> Option<ReplFrameMsg> {
        let mut runs = self.runs();
        let Some(&own) = runs.get(&msg.eval_id) else {
            return Some(msg);
        };
        let kind = msg.frame.get("kind").and_then(|k| k.as_str());
        if kind == Some("done") {
            runs.remove(&msg.eval_id);
        }
        let own = own?;
        match kind {
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

    /// The daemon's numbers for the runs an interrupt names in this connection's numbers. A number that is none of
    /// its own runs stays as it is: an announced run is named by the daemon's number.
    pub(crate) fn daemon_numbers(&self, named: &[u64]) -> Vec<u64> {
        let runs = self.runs();
        let mut out = Vec::new();
        for n in named {
            let before = out.len();
            out.extend(runs.iter().filter(|(_, own)| **own == Some(*n)).map(|(number, _)| *number));
            if out.len() == before {
                out.push(*n);
            }
        }
        out
    }

    fn runs(&self) -> MutexGuard<'_, BTreeMap<u64, Option<u64>>> {
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

    /// Two connections that both call their run 1 get distinct daemon numbers; each is written its own run under 1,
    /// without its `started`, and the other's run under the daemon's number, `started` first.
    #[test]
    fn each_connection_sees_its_own_run_under_its_own_number_and_the_others_under_the_daemons() {
        let (a, b) = (CallerNumbers::default(), CallerNumbers::default());
        let (na, nb) = (a.number(Some(1)), b.number(Some(1)));
        assert!(na >= 1 << 40 && nb >= 1 << 40 && na != nb, "{na} {nb}");
        let started = |n| msg(n, json!({ "kind": "started", "run_id": format!("exec-{n}"), "origin": "window", "display": "x" }));
        assert_eq!(kind_and_id(a.outgoing(started(na))), None, "a connection is never announced its own run");
        assert_eq!(kind_and_id(b.outgoing(started(na))), Some((na, "started".into())));
        let out = msg(na, json!({ "kind": "stdout", "text": "from a" }));
        assert_eq!(kind_and_id(a.outgoing(out.clone())), Some((1, "stdout".into())));
        assert_eq!(kind_and_id(b.outgoing(out)), Some((na, "stdout".into())));
        let done = msg(na, json!({ "kind": "done", "eval_id": na, "elapsed_ms": 1 }));
        let mine = a.outgoing(done.clone()).expect("a's own done");
        assert_eq!((mine.eval_id, mine.frame["eval_id"].clone()), (1, json!(1)), "the done names the caller's number too");
        assert_eq!(b.outgoing(done).expect("b sees a's done").eval_id, na);
        let late = msg(na, json!({ "kind": "stdout", "text": "after done" }));
        assert_eq!(kind_and_id(a.outgoing(late)), Some((na, "stdout".into())), "the run left at its done");
        assert_eq!(kind_and_id(b.outgoing(msg(nb, json!({ "kind": "stdout", "text": "from b" })))), Some((1, "stdout".into())));
    }

    /// A run the daemon refused its caller is written to that caller not at all, its `done` included (the reply said
    /// why), and to every other connection as any run is.
    #[test]
    fn a_refused_run_is_written_to_its_caller_not_at_all() {
        let (a, b) = (CallerNumbers::default(), CallerNumbers::default());
        let n = a.number(Some(4));
        a.refused(n);
        for frame in [
            json!({ "kind": "started", "run_id": format!("exec-{n}"), "origin": "window", "display": "x" }),
            json!({ "kind": "error", "message": "the REPL's queue is full", "stacktrace": [] }),
            json!({ "kind": "done", "eval_id": n, "elapsed_ms": 0 }),
        ] {
            assert_eq!(kind_and_id(a.outgoing(msg(n, frame.clone()))), None, "{frame}");
            assert_eq!(b.outgoing(msg(n, frame)).map(|m| m.eval_id), Some(n));
        }
        assert!(a.runs().is_empty(), "the refused run left at its done");
    }

    /// An interrupt names a connection's own runs by its numbers and announced runs by the daemon's; both reach the
    /// REPL as the daemon's numbers, and a number of no run stays as it is.
    #[test]
    fn an_interrupt_names_the_daemons_numbers() {
        let a = CallerNumbers::default();
        let mine = a.number(Some(7));
        let announced = next();
        assert_eq!(a.daemon_numbers(&[7, announced, 3]), vec![mine, announced, 3]);
    }

    /// A connection that misses the `done` of its runs keeps at most `REMEMBERED` of them, the newest.
    #[test]
    fn a_connection_remembers_a_bounded_number_of_runs() {
        let a = CallerNumbers::default();
        let first = a.number(Some(1));
        for own in 2..=(REMEMBERED as u64 + 1) {
            a.number(Some(own));
        }
        assert_eq!(a.runs().len(), REMEMBERED);
        assert!(!a.runs().contains_key(&first), "the oldest run went first");
        assert!(a.number(None) > first);
        assert_eq!(a.runs().len(), REMEMBERED, "a run the connection did not number is not remembered");
    }
}
