//! The interrupt signal for an eval that never yields: one ordinary signal per request, the forcing burst only on a
//! forced request.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::anyhow;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::lifecycle::child_signal::Contained;

/// How long the child must say nothing, with an interrupt unanswered, before the first signal, and after the last
/// before the daemon gives the answer itself.
const QUIET: Duration = Duration::from_secs(1);
/// The gap between the signals of a forced interrupt.
const BURST_GAP: Duration = Duration::from_millis(50);
/// How long a forced interrupt's signals go on, from the first. Julia forces the exception into code with no safepoint
/// at the fifth signal it counts close together (each adds 0.3 to a sum that decays with a one-second time constant,
/// and the first only starts the count; when an earlier signal started it, the fourth forces). After forcing it ignores
/// signals for half a second, and one after that is a new interrupt, held by the shim outside a user's code and
/// dropped, so its answer for the forced eval stays whole (julia/repl `held_task`). On macOS and Windows Julia counts
/// each signal on a thread of its own, so the burst forces by its fourth or fifth signal, and a burst that ends 0.4 s after
/// its first signal ends inside the half second after any force in it. On Linux the eval's own thread counts each one
/// in its signal handler, so the signals sent while that thread waits for a CPU merge into one count: on a loaded
/// computer five signals count as two or three, and the signals go on so that the thread counts one each time it runs.
/// A second force needs a signal sent 850 ms or more after the first, so they force at most once; a child that forced
/// by 0.25 s and then wrote nothing for half a second still takes the last of them as new interrupts.
const FORCE_SPAN: Duration = Duration::from_millis(if cfg!(any(target_os = "macos", windows)) { 400 } else { 800 });

/// An unanswered `repl.interrupt` the child says nothing through. An eval that never yields holds the shim's one
/// thread, so the shim cannot read the request (julia/repl `handle_interrupt`): once the child has said nothing for
/// `QUIET`, it is sent one interrupt signal (`Contained::interrupt`), which Julia raises at the eval's next safepoint
/// and holds through C code, compiling and garbage collection, as the stock REPL's Ctrl-C does. A forced request
/// (`force: true`) sends them `BURST_GAP` apart for `FORCE_SPAN`, which Julia forces into code with no safepoint, and
/// into C code, the compiler and the collector too, where it can crash or hang the REPL. Any line from the child
/// restarts the quiet, since the shim ran to write it, and one after a signal ends the signals; the interrupt's answer
/// ends the escalation. When the signals are over and `QUIET` has passed with no answer, the daemon answers the request
/// itself, saying what is left.
///
/// It signals only the eval the child runs (`for_request`, `targets`), only while it is in flight: never an idle REPL,
/// which a signal could end with what it defined, never an eval the request did not name, never after the eval's `done`.
pub(super) struct Escalation {
    /// The interrupt's wire id: its answer ends the escalation.
    id: u64,
    /// The eval the signals are for: the one the child was running when the interrupt was written.
    eval: u64,
    force: bool,
    /// When the next step goes, if the child says nothing until then.
    at: Instant,
    /// When the first signal went; unset until then.
    first: Option<Instant>,
    /// Whether the signals are over: a plain request's one is sent, a forced request's span has passed, or the child
    /// said something after one.
    over: bool,
}

/// What an escalation does at its time.
pub(super) enum Next {
    /// The escalation goes on: a signal went, or the quiet after the last one runs.
    Again(Escalation),
    /// The request is answered with this error: the signals are sent and it is still unanswered, or none could be sent.
    GiveUp(u64, anyhow::Error),
}

impl Escalation {
    /// The escalation for an interrupt request (wire `id`, `payload`) written while the child ran `running`, the first
    /// eval written and still in flight; `None` when no eval runs or the request names others (`eval_ids`).
    pub(super) fn for_request(id: u64, payload: &Value, running: Option<u64>) -> Option<Self> {
        let eval = running?;
        let named = payload.get("eval_ids").and_then(Value::as_array);
        if named.is_some_and(|named| !named.iter().any(|n| n.as_u64() == Some(eval))) {
            return None;
        }
        let force = payload.get("force").and_then(Value::as_bool).unwrap_or(false);
        Some(Self { id, eval, force, at: Instant::now() + QUIET, first: None, over: false })
    }

    /// The escalation to keep when a request has made `new` while `old` runs: the first, except that a forced request
    /// for the same eval replaces a plain one (a second Ctrl+C a moment after the first must force).
    pub(super) fn prefer(old: Option<Self>, new: Option<Self>) -> Option<Self> {
        match (old, new) {
            (Some(old), Some(new)) if new.force && !old.force && new.eval == old.eval => Some(new),
            (Some(old), _) => Some(old),
            (None, new) => new,
        }
    }

    /// Whether `running`, the first eval in flight, is still the one the signals are for.
    pub(super) fn targets(&self, running: Option<&u64>) -> bool {
        running == Some(&self.eval)
    }

    /// The child wrote a line: the escalation ends if that answered it, and otherwise waits a full quiet again. A line
    /// after a signal means the eval's thread runs Julia code again, so no signal follows it.
    pub(super) fn heard(mut self, pending: &HashMap<u64, oneshot::Sender<anyhow::Result<Value>>>) -> Option<Self> {
        self.at = Instant::now() + QUIET;
        self.over |= self.first.is_some();
        pending.contains_key(&self.id).then_some(self)
    }

    /// When the next step goes.
    pub(super) fn at(&self) -> Instant {
        self.at
    }

    /// Send the next signal, or answer when they are over.
    pub(super) fn step(mut self, contained: &Contained) -> Next {
        let now = Instant::now();
        if self.over {
            return Next::GiveUp(self.id, anyhow!(unanswered(self.force)));
        }
        if self.first.is_some_and(|first| now >= first + FORCE_SPAN) {
            // The span ended while this step waited: no signal, and the quiet runs from here.
            self.over = true;
            self.at = now + QUIET;
            return Next::Again(self);
        }
        if let Err(e) = contained.interrupt() {
            tracing::warn!(error = %e, "repl interrupt signal not sent");
            return Next::GiveUp(self.id, anyhow!(not_sent(&e.to_string())));
        }
        self.signalled(now);
        Next::Again(self)
    }

    /// A signal went at `now`: whether it was the last, and when the next step goes.
    fn signalled(&mut self, now: Instant) {
        let first = *self.first.get_or_insert(now);
        self.over = !self.force || now + BURST_GAP >= first + FORCE_SPAN;
        self.at = now + if self.over { QUIET } else { BURST_GAP };
    }
}

const RESTART: &str = "`--fresh` restarts it (`sot-fe repl run <ws> <file> --fresh`, or `r` on a .jl file in the window)";

/// The answer when the signals are sent and the eval has not stopped.
pub(super) fn unanswered(force: bool) -> String {
    if force {
        format!(
            "the eval went on through the forced interrupt too (it catches InterruptException, or a C call holds the \
             thread, or the computer is under heavy CPU load, where forcing may not stop it); {RESTART}"
        )
    } else {
        format!(
            "the eval did not stop at the interrupt signal: code with no safepoint, and C code, compiling and garbage \
             collection, take it only when they return (wait for that call to return, or force it). A second Ctrl+C in the window, or `sot-fe repl interrupt \
             --force`, forces it; forcing inside C code (BLAS), while compiling or while collecting garbage can crash \
             or hang the REPL, and {RESTART}"
        )
    }
}

/// The answer when no signal could be sent.
fn not_sent(error: &str) -> String {
    format!("no interrupt signal could be sent to the REPL ({error}), so its eval was not interrupted; {RESTART}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The rule: an escalation exists only for a running eval the request names (or names none), and then only while
    /// that eval is the first in flight.
    #[test]
    fn it_signals_only_the_running_eval_the_request_names() {
        let cases = [
            (json!({}), None, false),
            (json!({}), Some(7), true),
            (json!({ "eval_ids": [7] }), Some(7), true),
            (json!({ "eval_ids": [3, 7] }), Some(7), true),
            (json!({ "eval_ids": [99] }), Some(7), false),
            (json!({ "eval_ids": [] }), Some(7), false),
            (json!({ "eval_ids": [7] }), None, false),
        ];
        for (payload, running, expected) in cases {
            assert_eq!(Escalation::for_request(1, &payload, running).is_some(), expected, "{payload} {running:?}");
        }
        let e = Escalation::for_request(1, &json!({}), Some(7)).unwrap();
        assert!(e.targets(Some(&7)) && !e.targets(Some(&8)) && !e.targets(None));
    }

    /// A forced request for the same eval replaces a plain escalation; nothing else replaces one.
    #[test]
    fn a_forced_request_for_the_same_eval_replaces_a_plain_escalation() {
        let make = |id, eval, force: bool| Escalation::for_request(id, &json!({ "force": force }), Some(eval));
        let kept = |old, new| Escalation::prefer(old, new).map(|e| e.id);
        assert_eq!(kept(make(1, 7, false), make(2, 7, true)), Some(2));
        assert_eq!(kept(make(1, 7, false), make(2, 7, false)), Some(1));
        assert_eq!(kept(make(1, 7, true), make(2, 7, true)), Some(1));
        assert_eq!(kept(make(1, 7, false), make(2, 8, true)), Some(1));
        assert_eq!(kept(make(1, 7, false), None), Some(1));
        assert_eq!(kept(None, make(2, 7, true)), Some(2));
        assert_eq!(kept(None, None), None);
    }

    #[test]
    fn force_is_read_from_the_request_and_defaults_to_off() {
        assert!(!Escalation::for_request(1, &json!({}), Some(7)).unwrap().force);
        assert!(!Escalation::for_request(1, &json!({ "force": false }), Some(7)).unwrap().force);
        assert!(Escalation::for_request(1, &json!({ "force": true }), Some(7)).unwrap().force);
    }

    /// A forced request's signals go `BURST_GAP` apart while less than `FORCE_SPAN` has passed since the first, and end
    /// at the child's first line after one; a plain request's one signal is its last, and a line before any signal ends
    /// none.
    #[test]
    fn a_forced_burst_ends_at_its_span_or_at_the_childs_first_line() {
        let pending = HashMap::from([(1, oneshot::channel::<anyhow::Result<Value>>().0)]);
        let t0 = Instant::now();
        let mut forced = Escalation::for_request(1, &json!({ "force": true }), Some(7)).unwrap();
        let mut signals = 0;
        while !forced.over {
            forced.signalled(t0 + BURST_GAP * signals);
            signals += 1;
        }
        let (expected, what) = if cfg!(any(target_os = "macos", windows)) {
            (8, "signals at 0, 50, ..., 350 ms")
        } else {
            (16, "signals at 0, 50, ..., 750 ms")
        };
        assert_eq!(signals, expected, "{what}");
        assert_eq!(forced.at, t0 + BURST_GAP * (expected - 1) + QUIET, "the quiet runs from the last");
        let mut plain = Escalation::for_request(1, &json!({}), Some(7)).unwrap();
        plain.signalled(t0);
        assert!(plain.over && plain.at == t0 + QUIET);
        let mut heard = Escalation::for_request(1, &json!({ "force": true }), Some(7)).unwrap().heard(&pending).unwrap();
        assert!(!heard.over, "a line before any signal ends none");
        heard.signalled(t0);
        assert!(!heard.over);
        assert!(heard.heard(&pending).unwrap().over, "a line after a signal ends the signals");
    }

    /// Where Julia counts each signal on a thread of its own (macOS, Windows), any signal of the burst can be the one
    /// it forces at, so the burst ends 0.1 s or more inside the half second Julia ignores signals after a force: no
    /// signal of it is a new interrupt that lands in the shim's answer.
    #[test]
    fn where_julia_counts_every_signal_the_burst_ends_inside_its_half_second() {
        if cfg!(any(target_os = "macos", windows)) {
            assert!(FORCE_SPAN + Duration::from_millis(100) <= Duration::from_millis(500), "{FORCE_SPAN:?}");
        }
        assert!(FORCE_SPAN >= BURST_GAP * 5, "five signals at least, the count Julia forces at: {FORCE_SPAN:?}");
    }

    #[test]
    fn each_answer_says_what_is_left() {
        let plain = unanswered(false);
        assert!(plain.contains("second Ctrl+C") && plain.contains("--force") && plain.contains("crash"), "{plain}");
        assert!(plain.contains("--fresh"));
        let forced = unanswered(true);
        assert!(forced.contains("forced") && forced.contains("--fresh") && !forced.contains("second Ctrl+C"), "{forced}");
        assert!(forced.contains("heavy CPU load"), "{forced}");
        let none = not_sent("Access is denied");
        assert!(none.contains("no interrupt signal could be sent") && none.contains("Access is denied"), "{none}");
        assert!(none.contains("--fresh"));
    }
}
