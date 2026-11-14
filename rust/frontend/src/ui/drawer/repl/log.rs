//! REPL submissions, draft editing, history and interrupts, plus thin State send adapters.

use super::{
    DraftEdit, ReplEffects, ReplInterrupt, ReplModel, ReplSend, ReplTarget, Submission,
    SubmissionKind,
};
use crate::ui::*;
use std::time::{Duration, Instant};

/// One submitted REPL eval — code typed by the user plus the frames the
/// kernel returned. `in_flight` means we sent the request but haven't seen
/// the response yet; the chrome renders an italicised `(running…)` until
/// the matching `ReplEvalDone` lands.
#[derive(Clone)]
pub(in crate::ui) struct ReplEntry {
    pub(in crate::ui) eval_id: u64,
    pub(in crate::ui) code: String,
    pub(in crate::ui) frames: Vec<ReplFrame>,
    pub(in crate::ui) elapsed_ms: u64,
    pub(in crate::ui) in_flight: bool,
    /// Which prompt the user was on when this entry was submitted —
    /// `false` = `julia>`, `true` = `pkg>`. Stored per-entry so a
    /// later mode switch doesn't relabel old scrollback rows.
    pub(in crate::ui) pkg_mode: bool,
    /// `Some(label)` for a run this FE did NOT originate — a session's
    /// `repl.execute` (ADR 0033 phase 2). Renders a distinct prompt line
    /// (e.g. `⟨session ▸ run foo.jl⟩`) instead of the `julia>` echo, and participates
    /// in Up/Down history once complete (including its empty synthetic code). `None` for a local user eval.
    pub(in crate::ui) origin: Option<String>,
}

impl ReplModel {
    fn next_eval_id(&mut self) -> u64 {
        self.count = self.count.saturating_add(1);
        self.count
    }

    fn register(&mut self, target: &ReplTarget, code: String, pkg_mode: bool) -> u64 {
        let eval_id = self.next_eval_id();
        self.routes
            .insert((target.host.clone(), eval_id), target.view_key.clone());
        if self.live.log.len() >= 256 {
            let excess = self.live.log.len() - 255;
            self.live.log.drain(0..excess);
        }
        self.live.log.push(ReplEntry {
            eval_id,
            code,
            frames: Vec::new(),
            elapsed_ms: 0,
            in_flight: true,
            pkg_mode,
            origin: None,
        });
        eval_id
    }

    pub(in crate::ui) fn begin_eval(&mut self, target: ReplTarget) -> Option<ReplSend> {
        let code = std::mem::take(&mut self.live.input);
        if code.trim().is_empty() {
            return None;
        }
        self.live.history_pos = None;
        self.live.history_saved = None;
        let pkg_mode = self.live.pkg_mode;
        let eval_id = self.register(&target, code.clone(), pkg_mode);
        let request = OutgoingReq::ReplEval {
            eval_id,
            code,
            mode: pkg_mode.then(|| "pkg".to_string()),
            workspace_id: target.workspace_id,
        };
        Some(ReplSend {
            host: target.host,
            request,
            submission: Submission {
                eval_id,
                kind: SubmissionKind::Eval,
            },
        })
    }

    /// A delayed demo replaces the draft and submits without moving the view, even for whitespace.
    pub(in crate::ui) fn begin_demo_eval(
        &mut self,
        target: ReplTarget,
        code: String,
    ) -> Option<ReplSend> {
        self.live.input = code;
        self.begin_eval(target)
    }

    /// The caller validates the cursored .jl path before beginning a run.
    pub(in crate::ui) fn begin_file_run(
        &mut self,
        target: ReplTarget,
        path: String,
        fresh: bool,
    ) -> ReplSend {
        if fresh {
            self.live.log.clear();
            self.scroll_to_tail();
            self.live.pkg_mode = false;
            self.live.history_pos = None;
            self.live.history_saved = None;
        }
        let basename = path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string();
        let code = format!("{} {path}", if fresh { "r" } else { "R" });
        let eval_id = self.register(&target, code, false);
        ReplSend {
            host: target.host,
            request: OutgoingReq::ReplRunFile {
                eval_id,
                path,
                fresh,
                workspace_id: target.workspace_id,
            },
            submission: Submission {
                eval_id,
                kind: SubmissionKind::File { basename, fresh },
            },
        }
    }

    pub(in crate::ui) fn complete_submission(
        &mut self,
        submission: Submission,
        result: Result<(), String>,
    ) -> ReplEffects {
        if let Err(message) = &result {
            match &submission.kind {
                SubmissionKind::Eval => {
                    tracing::warn!(error = %message, eval_id = submission.eval_id(), "drop repl.eval request — channel closed");
                }
                SubmissionKind::File { .. } => {
                    tracing::warn!(error = %message, "failed to dispatch repl.run_file");
                }
            }
            if let Some(entry) = self
                .live
                .log
                .iter_mut()
                .find(|e| e.eval_id == submission.eval_id)
            {
                entry.in_flight = false;
                entry.frames.push(ReplFrame::Error {
                    message: format!("transport channel closed: {message}"),
                    stacktrace: Vec::new(),
                });
            }
        }
        // A failed send historically retains its routing registration.
        let status = match submission.kind {
            SubmissionKind::Eval => None,
            SubmissionKind::File { basename, fresh } => Some(if result.is_err() {
                format!("repl.run_file '{basename}' failed · channel closed")
            } else if fresh {
                format!("running '{basename}' (resetting REPL …)")
            } else {
                format!("running '{basename}' (existing repl)")
            }),
        };
        ReplEffects {
            status,
            ..Default::default()
        }
    }

    pub(in crate::ui) fn edit_draft(&mut self, edit: DraftEdit<'_>) {
        match edit {
            DraftEdit::Typed("]") if self.live.input.is_empty() && !self.live.pkg_mode => {
                self.live.pkg_mode = true
            }
            DraftEdit::Typed(text) => self
                .live
                .input
                .extend(text.chars().filter(|c| !c.is_control())),
            DraftEdit::Paste(text) => self
                .live
                .input
                .push_str(&text.replace("\r\n", "\n").replace('\r', "\n")),
            DraftEdit::Newline => self.live.input.push('\n'),
            DraftEdit::Space => self.live.input.push(' '),
            DraftEdit::Backspace if self.live.input.is_empty() && self.live.pkg_mode => {
                self.live.pkg_mode = false
            }
            DraftEdit::Backspace => {
                self.live.input.pop();
            }
            DraftEdit::Replace(text) => self.live.input = text,
        }
        self.scroll_to_tail();
    }

    pub(in crate::ui) fn history_back(&mut self) -> bool {
        let row = &mut self.live;
        if let Some(line) = history_step_back(
            &row.log,
            &mut row.history_pos,
            &mut row.history_saved,
            &row.input,
        ) {
            row.input = line;
            self.scroll_to_tail();
            true
        } else {
            false
        }
    }

    pub(in crate::ui) fn history_forward(&mut self) -> bool {
        let row = &mut self.live;
        if let Some(line) =
            history_step_forward(&row.log, &mut row.history_pos, &mut row.history_saved)
        {
            row.input = line;
            self.scroll_to_tail();
            true
        } else {
            false
        }
    }

    pub(in crate::ui) fn clear_scrollback(&mut self) -> ReplEffects {
        self.live.log.clear();
        self.scroll_to_tail();
        ReplEffects {
            clear_images: true,
            status: Some("repl · scrollback cleared".into()),
            redraw: true,
            ..Default::default()
        }
    }

    /// A person's page, half page or wheel step, `rows_above` older (negative toward the tail): a row at a time over
    /// `length`, at once when it is zero, by the page arithmetic.
    pub(in crate::ui) fn scroll_rows(&mut self, rows_above: i32, now: Instant, length: Duration) {
        let row = &mut self.live;
        self.roll.ask(rows_above, now, length, |rows| page_step(&mut row.scroll, rows));
    }
    /// A person's one-line step (Alt+Up, Alt+Down), by the line arithmetic when it is applied at once.
    pub(in crate::ui) fn scroll_line(&mut self, older: bool, now: Instant, length: Duration) {
        let row = &mut self.live;
        self.roll.ask(if older { 1 } else { -1 }, now, length, |rows| line_step(&mut row.scroll, rows));
    }
    /// One frame's scroll for the build: the rows the roll has reached go in first, so the build never reads an
    /// offset the frame has not spent (the commit writes the built value back, and would undo a later spend); `cap`
    /// is the drawer's rows. Answers the scroll and whether frames must keep coming.
    pub(in crate::ui) fn frame_scroll(&mut self, now: Instant, cap: f32) -> (u16, bool) {
        let row = &mut self.live;
        let moving = self.roll.spend(now, cap, |rows| page_step(&mut row.scroll, rows));
        (self.live.scroll, moving)
    }
    pub(super) fn finish_scroll(&mut self) {
        let row = &mut self.live;
        self.roll.finish(|rows| page_step(&mut row.scroll, rows));
    }
    /// The scroll the view showed: `shown`, the render's clamp of what was `built`. A clamp is the top, and a roll
    /// toward it stops there.
    pub(in crate::ui) fn commit_rendered_scroll(&mut self, built: u16, shown: u16) {
        if shown < built {
            self.roll.met_end();
        }
        self.live.scroll = shown;
    }
    /// A move to the tail by another rule (an edit, a submit, Ctrl+L, a fresh run): nothing stays owed.
    pub(in crate::ui) fn scroll_to_tail(&mut self) {
        self.roll.stop();
        self.live.scroll = 0;
    }

    pub(in crate::ui) fn in_flight_ids(&self) -> Vec<u64> {
        self.live
            .log
            .iter()
            .filter(|e| e.in_flight)
            .map(|e| e.eval_id)
            .collect()
    }

    pub(in crate::ui) fn prepare_interrupt(&mut self, target: ReplTarget) -> Option<ReplInterrupt> {
        let ids = self.in_flight_ids();
        if ids.is_empty() {
            self.live.input.clear();
            self.live.pkg_mode = false;
            return None;
        }
        let force = self.interrupted == ids;
        Some(ReplInterrupt {
            host: target.host,
            workspace_id: target.workspace_id,
            ids,
            force,
        })
    }

    pub(in crate::ui) fn complete_interrupt(
        &mut self,
        sent: Option<ReplInterrupt>,
        success: bool,
    ) -> ReplEffects {
        let mut effects = ReplEffects {
            redraw: true,
            ..Default::default()
        };
        if let Some(sent) = sent.filter(|_| success) {
            tracing::info!(force = sent.force, "repl.interrupt dispatched (Ctrl+C)");
            self.interrupted = sent.ids;
            effects.status = Some(if sent.force {
                "forcing the interrupt... (can crash or hang the REPL inside C code; r on a .jl file restarts it)"
            } else {
                "interrupting... (Ctrl+C again forces it if the eval does not stop)"
            }.to_string());
        }
        self.scroll_to_tail();
        effects
    }
}

impl State {
    pub(in crate::ui) fn repl_target(&self) -> ReplTarget {
        ReplTarget {
            host: self.selected.host().clone(),
            workspace_id: self.selected.workspace_id().clone(),
            view_key: self.active_ws_key(),
        }
    }

    pub(in crate::ui) fn submit_repl_input(&mut self) {
        let target = self.repl_target();
        let plan = self.repl_model.begin_eval(target);
        self.send_repl_eval(plan);
    }

    pub(in crate::ui) fn submit_demo_repl_input(&mut self, code: String) {
        let target = self.repl_target();
        let plan = self.repl_model.begin_demo_eval(target, code);
        self.send_repl_eval(plan);
    }

    fn send_repl_eval(&mut self, plan: Option<ReplSend>) {
        let Some(plan) = plan else { return };
        let result = self
            .send_to(&plan.host, plan.request)
            .map_err(|e| e.to_string());
        let effects = self.repl_model.complete_submission(plan.submission, result);
        self.apply_repl_effects(effects);
    }
}

/// Page, half-page and wheel arithmetic converts after the integer calculation, as before (it wraps past the top of
/// `u16`); the rendering clamps it. The offset after, as `RingRoll` reads it.
fn page_step(scroll: &mut u16, rows: i32) -> usize {
    *scroll = (*scroll as i32 + rows).max(0) as u16;
    *scroll as usize
}

/// A line step saturates.
fn line_step(scroll: &mut u16, rows: i32) -> usize {
    let step = u16::try_from(rows.unsigned_abs()).unwrap_or(u16::MAX);
    *scroll = if rows >= 0 { scroll.saturating_add(step) } else { scroll.saturating_sub(step) };
    *scroll as usize
}

/// Walk one step backward (older) through the REPL history. Returns
/// the code of the entry now selected, or `None` if there's nothing
/// older to step to. On the first step of a walk the current
/// `input` is saved into `saved` so a later forward-walk past the
/// newest entry can restore it.
///
/// In-flight entries are skipped — replaying a still-running line
/// would be confusing, and an `in_flight=true` entry is usually the
/// one the user just submitted anyway.
fn history_step_back(
    log: &[ReplEntry],
    pos: &mut Option<usize>,
    saved: &mut Option<String>,
    input: &str,
) -> Option<String> {
    let candidates: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.in_flight)
        .map(|(i, _)| i)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let new_pos = match *pos {
        None => {
            *saved = Some(input.to_string());
            candidates.len() - 1
        }
        Some(0) => return None,
        Some(p) => p - 1,
    };
    *pos = Some(new_pos);
    Some(log[candidates[new_pos]].code.clone())
}

/// Walk one step forward (newer). Returns the code of the entry now
/// selected, or — when walking past the newest entry — the saved
/// in-progress buffer (which also exits the walk by clearing `pos`
/// and `saved`). Returns `None` when not currently walking.
fn history_step_forward(
    log: &[ReplEntry],
    pos: &mut Option<usize>,
    saved: &mut Option<String>,
) -> Option<String> {
    let p = (*pos)?;
    let candidates: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.in_flight)
        .map(|(i, _)| i)
        .collect();
    if p + 1 >= candidates.len() {
        let restored = saved.take().unwrap_or_default();
        *pos = None;
        return Some(restored);
    }
    let new_pos = p + 1;
    *pos = Some(new_pos);
    Some(log[candidates[new_pos]].code.clone())
}

#[cfg(test)]
#[path = "log_tests.rs"]
mod tests;
