// The window's exit dispatch: what a close, a Quit or a relaunch request does, given the leave in progress. lease.rs
// declares it and re-exports it, so callers name it as `crate::lease::exit_intent`. Part of lifecycle; charter:
// rust/backend/src/lifecycle/CLAUDE.md.

use sot_protocol::ops::LeaveIntent;

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
/// leave in progress.
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
