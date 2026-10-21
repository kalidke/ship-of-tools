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
    let free = |_: &[String], _: Option<(u16, u16)>, _: &str| Some(Prompt::Empty);
    let d = Duration::from_secs(5);
    let err = wake_if_free(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", "x", &free, "claude", d, d)
        .expect_err("no lane to wake");
    assert_eq!(err.phase, "checkpoint");
    assert!(!err.submitted);
}
