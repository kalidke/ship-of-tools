//! The quit prompt and leaving: the Ctrl+Q prompt's key table, the one quit dispatcher, the exit path.

use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(in crate::ui) enum QuitPromptStep {
    Stay { keep: bool },
    Leave(LeaveIntent),
    Cancel,
    Ignore,
}

/// The prompt consumes logical keys without consulting configurable actions.
pub(in crate::ui) fn quit_prompt_step(keep: bool, key: &Key, repeat: bool) -> QuitPromptStep {
    let step = if repeat {
        QuitPromptStep::Ignore
    } else {
        match key {
            Key::Named(NamedKey::Tab) => QuitPromptStep::Stay { keep: !keep },
            Key::Named(NamedKey::Enter) => QuitPromptStep::Leave(if keep { LeaveIntent::Keep } else { LeaveIntent::Close }),
            _ => QuitPromptStep::Cancel,
        }
    };
    tracing::info!(?step, "quit prompt: key");
    step
}

/// A focus change away from the navigation pane dismisses the Ctrl+Q prompt
/// without quitting; no other prompt reacts to focus.
pub(in crate::ui) fn quit_prompt_on_focus(prompt: Option<&NavPrompt>, to: PaneFocus) -> Option<QuitPromptStep> {
    (matches!(prompt, Some(NavPrompt::ConfirmQuit { .. })) && to != PaneFocus::NavTree)
        .then_some(QuitPromptStep::Cancel)
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

fn open_quit_prompt(prompt: &mut Option<NavPrompt>, keep: bool) {
    *prompt = Some(NavPrompt::ConfirmQuit { keep });
    tracing::info!(keep, "quit prompt: open");
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
        open_quit_prompt(prompt, true);
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
                open_quit_prompt(&mut self.nav_prompt, false);
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
            // The question asked again shows only while the tree has focus (`set_focus`).
            if matches!(self.nav_prompt, Some(NavPrompt::ConfirmQuit { .. })) {
                self.set_focus(PaneFocus::NavTree);
            }
            self.window.request_redraw();
        }
        stayed
    }
}

#[cfg(test)]
#[cfg_attr(feature = "test-window-progress", allow(unused_imports, reason = "the harness=false native targets run none of these tests"))]
mod tests {
    use super::*;

    #[test]
    fn quit_prompt_on_focus_table() {
        let others = [
            Some(NavPrompt::ConfirmDelete { node_id: String::new(), label: String::new() }),
            None,
        ];
        for to in [PaneFocus::NavTree, PaneFocus::Preview, PaneFocus::Llm, PaneFocus::Repl] {
            for keep in [false, true] {
                let want = (to != PaneFocus::NavTree).then_some(QuitPromptStep::Cancel);
                assert_eq!(quit_prompt_on_focus(Some(&NavPrompt::ConfirmQuit { keep }), to), want);
            }
            for p in &others {
                assert_eq!(quit_prompt_on_focus(p.as_ref(), to), None);
            }
        }
    }

    #[test]
    fn redraw_exit_table() {
        // A leaving window exits only from `about_to_wait`.
        assert!(!redraw_exits(true, true, false));
        assert!(!redraw_exits(false, true, false));
        // The capture harness exits at its redraw.
        assert!(redraw_exits(true, false, true));
        // A plain `should_exit` with nothing leaving exits.
        assert!(redraw_exits(true, false, false));
        assert!(!redraw_exits(false, false, false));
        assert!(!redraw_exits(false, false, true));
    }
    #[test]
    fn quit_transitions_log_only_finite_fields() {
        let log = sot_log::test_log::capture();
        let mut prompt = None;
        open_quit_prompt(&mut prompt, false);
        let _ = quit_prompt_step(false, &Key::Character("typed-fixture-text".into()), false);
        let leases = crate::lease::Leases::new(true, vec![]);
        let mut leaving = None;
        let mut should_exit = false;
        assert_eq!(begin_leave(&leases, &mut prompt, &mut leaving, &mut should_exit,
            LeaveIntent::Keep, 0, std::time::Instant::now()), LeaveEffect::Finish(0));
        let text = log.text();
        for message in ["quit prompt: open", "quit prompt: key", "window: leaving"] {
            assert!(text.contains(message), "missing transition event: {message}");
        }
        assert!(!text.contains("typed-fixture-text"));
    }

    #[tokio::test]
    async fn leave_close_keep_and_handover_only_leave_leases() {
        use crate::lease::{grant_tests::bind, leave_tests::{leave_fake, logged, is_leave, finish}, LeaveStep};
        for (intent, name, code) in [(LeaveIntent::Close, "close", 0), (LeaveIntent::Keep, "keep", 0),
            (LeaveIntent::Handover, "handover", 75), (LeaveIntent::Handover, "handover", 76)] {
            let (listener, path) = bind("beginleave");
            let (log, _, fake) = leave_fake(listener, None, None, std::time::Duration::ZERO);
            let host = "fixture".to_string();
            let leases = crate::lease::Leases::new(false, vec![host.clone()]);
            tokio::time::timeout(std::time::Duration::from_secs(5), leases.before_data_connection(&host, &path, None, |_| {}))
                .await.unwrap().unwrap();
            let (mut prompt, mut leaving, mut should_exit) = (Some(NavPrompt::ConfirmQuit { keep: true }), None, false);
            let now = std::time::Instant::now();
            let effect = begin_leave(&leases, &mut prompt, &mut leaving, &mut should_exit, intent, code, now);
            assert!(should_exit && prompt.is_none(), "exit state is set before the UI effect");
            assert_eq!(effect, LeaveEffect::Redraw);
            assert!(matches!(leaving.as_mut().unwrap().poll(now), LeaveStep::Wait(_)));
            assert_eq!(leaving.as_ref().unwrap().exit_code, code);
            let seen = logged(&log, 1).await;
            assert!(seen.len() == 1 && is_leave(&seen[0], name), "{seen:?}");
            if intent == LeaveIntent::Keep {
                assert_eq!(begin_leave(&leases, &mut prompt, &mut leaving, &mut should_exit, LeaveIntent::Close, 0, now), LeaveEffect::Redraw);
                assert!(is_leave(&logged(&log, 2).await[1], "close"));
            }
            drop((leaving, leases));
            finish(fake, &log).await;
        }
        for intent in [LeaveIntent::Close, LeaveIntent::Keep, LeaveIntent::Handover] {
            let leases = crate::lease::Leases::new(true, vec![]);
            let (mut prompt, mut leaving, mut should_exit) = (None, None, false);
            assert_eq!(begin_leave(&leases, &mut prompt, &mut leaving, &mut should_exit, intent, 0,
                std::time::Instant::now()), LeaveEffect::Finish(0));
            assert!(should_exit && leaving.is_none());
        }
    }

    /// A stay, mid-wait (a Ctrl+Q) or at the deadline, ends the leave and keeps the window open: neither the next
    /// redraw nor the next wake exits, the late grant installs with no leave behind it, a new lease is taken, the
    /// lease notice names the stay until the next leave whenever the standings name no cause, a Keep asks again, and a
    /// relaunch command's status does not outlast the stay. A leave with no request unanswered never stays.
    #[tokio::test]
    async fn a_stay_keeps_the_window_open_until_the_next_leave() {
        use crate::lease::{grant_tests::bind, leave_tests::{leave_fake, logged, is_leave, finish}, LeaveStep};
        use std::time::Duration;
        for (intent, code, said, at_deadline, answer) in [(LeaveIntent::Keep, 0, "keep not done", false, "granted"),
            (LeaveIntent::Keep, 0, "keep not done", true, "granted"),
            (LeaveIntent::Handover, 75, "relaunch not done", false, "granted"),
            (LeaveIntent::Handover, 75, "relaunch not done", true, "granted"),
            (LeaveIntent::Keep, 0, "keep not done", false, "foreign")] {
            let (listener, path) = bind("staytransition");
            let (asked_tx, asked) = tokio::sync::oneshot::channel();
            let (open, open_rx) = tokio::sync::oneshot::channel();
            let (log, _, fake) = leave_fake(listener, Some((asked_tx, open_rx)), None, Duration::ZERO);
            let (host, next) = ("fixture".to_string(), "next".to_string());
            let leases = crate::lease::Leases::new(false, vec![host.clone()]);
            let granted = answer == "granted";
            if !granted {
                // A redial: the host's last answer (no backend there) is stale while the new request is out.
                let (gone, gone_path) = bind("staygone");
                drop(gone);
                assert!(leases.before_data_connection(&host, &gone_path, None, |_| {}).await.is_err(), "no backend");
            }
            let in_flight = {
                let (leases, host) = (leases.clone(), host.clone());
                tokio::spawn(async move { leases.before_data_connection(&host, &path, None, |_| {}).await })
            };
            asked.await.unwrap();
            let (mut prompt, mut leaving, mut should_exit, mut status) = (None, None, false, "relaunching…".to_string());
            let t0 = std::time::Instant::now();
            assert_eq!(begin_leave(&leases, &mut prompt, &mut leaving, &mut should_exit, intent, code, t0), LeaveEffect::Redraw);
            let at = if at_deadline { t0 + sot_protocol::ops::lease::KEEP_ACK_WAIT + Duration::from_secs(1) } else { t0 };
            if at_deadline {
                assert_eq!(leaving.as_mut().unwrap().poll(at), LeaveStep::Stay);
            }
            assert!(confirm_stay(&leases, &mut prompt, &mut leaving, &mut should_exit, &mut status, at),
                "{intent:?} with its request unanswered");
            assert!(leaving.is_none() && !should_exit && !redraw_exits(should_exit, leaving.is_some(), false),
                "a stayed window would exit at its next redraw or wake");
            let asked = (intent == LeaveIntent::Keep).then_some(true);
            assert_eq!(prompt.as_ref().map(|p| matches!(p, NavPrompt::ConfirmQuit { keep: true })), asked,
                "a Keep that stays asks again, Yes selected; a relaunch does not");
            assert_eq!(status, "", "the relaunch command's status outlasted the stay");
            assert!(leases.notice().is_some_and(|n| n.starts_with(said)), "the stay's notice: {:?}", leases.notice());
            open.send(answer).unwrap();
            assert_eq!(in_flight.await.unwrap().unwrap(), 0);
            assert_eq!(leases.held(&host), granted, "a late grant is held");
            let cause = if granted { said } else { "closing will not end sessions" };
            assert!(leases.notice().is_some_and(|n| n.starts_with(cause)), "after the {answer} answer: {:?}", leases.notice());
            let (listener, path) = bind("staynext");
            let (next_log, _, next_fake) = leave_fake(listener, None, None, Duration::ZERO);
            leases.before_data_connection(&next, &path, None, |_| {}).await.expect("a stayed window takes a new lease");
            assert_eq!(begin_leave(&leases, &mut prompt, &mut leaving, &mut should_exit, LeaveIntent::Keep, 0, t0),
                LeaveEffect::Redraw);
            assert_eq!(leases.notice(), None, "the next leave ends the stay's notice");
            assert!(!confirm_stay(&leases, &mut prompt, &mut leaving, &mut should_exit, &mut status, t0),
                "a Keep with no request unanswered");
            assert!(leaving.is_some() && should_exit);
            if granted {
                assert!(logged(&log, 1).await.first().is_some_and(|e| is_leave(e, "keep")), "the next leave reached the late grant");
            }
            drop((leaving, leases));
            // A refused host has no lease to leave: its daemon reads only the end.
            for (fake, log, n) in [(fake, log, if granted { 2 } else { 1 }), (next_fake, next_log, 2)] {
                let seen = finish(fake, &log).await;
                assert!(seen.len() == n && (n == 1 || is_leave(&seen[0], "keep")), "only the next leave was written: {seen:?}");
            }
        }
    }
}
