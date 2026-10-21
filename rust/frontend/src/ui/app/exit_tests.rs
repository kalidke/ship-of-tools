use super::*;

/// Ctrl+Q reaches `request_quit` from any pane, so a prompt left open in the tree can be there: its typed text
/// stays, and the question does not open over it.
#[test]
fn the_question_never_overwrites_another_prompt() {
    for other in [
        NavPrompt::CreateFile { dir_node_id: "files:".into(), input: "typed-name".into() },
        NavPrompt::ConfirmDelete { node_id: "files:a".into(), label: "a".into() },
        NavPrompt::ScaleEntry { node_id: "files:a".into(), input: "1".into() },
    ] {
        let mut prompt = Some(other.clone());
        assert!(!open_quit_prompt(&mut prompt, false));
        assert!(std::mem::discriminant(prompt.as_ref().unwrap()) == std::mem::discriminant(&other),
            "the open prompt is untouched");
        if let Some(NavPrompt::CreateFile { input, .. }) = &prompt {
            assert_eq!(input, "typed-name");
        }
    }
    let mut prompt = None;
    assert!(open_quit_prompt(&mut prompt, false) && matches!(prompt, Some(NavPrompt::ConfirmQuit { keep: false })));
    assert!(open_quit_prompt(&mut prompt, true), "the question may reopen itself");
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
    let _ = quit_prompt_step(false, &Key::Character("typed-fixture-text".into()), false, false);
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
