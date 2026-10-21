//! The quit prompt and leaving: the Ctrl+Q prompt's key table, the one quit dispatcher, the exit path.

use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(in crate::ui) enum QuitPromptStep {
    Stay { keep: bool },
    Leave(LeaveIntent),
    Cancel,
    Ignore,
}

/// The prompt consumes logical keys without consulting configurable actions, except that the quit chord itself
/// (`quit_again`, resolved by the caller from the bindings) and a modifier pressed alone change nothing: a second
/// Ctrl+Q, which is a Ctrl press and then a Q, leaves the question open instead of cancelling and reopening it.
pub(in crate::ui) fn quit_prompt_step(keep: bool, key: &Key, repeat: bool, quit_again: bool) -> QuitPromptStep {
    let step = if repeat || quit_again {
        QuitPromptStep::Ignore
    } else {
        match key {
            Key::Named(NamedKey::Control | NamedKey::Shift | NamedKey::Alt | NamedKey::Super | NamedKey::Meta
                | NamedKey::AltGraph) => QuitPromptStep::Ignore,
            Key::Named(NamedKey::Tab) => QuitPromptStep::Stay { keep: !keep },
            Key::Named(NamedKey::Enter) => QuitPromptStep::Leave(if keep { LeaveIntent::Keep } else { LeaveIntent::Close }),
            _ => QuitPromptStep::Cancel,
        }
    };
    tracing::info!(?step, "quit prompt: key");
    step
}

/// The quit question, and how its keys answer it (`quit_prompt_step`).
pub(in crate::ui) const QUIT_QUESTION: &str = "Keep the daemon and sessions running?";
pub(in crate::ui) const QUIT_KEYS: &str = "Tab switches \u{b7} Enter confirms \u{b7} any other key cancels";

/// The quit prompt as its text and its choice; the choice ends the prompt
/// (`nav_pinned_rows` keeps it whole on the last row).
pub(in crate::ui) fn quit_prompt_line(keep: bool) -> (String, String) {
    let choice = if keep { "No  [Yes]" } else { "[No]  Yes" };
    (format!("{QUIT_QUESTION}  {QUIT_KEYS}"), choice.to_string())
}

/// Whether the redraw that set `should_exit` ends the event loop. A window
/// that is leaving exits only from `about_to_wait`'s poll, with its own
/// exit code once the acks are in. The capture harness never leases and
/// `about_to_wait` returns early for it, so its exit is here.
pub(in crate::ui) fn redraw_exits(should_exit: bool, leaving: bool, harness: bool) -> bool {
    should_exit && (harness || !leaving)
}

/// The status when Ctrl+Q finds another prompt open: a typed name or entry is never discarded for the question.
pub(in crate::ui) const QUIT_REFUSED: &str = "quit · finish or cancel the open prompt first";

/// Opens the question unless another prompt is open, which it never overwrites (a file name typed in the tree stays
/// when focus has moved to another pane). False when it refused.
fn open_quit_prompt(prompt: &mut Option<NavPrompt>, keep: bool) -> bool {
    if matches!(prompt, Some(p) if !matches!(p, NavPrompt::ConfirmQuit { .. })) {
        tracing::info!("quit prompt: refused, another prompt is open");
        return false;
    }
    *prompt = Some(NavPrompt::ConfirmQuit { keep });
    tracing::info!(keep, "quit prompt: open");
    true
}

/// How long final teardown may take after the terminal decision before the process ends itself.
const BACKSTOP: std::time::Duration = std::time::Duration::from_secs(3);

/// The window's one process deadline. The first `arm` fixes the code and starts a std thread that ends the
/// process with it after `BACKSTOP` on the OS clock, whatever the UI thread or the transport runtime is doing.
#[derive(Clone)]
pub(in crate::ui) struct ExitDeadline {
    code: Arc<std::sync::OnceLock<i32>>,
    #[cfg(test)]
    inert: bool,
}

impl ExitDeadline {
    pub(in crate::ui) fn new() -> Self {
        Self { code: Arc::new(std::sync::OnceLock::new()), #[cfg(test)] inert: false }
    }

    /// Records the code and, the first time only, starts the backstop thread.
    pub(in crate::ui) fn arm(&self, code: i32) {
        if self.code.set(code).is_err() {
            return;
        }
        #[cfg(test)]
        if self.inert {
            return;
        }
        let start = std::time::Instant::now();
        // The thread's event must reach the sink the arming thread logs to.
        let dispatch = tracing::dispatcher::get_default(|d| d.clone());
        let spawned = std::thread::Builder::new().name("sot-exit-backstop".into()).spawn(move || {
            while let Some(left) = BACKSTOP.checked_sub(start.elapsed()) {
                std::thread::sleep(left);
            }
            tracing::dispatcher::with_default(&dispatch, || {
                tracing::error!(code, "window: final teardown exceeded its bound; exiting");
            });
            std::process::exit(code);
        });
        if let Err(e) = spawned {
            // Without the thread the bound cannot hold: end now rather than risk a hang.
            eprintln!("sot: cannot start the exit backstop ({e}); exiting with {code}");
            std::process::exit(code);
        }
    }
}

#[cfg(test)]
impl ExitDeadline {
    /// Records codes but starts no thread, for tests that return from a loop and keep running.
    pub(in crate::ui) fn inert() -> Self {
        Self { code: Arc::new(std::sync::OnceLock::new()), inert: true }
    }
}

/// A test holds teardown at the named stage (`SOT_TEST_T1_BARRIER`) so only the backstop can end the process.
#[cfg(test)]
pub(in crate::ui) fn barrier(stage: &str) {
    if std::env::var("SOT_TEST_T1_BARRIER").is_ok_and(|s| s == stage) {
        println!("barrier entered: {stage}");
        std::io::Write::flush(&mut std::io::stdout()).ok();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
}

/// A terminal close (a second request while leaving): resolve the final code, arm the deadline, then give a leave
/// already queued (a Close after a Keep) its bounded write before the runtime and its streams go.
pub(in crate::ui) fn terminal_close(deadline: &ExitDeadline, leases: &crate::lease::Leases,
    leaving: Option<&mut crate::lease::Leaving>, code: i32) -> i32 {
    let code = close_now(leaving, code);
    deadline.arm(code);
    #[cfg(test)]
    barrier("delivery");
    leases.deliver_queued(crate::lease::LEAVE_WRITE_WAIT);
    code
}

/// The window's last act: arm the deadline, then end the loop. A nonzero code (a relaunch) ends the process at once.
pub(in crate::ui) fn end_loop(deadline: &ExitDeadline, code: i32, end: impl FnOnce()) {
    deadline.arm(code);
    if code != 0 {
        #[cfg(windows)]
        allow_next_foreground();
        #[cfg(test)]
        barrier("direct");
        std::process::exit(code);
    }
    end();
}

#[derive(Debug, PartialEq, Eq)]
enum LeaveEffect { Redraw, Finish(i32) }

/// Only leases and exit-state slots are available to this transition.
fn begin_leave(leases: &crate::lease::Leases, prompt: &mut Option<NavPrompt>, leaving: &mut Option<crate::lease::Leaving>,
    should_exit: &mut bool, intent: LeaveIntent, code: i32, now: std::time::Instant) -> LeaveEffect {
    tracing::info!(?intent, code, "window: leaving");
    *prompt = None;
    *leaving = leases.leave_all(intent, code, now);
    *should_exit = true;
    if leaving.is_some() { LeaveEffect::Redraw } else { LeaveEffect::Finish(code) }
}

/// A leave's stay (`LeaveStep::Stay`, or a Ctrl+Q during its wait), over `begin_leave`'s exit slots and the status
/// line: when `Leases::stay` confirms it, the leave ends and the window stays open, its lease notice saying why, and
/// the status is cleared, since a relaunch command's "relaunching…" would contradict that notice (true); otherwise a
/// grant took the leave first, and the wait goes on (false). A Keep that stays asks again, Yes selected, so the close
/// dialog shows that notice: the window never seems to ignore the close.
fn confirm_stay(leases: &crate::lease::Leases, prompt: &mut Option<NavPrompt>, leaving: &mut Option<crate::lease::Leaving>,
    should_exit: &mut bool, status: &mut String, now: std::time::Instant) -> bool {
    if !leaving.as_mut().is_some_and(|l| leases.stay(l, now)) {
        return false;
    }
    if leaving.as_ref().is_some_and(|l| l.intent == LeaveIntent::Keep) {
        let _ = open_quit_prompt(prompt, true);
    }
    *leaving = None;
    *should_exit = false;
    status.clear();
    true
}

impl State {
    /// ADR 0041 step 6 U3 ruling (a): the ONE quit dispatcher every
    /// user-requested exit routes through — the window-close request and
    /// the Quit keybind both call this instead of `event_loop.exit()`
    /// directly. Exit 75 (self-relaunch) reaches `leave` from
    /// `window_event`'s relaunch-flag branch instead; a crash and
    /// `--capture` never reach it.
    ///
    /// `exit_intent` decides: the window's close button leaves with Close,
    /// Ctrl+Q asks first (`NavPrompt::ConfirmQuit`), and a second request
    /// while leaving exits at once (an X during a Keep closes instead, and a Ctrl+Q while a Keep or a Handover waits
    /// on an unanswered lease request stays open, `confirm_stay`). `leave` tells each held lease's daemon
    /// what to do with this computer's sessions and the window exits once
    /// the acks are in (`about_to_wait`).
    pub(in crate::ui) fn request_quit(&mut self, event_loop: &ActiveEventLoop, reason: ExitReason) {
        match exit_intent(reason, self.leaving.as_ref().map(|l| l.intent)) {
            ExitStep::Ask => {
                if !open_quit_prompt(&mut self.nav_prompt, false) {
                    self.status = QUIT_REFUSED.to_string();
                }
                self.window.request_redraw();
            }
            ExitStep::Now { code } => {
                // A Ctrl+Q while a Keep or a Handover waits on an unanswered lease request stays open, as at the
                // leave's deadline: exiting would drop the request. An X still exits.
                if matches!(reason, ExitReason::QuitKey) && self.stay_open(std::time::Instant::now()) {
                    return;
                }
                let code = terminal_close(&self.exit_deadline, &self.leases, self.leaving.as_mut(), code);
                self.finish_exit(event_loop, code);
            }
            ExitStep::Ignore => {}
            ExitStep::Supersede => self.leave(event_loop, LeaveIntent::Close, 0),
            ExitStep::Leave { intent, code } => self.leave(event_loop, intent, code),
        }
    }

    /// Start leaving: tell every held lease's daemon `intent`. With a lease
    /// to leave, the exit itself happens in `about_to_wait` once the acks are
    /// in; with none, at once. The window never ends the drawer's session
    /// itself: the daemon's Close ends it, and a Keep keeps it.
    pub(in crate::ui) fn leave(&mut self, event_loop: &ActiveEventLoop, intent: LeaveIntent, code: i32) {
        self.relaunch_stayed = false;
        match begin_leave(&self.leases, &mut self.nav_prompt, &mut self.leaving, &mut self.should_exit,
            intent, code, std::time::Instant::now()) {
            LeaveEffect::Redraw => self.window.request_redraw(),
            LeaveEffect::Finish(code) => self.finish_exit(event_loop, code),
        }
    }

    pub(in crate::ui) fn finish_exit(&mut self, event_loop: &ActiveEventLoop, code: i32) {
        end_loop(&self.exit_deadline, code, || event_loop.exit());
    }

    /// A Keep or Handover still waiting on an unanswered lease request (`confirm_stay`): exiting would drop the
    /// request, and a later grant would end this computer's sessions, so the window stays open. False when a grant
    /// took the leave first.
    pub(in crate::ui) fn stay_open(&mut self, now: std::time::Instant) -> bool {
        let handover = self.leaving.as_ref().is_some_and(|l| l.intent == LeaveIntent::Handover);
        let stayed = confirm_stay(&self.leases, &mut self.nav_prompt, &mut self.leaving, &mut self.should_exit,
            &mut self.status, now);
        if stayed {
            self.relaunch_stayed = handover;
            self.window.request_redraw();
        }
        stayed
    }
}

#[cfg(test)]
#[cfg_attr(feature = "test-window-progress", allow(unused_imports, reason = "the harness=false native targets run none of these tests"))]
#[path = "exit_tests.rs"]
mod tests;
