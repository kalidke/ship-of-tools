//! REPL submissions, draft editing, history and interrupts, plus thin State send adapters.

use super::{
    DraftEdit, ReplEffects, ReplInterrupt, ReplModel, ReplSend, ReplTarget, Submission,
    SubmissionKind,
};
use crate::ui::*;

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

    /// The caller validates the cursored .jl path before beginning a run.
    pub(in crate::ui) fn begin_file_run(
        &mut self,
        target: ReplTarget,
        path: String,
        fresh: bool,
    ) -> ReplSend {
        if fresh {
            self.live.log.clear();
            self.live.scroll = 0;
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
            tracing::warn!(error = %message, eval_id = submission.eval_id(), "drop REPL request — channel closed");
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

    pub(in crate::ui) fn edit_draft(&mut self, edit: DraftEdit<'_>) -> bool {
        let before = (self.live.input.len(), self.live.pkg_mode, self.live.scroll);
        let replacement_changed =
            matches!(&edit, DraftEdit::Replace(text) if text != &self.live.input);
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
        replacement_changed
            || before != (self.live.input.len(), self.live.pkg_mode, self.live.scroll)
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
            row.scroll = 0;
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
            row.scroll = 0;
            true
        } else {
            false
        }
    }

    pub(in crate::ui) fn clear_scrollback(&mut self) -> ReplEffects {
        self.live.log.clear();
        self.live.scroll = 0;
        ReplEffects {
            clear_images: true,
            status: Some("repl · scrollback cleared".into()),
            redraw: true,
            ..Default::default()
        }
    }

    /// Page, half-page and wheel arithmetic converts after the integer calculation, as before.
    pub(in crate::ui) fn scroll_rows(&mut self, rows_above: i32) {
        self.live.scroll = (self.live.scroll as i32 + rows_above).max(0) as u16;
    }
    pub(in crate::ui) fn scroll_line(&mut self, older: bool) {
        self.live.scroll = if older {
            self.live.scroll.saturating_add(1)
        } else {
            self.live.scroll.saturating_sub(1)
        };
    }
    pub(in crate::ui) fn commit_rendered_scroll(&mut self, scroll: u16) {
        self.live.scroll = scroll;
    }
    pub(in crate::ui) fn scroll_to_tail(&mut self) {
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
            host: self.active_host.clone(),
            workspace_id: self.active_workspace_id.clone(),
            view_key: self.active_ws_key(),
        }
    }

    pub(in crate::ui) fn submit_repl_input(&mut self) {
        let target = self.repl_target();
        let Some(plan) = self.repl_model.begin_eval(target) else {
            return;
        };
        let result = self
            .send_to(&plan.host, plan.request)
            .map_err(|e| e.to_string());
        let effects = self.repl_model.complete_submission(plan.submission, result);
        self.apply_repl_effects(effects);
    }
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
