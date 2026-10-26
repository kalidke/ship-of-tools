use super::*;

#[test]
fn wake_outcome_table() {
    let failed = || HeadlessError { phase: "record", detail: "input delivery unknown".to_string(), submitted: true };
    assert_eq!(wake_outcome(Ok(())), WakeOutcome::Woke);
    assert_eq!(wake_outcome(Err(failed())), WakeOutcome::Unconfirmed { step: "enter", detail: "record: input delivery unknown".to_string() });
}

#[test]
fn a_text_write_that_may_have_landed_is_not_a_clean_refusal() {
    let failed = |phase| HeadlessError { phase, detail: "x".to_string(), submitted: true };
    // Only the stale refusal (phase "input") wrote nothing; any other failure may have reached the agent.
    assert_eq!(text_failed(failed("input")), WakeOutcome::Unconfirmed { step: "text", detail: "input: x".to_string() });
    // The phases `send_text` returns: `input` (stale), `record` (delivery unknown or the deadline), `take` (the client died).
    for phase in ["record", "take"] {
        assert_eq!(text_failed(failed(phase)), WakeOutcome::TextUnknown { detail: format!("{phase}: x") });
    }
}

// Observed on Linux only: the attach client's worker is the same code elsewhere, but no host here runs it.
#[cfg(target_os = "linux")]
#[test]
fn a_wake_without_a_lane_is_a_checkpoint_error() {
    let free = |_: &[String], _: Option<(u16, u16)>, _: &str, _: bool| Some(Prompt::Empty);
    let d = Duration::from_secs(5);
    let err = wake_if_free(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", "h", "x", None, &free, "claude", d, d)
        .expect_err("no lane to wake");
    assert_eq!(err.phase, "checkpoint");
    assert!(!err.submitted);
}

/// A typing logs one INFO event that names the handle, what was sent, the box rows the gate read and the outcome, with
/// the screen's text escaped.
#[test]
fn a_typing_logs_one_event_with_what_the_gate_read() {
    #[derive(Clone, Default)]
    struct Buf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let sink = buf.clone();
    let subscriber = tracing_subscriber::fmt().with_ansi(false).with_writer(move || sink.clone()).finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let read = ("col 2: > U+2588".to_string(), vec!["---".to_string(), "> \u{1b}draft".to_string(), "---".to_string()]);
    log_typing("some-handle", "line and enter", &read, &WakeOutcome::Woke);
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert_eq!(log.lines().count(), 1, "{log}");
    for part in ["INFO", "some-handle", "line and enter", "col 2: > U+2588", "draft", "Woke", "\\u{1b}"] {
        assert!(log.contains(part), "{part} missing: {log}");
    }
}

#[test]
fn a_typing_attempt_owns_the_row_until_it_ends() {
    let tmp = tempfile::tempdir().unwrap();
    let row = tmp.path().join("row");
    let another = tmp.path().join("another");
    let owned = Typing::take(&row).expect("first attempt");
    let independent = Typing::take(&another).expect("other rows can type concurrently");
    let free = |_: &[String], _: Option<(u16, u16)>, _: &str, _: bool| Some(Prompt::Empty);
    let d = Duration::from_secs(1);
    let attempted = wake_if_free(&row, "test", "h", "line", None, &free, "claude", d, d).unwrap();
    assert_eq!(attempted.outcome, WakeOutcome::NotFree { reason: "another typing on this row", row: String::new() });
    drop(owned);
    assert!(Typing::take(&row).is_some(), "attempt releases its row on exit");
    drop(independent);
}

#[test]
fn the_prompt_gate_checks_the_supplied_lines_fit_and_exact_contents() {
    let boxed = |text: &str| vec!["─".repeat(32), format!("❯\u{a0}{text}"), "─".repeat(32)];
    let prompt = |lines: &[String], text| super::screen::prompt_of(lines, Some((1, 2)), "claude", false, text);
    assert_eq!(prompt(&boxed(""), "[sot-page] ok"), Ok(Prompt::Empty));
    assert_eq!(prompt(&boxed(""), &"x".repeat(32)), Err("pane too narrow for the line"));
    assert_eq!(prompt(&boxed("[sot-page] ok"), "[sot-page] ok"), Ok(Prompt::HoldsLine));
    assert!(prompt(&boxed("[sot-page] other"), "[sot-page] ok").is_err());
}


#[test]
fn a_boot_look_retries_when_another_typing_owns_the_row() {
    let pending = super::super::boot_look_pending;
    assert_eq!(pending(&Ok(WakeOutcome::NotFree { reason: "another typing on this row", row: String::new() })),
        Some("another typing on this row"));
    assert_eq!(pending(&Err(HeadlessError { phase: "attach", detail: "unavailable".into(), submitted: false })),
        Some("the row could not be read"));
    assert_eq!(pending(&Ok(WakeOutcome::NotFree { reason: "dialog", row: String::new() })), None);
    assert_eq!(pending(&Ok(WakeOutcome::Woke)), None);
}
