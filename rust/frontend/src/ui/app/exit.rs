//! The quit prompt and leaving: the Ctrl+Q prompt's key table, the one quit dispatcher, the exit path.

use super::*;

/// A key the Ctrl+Q prompt reacts to.
#[derive(Clone, Copy)]
enum QuitKey {
    Tab,
    Enter,
    Esc,
    Other,
}

#[derive(Debug, PartialEq, Eq)]
pub(in crate::ui) enum QuitPromptStep {
    Stay { keep: bool },
    Leave(LeaveIntent),
    Cancel,
    Ignore,
}

/// The Ctrl+Q prompt's key table: Tab flips the answer, Enter confirms it,
/// Esc cancels, anything else changes nothing.
fn quit_prompt_key(keep: bool, key: QuitKey) -> QuitPromptStep {
    match key {
        QuitKey::Tab => QuitPromptStep::Stay { keep: !keep },
        QuitKey::Enter => QuitPromptStep::Leave(if keep { LeaveIntent::Keep } else { LeaveIntent::Close }),
        QuitKey::Esc => QuitPromptStep::Cancel,
        QuitKey::Other => QuitPromptStep::Ignore,
    }
}

/// The Ctrl+Q prompt's reading of a key. It owns the keyboard while open,
/// so every key reaches it before any global binding: Tab, Enter and Esc act
/// as `quit_prompt_key` says, and repeats and every other key do nothing.
pub(in crate::ui) fn prompt_takes_key(keep: bool, tab: bool, action: Option<Action>, repeat: bool) -> QuitPromptStep {
    if repeat {
        return QuitPromptStep::Ignore;
    }
    let key = if tab {
        QuitKey::Tab
    } else if action == Some(Action::Confirm) {
        QuitKey::Enter
    } else if action == Some(Action::Cancel) {
        QuitKey::Esc
    } else {
        QuitKey::Other
    };
    quit_prompt_key(keep, key)
}

/// A focus change away from the navigation pane dismisses the Ctrl+Q prompt
/// without quitting; no other prompt reacts to focus.
pub(in crate::ui) fn quit_prompt_on_focus(prompt: Option<&NavPrompt>, to: PaneFocus) -> Option<QuitPromptStep> {
    (matches!(prompt, Some(NavPrompt::ConfirmQuit { .. })) && to != PaneFocus::NavTree)
        .then_some(QuitPromptStep::Cancel)
}

/// The quit prompt as its text and its choice; the choice ends the prompt
/// (`nav_pinned_rows` keeps it whole on the last row).
pub(in crate::ui) fn quit_prompt_line(keep: bool) -> (String, String) {
    let choice = if keep { "No  [Yes]" } else { "[No]  Yes" };
    (
        "Keep the daemon and sessions running?  Tab switches \u{b7} Enter confirms \u{b7} Esc cancels".to_string(),
        choice.to_string(),
    )
}

/// Whether the redraw that set `should_exit` ends the event loop. A window
/// that is leaving exits only from `about_to_wait`'s poll, with its own
/// exit code once the acks are in. The capture harness never leases and
/// `about_to_wait` returns early for it, so its exit is here.
pub(in crate::ui) fn redraw_exits(should_exit: bool, leaving: bool, harness: bool) -> bool {
    should_exit && (harness || !leaving)
}

fn open_quit_prompt(prompt: &mut Option<NavPrompt>) {
    *prompt = Some(NavPrompt::ConfirmQuit { keep: false });
}

#[derive(Debug, PartialEq, Eq)]
enum LeaveEffect { Redraw, Finish(i32) }

/// Only leases and exit-state slots are available to this transition.
fn begin_leave(leases: &crate::lease::Leases, prompt: &mut Option<NavPrompt>, leaving: &mut Option<crate::lease::Leaving>,
    should_exit: &mut bool, intent: LeaveIntent, code: i32, now: std::time::Instant) -> LeaveEffect {
    *prompt = None;
    *leaving = leases.leave_all(intent, code, now);
    *should_exit = true;
    if leaving.is_some() { LeaveEffect::Redraw } else { LeaveEffect::Finish(code) }
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
    /// while leaving exits at once (an X during a Keep closes instead). `leave` tells each held lease's daemon
    /// what to do with this computer's sessions and the window exits once
    /// the acks are in (`about_to_wait`).
    pub(in crate::ui) fn request_quit(&mut self, event_loop: &ActiveEventLoop, reason: ExitReason) {
        match exit_intent(reason, self.leaving.as_ref().map(|l| l.intent)) {
            ExitStep::Ask => {
                open_quit_prompt(&mut self.nav_prompt);
                self.window.request_redraw();
            }
            ExitStep::Now { code } => {
                // A leave already queued (a Close after a Keep) is written
                // before the runtime and its streams go.
                self.leases.deliver_queued(crate::lease::LEAVE_WRITE_WAIT);
                let code = close_now(self.leaving.as_mut(), code);
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
        match begin_leave(&self.leases, &mut self.nav_prompt, &mut self.leaving, &mut self.should_exit,
            intent, code, std::time::Instant::now()) {
            LeaveEffect::Redraw => self.window.request_redraw(),
            LeaveEffect::Finish(code) => self.finish_exit(event_loop, code),
        }
    }

    pub(in crate::ui) fn finish_exit(&mut self, event_loop: &ActiveEventLoop, code: i32) {
        if code != 0 {
            #[cfg(windows)]
            allow_next_foreground();
            std::process::exit(code);
        }
        event_loop.exit();
    }
}

#[cfg(test)]
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
    fn quit_prompt_key_table() {
        use QuitKey::*;
        use QuitPromptStep::*;
        assert_eq!(quit_prompt_key(false, Tab), Stay { keep: true });
        assert_eq!(quit_prompt_key(true, Tab), Stay { keep: false });
        assert_eq!(quit_prompt_key(false, Enter), Leave(LeaveIntent::Close));
        assert_eq!(quit_prompt_key(true, Enter), Leave(LeaveIntent::Keep));
        assert_eq!(quit_prompt_key(false, Esc), Cancel);
        assert_eq!(quit_prompt_key(true, Esc), Cancel);
        assert_eq!(quit_prompt_key(false, Other), Ignore);
        assert_eq!(quit_prompt_key(true, Other), Ignore);
        // The prompt sees every key before global dispatch: Ctrl+T (the
        // terminal drawer) and Ctrl+= (font size) do nothing while it is open.
        assert_eq!(prompt_takes_key(false, false, Some(Action::ToggleTerminalDrawer), false), Ignore);
        assert_eq!(prompt_takes_key(true, false, Some(Action::FontScaleUp), false), Ignore);
        assert_eq!(prompt_takes_key(false, false, None, false), Ignore);
        assert_eq!(prompt_takes_key(false, true, None, false), Stay { keep: true });
        assert_eq!(prompt_takes_key(true, false, Some(Action::Confirm), false), Leave(LeaveIntent::Keep));
        assert_eq!(prompt_takes_key(false, false, Some(Action::Cancel), false), Cancel);
        assert_eq!(prompt_takes_key(false, false, Some(Action::Confirm), true), Ignore);
        assert_eq!(prompt_takes_key(false, true, None, true), Ignore);
        let (_, no) = quit_prompt_line(false);
        assert!(no.contains("[No]") && no.contains("Yes") && !no.contains("[Yes]"));
        let (_, yes) = quit_prompt_line(true);
        assert!(yes.contains("[Yes]") && !yes.contains("[No]"));
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
        println!("T1 body entered: ui::app::exit::tests::quit_transitions_log_only_finite_fields");
        let log = sot_log::test_log::capture();
        let mut prompt = None;
        open_quit_prompt(&mut prompt);
        let _ = prompt_takes_key(false, false, None, false);
        let leases = crate::lease::Leases::new(true, vec![]);
        let mut leaving = None;
        let mut should_exit = false;
        assert_eq!(begin_leave(&leases, &mut prompt, &mut leaving, &mut should_exit,
            LeaveIntent::Keep, 0, std::time::Instant::now()), LeaveEffect::Finish(0));
        let text = log.text();
        println!("T1 fixture observed: captured actual quit transitions");
        for message in ["quit prompt: open", "quit prompt: key", "window: leaving"] {
            assert!(text.contains(message), "missing transition event: {message}");
        }
        assert!(!text.contains("typed-fixture-text"));
        println!("T1 assertion passed: missing transition event: quit prompt: open");
    }

    #[tokio::test]
    async fn leave_close_keep_and_handover_only_leave_leases() {
        println!("T1 body entered: ui::app::exit::tests::leave_close_keep_and_handover_only_leave_leases");
        use crate::lease::{grant_tests::bind, leave_tests::{leave_fake, logged, is_leave, finish}, LeaveStep};
        for (intent, name, code) in [(LeaveIntent::Close, "close", 0), (LeaveIntent::Keep, "keep", 0),
            (LeaveIntent::Handover, "handover", 75), (LeaveIntent::Handover, "handover", 76)] {
            let (listener, path) = bind("beginleave");
            let (log, _, fake) = leave_fake(listener, None, None, std::time::Duration::ZERO);
            let host = "fixture".to_string();
            let leases = crate::lease::Leases::new(false, vec![host.clone()]);
            tokio::time::timeout(std::time::Duration::from_secs(5), leases.before_data_connection(&host, &path, None))
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
        println!("T1 fixture observed: actual lease frames and exit effects");
        println!("T1 assertion passed: exit state is set before the UI effect");
    }

}
