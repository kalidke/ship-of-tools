//! The REPL log: one entry per submitted eval, the submit path and the Up/Down input history.

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
    /// (e.g. `⟨session ▸ run foo.jl⟩`) instead of the `julia>` echo, and is
    /// kept out of the Up/Down input history. `None` for a local user eval.
    pub(in crate::ui) origin: Option<String>,
}

impl State {
    /// Send the current REPL input buffer to the backend as a `repl.eval`
    /// request, push an in-flight entry into the scrollback, and clear the
    /// input. Empty input is a no-op (no point round-tripping whitespace).
    /// The eval_id is locally generated; the response handler reconciles
    /// by matching on it.
    /// ADR 0042 L2a note: the drawer's Repl content, unlike Terminal and
    /// Monitor, is deliberately NOT pinned to `default_host` — it's the
    /// per-workspace Julia REPL (keyed by `active_workspace_id`, one per
    /// workspace, `repl_lifecycle` tracks each host's independently), so it
    /// correctly follows `active_host`/`self.send` like every other
    /// workspace-scoped operation. "No drawer host switching" refers to the
    /// drawer's fixed TENANT (ADR 0041: one drawer, terminal/monitor/julia
    /// + the SoT LLM) and its Terminal/Monitor content's home connection —
    /// not to which workspace's REPL the drawer happens to be showing.
    pub(in crate::ui) fn submit_repl_input(&mut self) {
        let code = std::mem::take(&mut self.repl_input);
        if code.trim().is_empty() {
            return;
        }
        // Submit exits any active history walk — the saved buffer is
        // discarded because the user committed to this line.
        self.history_pos = None;
        self.history_saved = None;
        self.repl_eval_counter = self.repl_eval_counter.saturating_add(1);
        let eval_id = self.repl_eval_counter;
        // Tag the eval with the host+workspace it belongs to so a
        // `ReplEvalDone` reply arriving after a swap routes back to the
        // right log -- and, since this key also disambiguates which
        // HOST'S eval_id 1 this is, to the right host's log too.
        let owner_key: HostKey = self.active_host.clone();
        let workspace_key = self.active_ws_key();
        self.eval_id_workspace
            .insert((owner_key, eval_id), workspace_key.clone());
        // Bound the scrollback at a reasonable cap so a long session
        // doesn't accumulate forever. Trim from the front.
        if self.repl_log.len() >= 256 {
            let excess = self.repl_log.len() - 255;
            self.repl_log.drain(0..excess);
        }
        let pkg_mode = self.repl_pkg_mode;
        self.repl_log.push(ReplEntry {
            eval_id,
            code: code.clone(),
            frames: Vec::new(),
            elapsed_ms: 0,
            in_flight: true,
            pkg_mode,
            origin: None,
        });
        let mode = if pkg_mode {
            Some("pkg".to_string())
        } else {
            None
        };
        if let Err(e) = self.send(crate::net::transport::OutgoingReq::ReplEval {
            eval_id,
            code,
            mode,
            workspace_id: self.active_workspace_id.clone(),
        }) {
            tracing::warn!(error = %e, eval_id, "drop repl.eval request — channel closed");
            // Mark the entry done with an error frame so the user sees
            // why nothing happened.
            if let Some(entry) = self.repl_log.iter_mut().find(|e| e.eval_id == eval_id) {
                entry.in_flight = false;
                entry.frames.push(sot_protocol::ReplFrame::Error {
                    message: format!("transport channel closed: {e}"),
                    stacktrace: Vec::new(),
                });
            }
        }
    }

    pub(in crate::ui) fn history_step_back(&mut self) -> Option<String> {
        history_step_back(
            &self.repl_log,
            &mut self.history_pos,
            &mut self.history_saved,
            &self.repl_input,
        )
    }

    pub(in crate::ui) fn history_step_forward(&mut self) -> Option<String> {
        history_step_forward(
            &self.repl_log,
            &mut self.history_pos,
            &mut self.history_saved,
        )
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
mod tests {
    use super::*;

    fn entry(eval_id: u64, code: &str, in_flight: bool) -> ReplEntry {
        ReplEntry {
            eval_id,
            code: code.to_string(),
            frames: Vec::new(),
            elapsed_ms: 0,
            in_flight,
            pkg_mode: false,
            origin: None,
        }
    }

    #[test]
    fn history_step_back_returns_none_on_empty_log() {
        let log: Vec<ReplEntry> = Vec::new();
        let mut pos = None;
        let mut saved = None;
        assert_eq!(history_step_back(&log, &mut pos, &mut saved, "draft"), None);
        assert!(pos.is_none());
        assert!(saved.is_none());
    }

    #[test]
    fn history_step_back_skips_in_flight_entries() {
        let log = vec![entry(1, "x = 1", false), entry(2, "x = 2", true)];
        let mut pos = None;
        let mut saved = None;
        // Only the completed entry is a candidate, so back lands on it.
        assert_eq!(
            history_step_back(&log, &mut pos, &mut saved, "draft").as_deref(),
            Some("x = 1")
        );
        // Saved on entry to the walk.
        assert_eq!(saved.as_deref(), Some("draft"));
        // No older entry; second back is a no-op.
        assert_eq!(history_step_back(&log, &mut pos, &mut saved, "x = 1"), None);
    }

    #[test]
    fn history_step_back_walks_oldest_to_newest_in_reverse() {
        let log = vec![
            entry(1, "first", false),
            entry(2, "second", false),
            entry(3, "third", false),
        ];
        let mut pos = None;
        let mut saved = None;
        assert_eq!(
            history_step_back(&log, &mut pos, &mut saved, "").as_deref(),
            Some("third")
        );
        assert_eq!(
            history_step_back(&log, &mut pos, &mut saved, "third").as_deref(),
            Some("second")
        );
        assert_eq!(
            history_step_back(&log, &mut pos, &mut saved, "second").as_deref(),
            Some("first")
        );
        // At the oldest; further back returns None and pos stays put.
        assert_eq!(history_step_back(&log, &mut pos, &mut saved, "first"), None);
        assert_eq!(pos, Some(0));
    }

    #[test]
    fn history_step_forward_no_op_when_not_walking() {
        let log = vec![entry(1, "a", false)];
        let mut pos = None;
        let mut saved = None;
        assert_eq!(history_step_forward(&log, &mut pos, &mut saved), None);
    }

    #[test]
    fn history_step_forward_past_newest_restores_saved_buffer() {
        let log = vec![entry(1, "first", false), entry(2, "second", false)];
        let mut pos = None;
        let mut saved = None;
        // Walk back twice; saved captures the original draft.
        let _ = history_step_back(&log, &mut pos, &mut saved, "in-progress");
        let _ = history_step_back(&log, &mut pos, &mut saved, "second");
        assert_eq!(pos, Some(0));
        // Forward once: back to "second".
        assert_eq!(
            history_step_forward(&log, &mut pos, &mut saved).as_deref(),
            Some("second")
        );
        // Forward again: past newest, restore "in-progress", exit walk.
        assert_eq!(
            history_step_forward(&log, &mut pos, &mut saved).as_deref(),
            Some("in-progress")
        );
        assert!(pos.is_none());
        assert!(saved.is_none());
    }

    #[test]
    fn history_step_forward_with_no_saved_buffer_yields_empty_string() {
        // Shouldn't normally happen (saved is set on first back), but
        // verify the unwrap_or_default fallback doesn't panic.
        let log = vec![entry(1, "x", false)];
        let mut pos = Some(0);
        let mut saved: Option<String> = None;
        assert_eq!(
            history_step_forward(&log, &mut pos, &mut saved).as_deref(),
            Some("")
        );
        assert!(pos.is_none());
    }
}
