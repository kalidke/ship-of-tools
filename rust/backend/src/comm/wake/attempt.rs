//! One wake attempt: attach, check the screen is a free prompt and holds still, type the line, wait for it, then Enter.

use super::*;
use super::screen::{cursor_row_trace, free_test_lines, held_rows, wake_lines, Prompt};
use crate::rows::run::headless::{attach, checkpointed, send_enter, send_text, Client, HeadlessError, POLL_INTERVAL, SHUTDOWN_WAIT};

/// What [`wake_if_free`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum WakeOutcome {
    /// The screen was not a free prompt; nothing was typed. `row` is the cursor's row as the gate read it
    /// ([`cursor_row_trace`]).
    NotFree { row: String },
    /// The line was typed and Enter written.
    Woke,
    /// The line was typed but the live screen then did not show it alone in main's input box, for `reason`
    /// (the gate's own); `border` is the line above the cursor's row at the gate, and `row` the cursor's row
    /// ([`cursor_row_trace`]). No Enter was sent.
    TypedNoEnter { reason: &'static str, border: String, row: String },
    /// The Enter write (`step` `"enter"`) failed in a phase other than `input` (`detail`, the phase and its text): its
    /// delivery is unknown, and it may still have reached the agent. For `step` `"text"` it is only the stale refusal
    /// (phase `input`), which wrote nothing.
    Unconfirmed { step: &'static str, detail: String },
    /// The Enter write was refused as stale (phase `input`, `detail` the phase and its text): nothing was written, so the
    /// typed line still owes its Enter.
    EnterRefused { detail: String },
    /// The text write failed in a phase other than `"input"`, or its delivery is unknown: the line may have reached
    /// the agent (and may draw later), so it counts as typed.
    TextUnknown { detail: String },
    /// An Enter was owed for another run's box and no mail is unread: the attach found a different run, whose box owes
    /// nothing, so nothing was typed and the debt is over.
    RunChanged,
}

/// A wake attempt's result: what it did, and the run it attached to, which an owed Enter then belongs to.
#[derive(Debug, PartialEq, Eq)]
pub struct Attempted {
    pub outcome: WakeOutcome,
    pub run: String,
}

/// A Complete attempt's debt: an Enter owed for the line typed into the box of `run` (`None`: a boot look, whose daemon
/// cannot tell which run it was typed into). An attach that names another run drops the debt; with `mail_unread` the
/// attempt then goes on as a wake of the new run, without it the attempt ends [`WakeOutcome::RunChanged`].
#[derive(Debug, Clone, Copy)]
pub struct Owed<'a> {
    pub run: Option<&'a str>,
    pub mail_unread: bool,
}

/// The outcome once the line is typed and the gate passed: the Enter write's result. Only the stale refusal (phase
/// `"input"`) wrote nothing, as for the text ([`text_failed`]).
pub(crate) fn wake_outcome(enter: Result<(), HeadlessError>) -> WakeOutcome {
    match enter {
        Ok(()) => WakeOutcome::Woke,
        Err(e) if e.phase == "input" => WakeOutcome::EnterRefused { detail: format!("{}: {}", e.phase, e.detail) },
        Err(e) => unconfirmed("enter", e),
    }
}

/// A failed text write: only the stale refusal (phase `"input"`) wrote nothing, so only that is a clean refusal.
fn text_failed(e: HeadlessError) -> WakeOutcome {
    if e.phase == "input" {
        unconfirmed("text", e)
    } else {
        WakeOutcome::TextUnknown { detail: format!("{}: {}", e.phase, e.detail) }
    }
}

/// A failed write on `step`, as the outcome the wake reports.
pub(crate) fn unconfirmed(step: &'static str, e: HeadlessError) -> WakeOutcome {
    WakeOutcome::Unconfirmed { step, detail: format!("{}: {}", e.phase, e.detail) }
}

/// The comm wake's one attach (0031 B3): attach, checkpoint, test the
/// screen on that same client with `is_free(lines, cursor, agent)` (the lines as
/// [`free_test_lines`] reads them; `None` refuses), and
/// only then type `line` and, once the typed-line gate says it sits alone in main's input box (asked of the live
/// screen until `op_budget` after the write), Enter ([`type_then_enter`]). A box that already holds just the wake
/// line (`Prompt::HoldsLine`, on the first frame and the live one) gets Enter alone and no second typing. A screen
/// that is not free gets no hold (it still costs the attach); one that is
/// must then hold identical (the cursor, and every row through the line
/// under it) for `still_for`, else it is a working row and nothing is
/// typed. `is_free` is asked of the first frame and again immediately
/// before typing. Never takes the pen unless the prompt is free and
/// still, and never retries. `owed` is an Enter owed for a line typed earlier ([`Owed`]); `is_free` is told, as its
/// last argument, whether the attempt completes that line (an empty box is not free then), which is known only once the
/// attach has named the run.
pub fn wake_if_free(
    state_dir: &Path,
    controller_id: &str,
    handle: &str,
    line: &str,
    owed: Option<Owed>,
    is_free: &dyn Fn(&[String], Option<(u16, u16)>, &str, bool) -> Option<Prompt>,
    agent: &str,
    still_for: Duration,
    op_budget: Duration,
) -> Result<Attempted, HeadlessError> {
    let mut client = checkpointed(attach(state_dir, controller_id)?, Instant::now() + op_budget)?;
    // The debt is the run's: another run's box holds no line, whatever this daemon typed into the one before it.
    let run = client.run();
    let completes = owed.is_some_and(|o| o.run.map_or(true, |owed_run| owed_run == run));
    let run_changed = owed.is_some() && !completes;
    let ends_here = run_changed && owed.is_some_and(|o| !o.mail_unread);
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
    let first_prompt = if ends_here { None } else { is_free(&seen, Some(cursor), agent, completes) };
    let out = if ends_here {
        Ok(WakeOutcome::RunChanged)
    } else if first_prompt.is_none() {
        Ok(WakeOutcome::NotFree { row: cursor_row_trace(client.screen()) })
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
        let live = if still { is_free(&free_test_lines(client.screen()), Some(client.screen().cursor_position()), agent, completes) } else { None };
        match live {
            Some(prompt) if Some(prompt) == first_prompt => {
                mark("final-ok");
                // Test seam: holds the attempt this many milliseconds between its last check and its typing, so a test can
                // replace the run in between. Inert unless named.
                if let Some(ms) = std::env::var("SOT_TEST_WAKE_HOLD_AFTER_FINAL_MS").ok().and_then(|s| s.parse::<u64>().ok()) {
                    std::thread::sleep(Duration::from_millis(ms));
                }
                let read = typing_read(&client);
                let (sent, out) = if prompt == Prompt::HoldsLine {
                    tracing::info!("comm wake: Enter for the wake line left in main's input box");
                    ("enter", wake_outcome(send_enter(&mut client, Some(&run), op_budget)))
                } else {
                    ("line and enter", type_then_enter(&mut client, line, agent, &run, op_budget))
                };
                log_typing(handle, sent, &read, &out);
                Ok(out)
            }
            _ => Ok(WakeOutcome::NotFree { row: cursor_row_trace(client.screen()) }),
        }
    };
    client.shutdown(SHUTDOWN_WAIT);
    mark("done");
    out.map(|outcome| Attempted { outcome, run })
}

/// What the gate read at the moment of typing: the cursor's row ([`cursor_row_trace`]) and the rows of the box around
/// it, the row above through the row below, as [`free_test_lines`] gave them to the gate.
fn typing_read(client: &Client) -> (String, Vec<String>) {
    let lines = free_test_lines(client.screen());
    let at = client.screen().cursor_position().0 as usize;
    let rows = lines.get(at.saturating_sub(1)..(at + 2).min(lines.len())).unwrap_or_default().to_vec();
    (cursor_row_trace(client.screen()), rows)
}

/// The one INFO event per typing: the row's handle, what was sent, what the gate read and the outcome. Screen text
/// is logged as data (`{:?}` escapes it), never as part of the message.
fn log_typing(handle: &str, sent: &str, read: &(String, Vec<String>), outcome: &WakeOutcome) {
    tracing::info!(handle, sent, cursor_row = %read.0, box_rows = ?read.1, outcome = ?outcome, "comm wake: typed into the row");
}

/// Types `line`, pumps until the typed-line gate passes or `op_budget` has passed since the write returned, then
/// writes Enter. A line that never shows alone gets no Enter ([`WakeOutcome::TypedNoEnter`]).
fn type_then_enter(client: &mut Client, line: &str, agent: &str, run: &str, op_budget: Duration) -> WakeOutcome {
    if let Err(e) = send_text(client, line.as_bytes(), Some(run), op_budget) {
        return text_failed(e);
    }
    let deadline = Instant::now() + op_budget;
    loop {
        client.pump();
        let lines = free_test_lines(client.screen());
        let cursor = Some(client.screen().cursor_position());
        match super::screen::typed_refusal(&lines, cursor, agent, cfg!(windows), line) {
            None => return wake_outcome(send_enter(client, Some(run), op_budget)),
            Some(reason) if Instant::now() >= deadline => {
                let border = cursor.and_then(|(row, _)| lines.get((row as usize).checked_sub(1)?)).cloned().unwrap_or_default();
                return WakeOutcome::TypedNoEnter { reason, border, row: cursor_row_trace(client.screen()) };
            }
            Some(_) => std::thread::sleep(POLL_INTERVAL),
        }
    }
}

#[cfg(test)]
#[path = "attempt_tests.rs"]
mod tests;
