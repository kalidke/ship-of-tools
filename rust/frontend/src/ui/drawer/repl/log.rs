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

/// The REPL drawer across rows: the pane each row left (`parked`, shown again when the row is), and this window's one
/// eval count, which is no row's: a row switch never changes it, so no two of the window's runs in flight share a
/// number, and the daemon writes each run to the window under its own number (ADR 0033, the one-numberer amendment).
#[derive(Default)]
pub(in crate::ui) struct ReplRows {
    /// Each row's pane as the window left it, keyed like every per-row map (`WsKey`).
    pub(in crate::ui) parked: HashMap<WsKey, WorkspaceReplSnapshot>,
    count: u64,
}

impl ReplRows {
    /// The number of this window's next eval or `r`/`R` run.
    pub(in crate::ui) fn next_eval_id(&mut self) -> u64 {
        self.count = self.count.saturating_add(1);
        self.count
    }

    /// Parks the pane of `row`, which the window is leaving.
    pub(in crate::ui) fn park(&mut self, row: WsKey, pane: WorkspaceReplSnapshot) {
        self.parked.insert(row, pane);
    }

    /// The pane `row` was left with, to show it again; `None` on the row's first visit.
    pub(in crate::ui) fn unpark(&self, row: &WsKey) -> Option<WorkspaceReplSnapshot> {
        self.parked.get(row).cloned()
    }
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
        let eval_id = self.repl_rows.next_eval_id();
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

/// The eval ids of every entry still in flight: the evals a Ctrl+C means. An
/// entry whose done frame was lost stays in flight, so the interrupt names them
/// all and reaches the running eval if it is any of them.
pub(in crate::ui) fn in_flight_eval_ids(log: &[ReplEntry]) -> Vec<u64> {
    log.iter()
        .filter(|e| e.in_flight)
        .map(|e| e.eval_id)
        .collect()
}

/// Whether this Ctrl+C forces the interrupt: the evals in flight are the ones the last Ctrl+C interrupted, so the
/// ordinary signal it sent did not stop them.
pub(in crate::ui) fn repl_interrupt_forced(last: &[u64], in_flight: &[u64]) -> bool {
    !in_flight.is_empty() && last == in_flight
}

#[cfg(test)]
#[path = "log_tests.rs"]
mod tests;
