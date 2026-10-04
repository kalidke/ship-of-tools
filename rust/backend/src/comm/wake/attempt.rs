//! One wake attempt: attach, check the screen is a free prompt and holds still, type the line, then Enter.

use super::*;
use super::screen::{free_test_lines, held_rows, wake_lines};
use crate::rows::run::headless::{attach, checkpointed, send_enter, type_and_pace, HeadlessError, POLL_INTERVAL, SHUTDOWN_WAIT};

/// What [`wake_if_free`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum WakeOutcome {
    /// The screen was not a free prompt; nothing was typed.
    NotFree,
    /// The line was typed and Enter written.
    Woke,
    /// The line was typed but the live screen then did not show it alone in main's input box, for `reason`
    /// (the gate's own); `border` is the line above the cursor's row at the gate. No Enter was sent.
    TypedNoEnter { reason: &'static str, border: String },
    /// A write on `step` (`"text"` or `"enter"`) returned an error (`detail`, the phase and its text), or its
    /// delivery is unknown: what that step wrote may still have reached the agent.
    Unconfirmed { step: &'static str, detail: String },
}

/// The outcome once the line is typed and the gate passed: the Enter write's result.
pub(crate) fn wake_outcome(enter: Result<(), HeadlessError>) -> WakeOutcome {
    match enter {
        Ok(()) => WakeOutcome::Woke,
        Err(e) => unconfirmed("enter", e),
    }
}

/// A failed write on `step`, as the outcome the wake reports.
pub(crate) fn unconfirmed(step: &'static str, e: HeadlessError) -> WakeOutcome {
    WakeOutcome::Unconfirmed { step, detail: format!("{}: {}", e.phase, e.detail) }
}

/// The comm wake's one attach (0031 B3): attach, checkpoint, test the
/// screen on that same client with `is_free(lines, cursor, agent)` (the lines as
/// [`free_test_lines`] reads them), and
/// only then type `line` and, if the typed-line gate (asked of the live screen after the pacing wait) says
/// the line sits alone in main's input box, Enter, as [`write_and_enter`] does. A screen
/// that is not free gets no hold (it still costs the attach); one that is
/// must then hold identical (the cursor, and every row through the line
/// under it) for `still_for`, else it is a working row and nothing is
/// typed. `is_free` is asked of the first frame and again immediately
/// before typing. Never takes the pen unless the prompt is free and
/// still, and never retries.
pub fn wake_if_free(
    state_dir: &Path,
    controller_id: &str,
    line: &str,
    is_free: &dyn Fn(&[String], Option<(u16, u16)>, &str) -> bool,
    agent: &str,
    still_for: Duration,
    op_budget: Duration,
    quiet_budget: Duration,
    pacing_budget: Duration,
) -> Result<WakeOutcome, HeadlessError> {
    let mut client = checkpointed(attach(state_dir, controller_id)?, Instant::now() + op_budget)?;
    let cursor = client.screen().cursor_position();
    let first = wake_lines(client.screen());
    let seen = free_test_lines(client.screen());
    // `SOT_TEST_WAKE_MARKS` (test-only, the `SOT_TEST_PACING_HOLD` convention): a directory in which the attempt
    // leaves a file at each point, for a stub that moves focus at exactly `hold` and `final-ok`: `hold` when the hold
    // begins (the screen was free), `final-ok` when the live final check passes (before typing), `done` when
    // the client is shut down. An attach or checkpoint failure before the hold (above) returns early and
    // writes no `done`.
    let marks = std::env::var_os("SOT_TEST_WAKE_MARKS").map(std::path::PathBuf::from);
    let mark = |name: &str| {
        if let Some(dir) = &marks {
            let path = dir.join(name);
            if let Err(e) = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, b"")) {
                tracing::warn!(path = %path.display(), error = %e, "comm wake: test mark not written");
            }
        }
    };
    let out = if !is_free(&seen, Some(cursor), agent) {
        Ok(WakeOutcome::NotFree)
    } else {
        mark("hold");
        let held_from = Instant::now();
        let mut still = true;
        while still && held_from.elapsed() < still_for {
            std::thread::sleep(POLL_INTERVAL);
            client.pump();
            still = client.screen().cursor_position() == cursor && held_rows(&wake_lines(client.screen()), cursor.0) == held_rows(&first, cursor.0);
        }
        // The live screen, not `seen`: the rows through the box can hold still while focus moves below them.
        if still && is_free(&free_test_lines(client.screen()), Some(client.screen().cursor_position()), agent) {
            mark("final-ok");
            Ok(match type_and_pace(&mut client, line.as_bytes(), op_budget, quiet_budget, pacing_budget) {
                Err(e) => unconfirmed("text", e),
                Ok(_) => {
                    client.pump();
                    let lines = free_test_lines(client.screen());
                    let cursor = Some(client.screen().cursor_position());
                    match super::screen::typed_refusal(&lines, cursor, agent, cfg!(windows), line) {
                        Some(reason) => {
                            let border = cursor.and_then(|(row, _)| lines.get((row as usize).checked_sub(1)?)).cloned().unwrap_or_default();
                            WakeOutcome::TypedNoEnter { reason, border }
                        }
                        None => wake_outcome(send_enter(&mut client, op_budget)),
                    }
                }
            })
        } else {
            Ok(WakeOutcome::NotFree)
        }
    };
    client.shutdown(SHUTDOWN_WAIT);
    mark("done");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_outcome_table() {
        let failed = || HeadlessError { phase: "record", detail: "input delivery unknown".to_string(), submitted: true };
        assert_eq!(wake_outcome(Ok(())), WakeOutcome::Woke);
        assert_eq!(wake_outcome(Err(failed())), WakeOutcome::Unconfirmed { step: "enter", detail: "record: input delivery unknown".to_string() });
    }

    #[test]
    fn text_write_failure_is_unconfirmed_not_skipped() {
        // The mapping `wake_if_free` applies to a `type_and_pace` error; a real text-write failure needs a stub
        // supervisor with no seam here, so the mapping is tested directly.
        let failed = HeadlessError { phase: "write", detail: "broken pipe".to_string(), submitted: true };
        let out = unconfirmed("text", failed);
        assert_eq!(out, WakeOutcome::Unconfirmed { step: "text", detail: "write: broken pipe".to_string() });
    }

    // Observed on Linux only: the attach client's worker is the same code elsewhere, but no host here runs it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_wake_without_a_lane_is_a_checkpoint_error() {
        let free = |_: &[String], _: Option<(u16, u16)>, _: &str| true;
        let d = Duration::from_secs(5);
        let err = wake_if_free(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", "x", &free, "claude", d, d, d, d)
            .expect_err("no lane to wake");
        assert_eq!(err.phase, "checkpoint");
        assert!(!err.submitted);
    }
}
