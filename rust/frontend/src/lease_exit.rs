// The window's exit decision: what a close, a Quit or a relaunch request does, given the leave in progress, and
// whether a leave stays open instead (`Leases::stay`). lease.rs declares it and re-exports it, so callers name it as
// `crate::lease::exit_intent`. Part of lifecycle; charter: rust/backend/src/lifecycle/CLAUDE.md.

use std::time::Instant;

use sot_protocol::ops::LeaveIntent;

use super::{Leases, Leaving};

/// Why the window is being asked to exit.
#[derive(Clone, Copy)]
pub(crate) enum ExitReason {
    WindowClose,
    QuitKey,
    Relaunch(i32),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExitStep {
    Ask,
    Leave { intent: LeaveIntent, code: i32 },
    Supersede,
    Now { code: i32 },
    Ignore,
}

/// What an exit request does. A window already leaving exits at once, with
/// 0, on a second close, except that an X or OS close during a Keep
/// supersedes it with a Close: the user's latest intent wins, so a second
/// close during a Handover never relaunches. A relaunch then defers to the
/// leave in progress. `request_quit` first offers a Ctrl+Q's `Now` to the
/// stay (`Leases::stay`).
pub(crate) fn exit_intent(reason: ExitReason, leaving: Option<LeaveIntent>) -> ExitStep {
    match (reason, leaving) {
        (ExitReason::WindowClose, None) => ExitStep::Leave { intent: LeaveIntent::Close, code: 0 },
        (ExitReason::QuitKey, None) => ExitStep::Ask,
        (ExitReason::Relaunch(code), None) => ExitStep::Leave { intent: LeaveIntent::Handover, code },
        (ExitReason::WindowClose, Some(LeaveIntent::Keep)) => ExitStep::Supersede,
        (ExitReason::WindowClose | ExitReason::QuitKey, Some(_)) => ExitStep::Now { code: 0 },
        (ExitReason::Relaunch(_), Some(_)) => ExitStep::Ignore,
    }
}

/// A second close exits at once with `code` (0), and the leave in progress
/// takes that code: winit still runs `about_to_wait` while it shuts down, and
/// its poll exits with the leave's own code, so no restart code outlives a
/// close.
pub(crate) fn close_now(leaving: Option<&mut crate::lease::Leaving>, code: i32) -> i32 {
    if let Some(l) = leaving {
        l.exit_code = code;
    }
    code
}

impl Leases {
    /// Confirms a leave's stay (`LeaveStep::Stay`, or a Ctrl+Q during the wait) under the lock `install` takes, so no
    /// grant can take the leave between the decision and the stay. True when a handshake in flight is still
    /// unsettled: the leave ends, a late grant installs with no leave behind it, the next handshake is taken, and
    /// `notice` names the stay until the next leave. False when the last one settled first: it took the leave, and
    /// the wait goes on.
    pub(crate) fn stay(&self, leaving: &mut Leaving, now: Instant) -> bool {
        let mut book = self.book.lock().unwrap();
        leaving.settle(now);
        if !leaving.must_stay() {
            return false;
        }
        book.leaving = None;
        book.late = None;
        book.stayed = Some(leaving.intent);
        tracing::warn!(intent = ?leaving.intent, "window lease: a request is still unanswered; the window stays open");
        true
    }
}

impl Leaving {
    /// A Keep or a Handover with a lease handshake in flight that has not settled: exiting would drop a request the
    /// daemon may still grant, and that grant would depart as a Close.
    pub(super) fn must_stay(&self) -> bool {
        self.late.is_some() && self.unsettled > 0 && self.intent != LeaveIntent::Close
    }
}

/// What `notice` says after a leave stayed, and what to do next; a Close never stays.
pub(super) fn stay_line(intent: LeaveIntent) -> &'static str {
    if intent == LeaveIntent::Handover {
        "relaunch not done: this computer's backend did not answer the window lease in time, so the window stayed \
         open; ask for the relaunch again"
    } else {
        "keep not done: this computer's backend did not answer the window lease in time, so the window stayed open \
         and the sessions run on; Ctrl+Q then Yes tries again"
    }
}
