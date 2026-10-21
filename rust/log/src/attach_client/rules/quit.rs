//! Ruling (a): the one quit dispatcher (`QuitDispatcher`).

use crate::lane::wire::{SupervisorOperationState, SupervisorRefusedReason};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------
// (a) One quit dispatcher, waiting for `record_closed` then `record_verified`
// ---------------------------------------------------------------------

/// Bounds how long the "ending session" window waits before switching to
/// "outcome unknown" — ADR 0041's own pinned bound-graph row: "FE quit |
/// DERIVED `fence acquisition` (90 s today) → 'outcome unknown'." NOT
/// the ordinary lane-operation reply budget (Lifecycle "reply read 5 s"):
/// `end_run`'s own reply is deliberately DEFERRED until real, slow OS
/// work completes underneath it — killing the process, closing the
/// ConPTY, sealing the voyage, then verifying it — and the ADR's own
/// bound-graph table derives this EXACT figure (`readiness + kill wait +
/// 20 s`, 90 s today) for precisely that wait, not a number this crate
/// invents independently.
pub const QUIT_CUTOFF: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuitState {
    Idle,
    /// `end_run` sent, holding the window open, waiting for its own
    /// reply (`record_closed`, per Lifecycle "the COMMAND reply arrives
    /// at record_closed").
    Ending { operation_id: String },
    /// `record_closed` arrived; now polling `query` for `record_verified`
    /// — Lifecycle: "`record_verified` follows through `query`."
    Verifying { operation_id: String },
    /// `record_verified` observed — the caller may now actually exit.
    Ended,
    /// The operation reached an explicit `Failed{detail}` reply — a
    /// concrete, immediate terminal outcome, never held for the cutoff.
    Failed { detail: String },
    /// The operation reached an explicit `Refused{reason}` reply
    /// (`stale_voyage` in practice) — likewise immediate.
    Refused { reason: SupervisorRefusedReason },
    /// The cutoff expired first: "the window STAYS UP and says 'ending
    /// the session did not complete — outcome unknown'."
    OutcomeUnknown,
}

/// ADR 0041 ruling (a): every user-requested exit routes through exactly
/// one of these, latched so a second quit press cannot fire a second
/// `end_run` while one is already outstanding.
#[derive(Debug)]
pub struct QuitDispatcher {
    state: QuitState,
    started_at: Option<Instant>,
}

impl Default for QuitDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl QuitDispatcher {
    pub fn new() -> Self {
        Self { state: QuitState::Idle, started_at: None }
    }

    pub fn state(&self) -> &QuitState {
        &self.state
    }

    /// Starts the ending transaction. Returns `true` iff THIS call is
    /// the one that should send `end_run` (idempotent against repeated
    /// quit presses while already ending/verifying/ended/unknown).
    pub fn request_quit(&mut self, operation_id: String, now: Instant) -> bool {
        if matches!(self.state, QuitState::Idle) {
            self.state = QuitState::Ending { operation_id };
            self.started_at = Some(now);
            true
        } else {
            false
        }
    }

    /// The operation id this dispatcher is currently waiting on a reply
    /// for (either the initial command or a subsequent `query`) —
    /// `Some` while `Ending` or `Verifying`, `None` otherwise. The
    /// runtime uses this to know whether it should keep polling `query`.
    pub fn operation_id(&self) -> Option<&str> {
        match &self.state {
            QuitState::Ending { operation_id } | QuitState::Verifying { operation_id } => Some(operation_id),
            _ => None,
        }
    }

    /// Applies a `SupervisorOperationState` reply — from the `end_run`
    /// command's own reply OR a later `query` — to the dispatcher.
    /// `RecordClosed` moves `Ending -> Verifying` (never exits yet);
    /// `RecordVerified` is the ONLY thing that reaches `Ended`, from
    /// either `Ending` (a fast authority that verified before this
    /// dispatcher ever queried) or `Verifying`. `Failed`/`Refused`
    /// surface immediately as their own terminal states — "never waits
    /// for the cutoff." Anything else (`Accepted`, `Stopping`,
    /// `ResetDone`, `UnknownOperation`) is not a state `end_run`'s own
    /// command/query ever legitimately answers with here, so it is
    /// ignored rather than mis-transitioned.
    pub fn on_operation_state(&mut self, state: SupervisorOperationState) {
        let waiting = matches!(self.state, QuitState::Ending { .. } | QuitState::Verifying { .. });
        if !waiting {
            return;
        }
        match state {
            SupervisorOperationState::RecordClosed => {
                if let QuitState::Ending { operation_id } = &self.state {
                    self.state = QuitState::Verifying { operation_id: operation_id.clone() };
                }
            }
            SupervisorOperationState::RecordVerified => {
                self.state = QuitState::Ended;
            }
            SupervisorOperationState::Failed { detail } => {
                self.state = QuitState::Failed { detail };
            }
            SupervisorOperationState::Refused { reason } => {
                self.state = QuitState::Refused { reason };
            }
            SupervisorOperationState::Accepted
            | SupervisorOperationState::Stopping
            | SupervisorOperationState::ResetDone { .. }
            | SupervisorOperationState::UnknownOperation => {}
        }
    }

    /// Advances the cutoff clock; call once per tick while `Ending` or
    /// `Verifying`.
    pub fn tick(&mut self, now: Instant) {
        let waiting = matches!(self.state, QuitState::Ending { .. } | QuitState::Verifying { .. });
        if let (true, Some(started)) = (waiting, self.started_at) {
            if now.duration_since(started) >= QUIT_CUTOFF {
                self.state = QuitState::OutcomeUnknown;
            }
        }
    }

    pub fn should_exit(&self) -> bool {
        matches!(self.state, QuitState::Ended)
    }

    /// The visible window message while ending, verifying, or after a
    /// terminal outcome; `None` once idle/ended (nothing to show).
    pub fn message(&self) -> Option<String> {
        match &self.state {
            QuitState::Ending { .. } => Some("ending session\u{2026}".to_string()),
            QuitState::Verifying { .. } => Some("verifying the session ended\u{2026}".to_string()),
            QuitState::OutcomeUnknown => {
                Some("ending the session did not complete \u{2014} outcome unknown".to_string())
            }
            QuitState::Failed { detail } => Some(format!("ending the session failed: {detail}")),
            QuitState::Refused { reason } => Some(format!("ending the session was refused: {reason:?}")),
            QuitState::Idle | QuitState::Ended => None,
        }
    }
}

#[cfg(test)]
#[path = "quit_tests.rs"]
mod tests;
