//! Ruling (d): reconnect backoff and the terminal decision (`ReconnectState`).

use crate::lane::wire::SupervisorPhase;
use crate::host::redial::Redial;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------
// (d) Reconnect is bounded and classified
// ---------------------------------------------------------------------

/// Why a reconnect episode is TERMINAL — an actionable error offering
/// retry and reset, never silently retried again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalReason {
    HelloRefusedVersionSkew,
    ForeignPipe,
    AccessDenied,
    OperatorCancel,
    /// A still-answering authority's own terminal phase.
    SupervisorPhase(SupervisorPhase),
    /// The voyage pipe is absent while the supervisor lane is absent OR
    /// unresponsive, sustained for the whole [`HEALTH_WINDOW`].
    HealthWindowExpired,
    /// ADR 0045 decision 4: a lane-bridge daemon refused `lane.connect`
    /// with a code this crate has no dedicated classifier for (not
    /// `unauthenticated`, which stays [`Self::AccessDenied`]) — `code`
    /// and `detail` ride through VERBATIM rather than collapsing into
    /// [`Self::ForeignPipe`]/[`Self::AccessDenied`], neither of which
    /// carries a field: an operator reading the pane line deserves the
    /// daemon's own diagnostic (`unknown_workspace`, `not_capsule`,
    /// `voyage_mismatch`, ...), not just a generic label.
    LaneRefused { code: String, detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectDecision {
    Retry,
    Terminal(TerminalReason),
}

/// The first wait of the doubling-to-[`RECONNECT_BACKOFF_CAP`] sequence
/// [`ReconnectState`] waits on platform's `Redial` (started over only after an
/// attach that lasted `STABLE`), AND the fixed interval at which the worker
/// polls a live supervisor connection's `Status` (ADR 0043 decision 28).
pub const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_millis(250);
pub const RECONNECT_BACKOFF_CAP: Duration = Duration::from_secs(4);

/// The bound above which "the voyage pipe absent while [the supervisor]
/// lane is absent OR UNRESPONSIVE" gives up (ADR 0041 Lifecycle
/// "Reconnect is bounded, classified..."). Pinned to the same 120 s
/// figure the ADR's own "Upgrade and version skew" section names as THE
/// health window ("readiness + stability, 120 s at today's provisional
/// values") — a distinct instance of the same named concept, not
/// `supervisor/`'s private `READINESS_CUTOFF + STABILITY_INTERVAL`
/// (that pair governs the LAUNCHER's rollback decision, a different
/// authority, and is not `pub`). Re-derive both together if the
/// provisional value ever changes.
pub const HEALTH_WINDOW: Duration = Duration::from_secs(120);

/// The reconnect episode's own classifier state: the redial pace (250 ms
/// doubling to 4 s, started over only after an attach that lasted `STABLE`),
/// and how long the "pipe absent, lane absent-or-unresponsive" condition
/// has been continuously observed.
#[derive(Debug)]
pub struct ReconnectState {
    redial: Redial,
    unresponsive_since: Option<Instant>,
}

impl Default for ReconnectState {
    fn default() -> Self {
        Self::new()
    }
}

impl ReconnectState {
    pub fn new() -> Self {
        Self { redial: Redial::new(RECONNECT_BACKOFF_INITIAL, RECONNECT_BACKOFF_CAP), unresponsive_since: None }
    }

    pub fn classify_hello_refused_version_skew(&mut self) -> ReconnectDecision {
        ReconnectDecision::Terminal(TerminalReason::HelloRefusedVersionSkew)
    }
    pub fn classify_foreign(&mut self) -> ReconnectDecision {
        ReconnectDecision::Terminal(TerminalReason::ForeignPipe)
    }
    pub fn classify_access_denied(&mut self) -> ReconnectDecision {
        ReconnectDecision::Terminal(TerminalReason::AccessDenied)
    }
    /// ADR 0045 decision 4: a lane-bridge refusal reaching a terminal
    /// classifier — everything EXCEPT `unauthenticated`, which stays
    /// [`Self::classify_access_denied`]. `code`/`detail` ride through to
    /// [`TerminalReason::LaneRefused`] verbatim.
    pub fn classify_lane_refused(&mut self, code: String, detail: String) -> ReconnectDecision {
        ReconnectDecision::Terminal(TerminalReason::LaneRefused { code, detail })
    }
    pub fn classify_operator_cancel(&mut self) -> ReconnectDecision {
        ReconnectDecision::Terminal(TerminalReason::OperatorCancel)
    }

    /// A STILL-ANSWERING supervisor lane reporting its own terminal
    /// phase — visible immediately, no timeout needed (the phase itself
    /// IS the proof).
    pub fn classify_supervisor_phase(&mut self, phase: SupervisorPhase) -> ReconnectDecision {
        match phase {
            SupervisorPhase::EndedNoRespawn | SupervisorPhase::Terminal => {
                ReconnectDecision::Terminal(TerminalReason::SupervisorPhase(phase))
            }
            _ => ReconnectDecision::Retry,
        }
    }

    /// The voyage pipe is absent AND the supervisor lane is absent or
    /// unresponsive — the ONE case that needs the timeout, since neither
    /// side can prove a terminal fact. The CALLER is responsible for
    /// only invoking this when BOTH halves of the conjunction are
    /// currently true (Codex review round, finding 8: a live attach
    /// pipe with an unresponsive supervisor must never reach this at
    /// all — see `attach_client/client.rs`'s own doc on where this is and is
    /// not called). `now` is checked against the FIRST time this
    /// condition was observed continuously.
    pub fn classify_unresponsive(&mut self, now: Instant) -> ReconnectDecision {
        let since = *self.unresponsive_since.get_or_insert(now);
        if now.duration_since(since) >= HEALTH_WINDOW {
            ReconnectDecision::Terminal(TerminalReason::HealthWindowExpired)
        } else {
            ReconnectDecision::Retry
        }
    }

    /// The unresponsive condition resolved (either half of the
    /// conjunction became true again) — clears the clock so a LATER
    /// unresponsive spell starts its own fresh window rather than
    /// inheriting an old one.
    pub fn clear_unresponsive(&mut self) {
        self.unresponsive_since = None;
    }

    /// A failed dial or attach step: the wait before the next attempt
    /// (250 ms doubling to 4 s), whether or not the row has ever attached —
    /// over an ssh lane every dial is a login (ADR 0043 decision 28).
    pub fn retry_with_backoff(&mut self) -> Duration {
        self.redial.after(Duration::ZERO)
    }

    /// An attached session that ended after `lasted`: the wait before the
    /// next episode. It starts over at 250 ms only when the session lasted
    /// `STABLE`; an attach that drops sooner keeps the doubling.
    pub fn retry_after_session(&mut self, lasted: Duration) -> Duration {
        self.redial.after(lasted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- (d) ReconnectState ---------------------------------------------

    #[test]
    fn backoff_doubles_from_a_fresh_state_and_only_a_stable_session_restarts_it() {
        // ADR 0043 decision 28: every failed dial doubles, whether or not the
        // row has ever attached — over an ssh lane each dial is a login.
        let mut r = ReconnectState::new();
        let waits: Vec<Duration> = (0..6).map(|_| r.retry_with_backoff()).collect();
        assert_eq!(
            waits,
            vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(4),
            ]
        );
        let short = crate::host::redial::STABLE - Duration::from_secs(1);
        assert_eq!(r.retry_after_session(short), Duration::from_secs(4), "an attach that drops sooner keeps the doubling");
        assert_eq!(r.retry_after_session(crate::host::redial::STABLE), Duration::from_millis(250));
    }

    #[test]
    fn post_attach_backoff_doubles_and_caps_at_4s() {
        let mut r = ReconnectState::new();
        let mut waits = vec![r.retry_after_session(crate::host::redial::STABLE)];
        for _ in 0..5 {
            waits.push(r.retry_with_backoff());
        }
        assert_eq!(
            waits,
            vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(4),
            ]
        );
    }

    #[test]
    fn terminal_cases_are_immediately_terminal() {
        let mut r = ReconnectState::new();
        assert_eq!(
            r.classify_hello_refused_version_skew(),
            ReconnectDecision::Terminal(TerminalReason::HelloRefusedVersionSkew)
        );
        assert_eq!(r.classify_foreign(), ReconnectDecision::Terminal(TerminalReason::ForeignPipe));
        assert_eq!(
            r.classify_access_denied(),
            ReconnectDecision::Terminal(TerminalReason::AccessDenied)
        );
        assert_eq!(
            r.classify_operator_cancel(),
            ReconnectDecision::Terminal(TerminalReason::OperatorCancel)
        );
    }

    #[test]
    fn supervisor_ended_no_respawn_and_terminal_are_terminal_ready_is_not() {
        let mut r = ReconnectState::new();
        assert_eq!(
            r.classify_supervisor_phase(SupervisorPhase::EndedNoRespawn),
            ReconnectDecision::Terminal(TerminalReason::SupervisorPhase(SupervisorPhase::EndedNoRespawn))
        );
        assert_eq!(
            r.classify_supervisor_phase(SupervisorPhase::Terminal),
            ReconnectDecision::Terminal(TerminalReason::SupervisorPhase(SupervisorPhase::Terminal))
        );
        assert_eq!(r.classify_supervisor_phase(SupervisorPhase::Ready), ReconnectDecision::Retry);
        assert_eq!(r.classify_supervisor_phase(SupervisorPhase::Starting), ReconnectDecision::Retry);
        assert_eq!(r.classify_supervisor_phase(SupervisorPhase::Ending), ReconnectDecision::Retry);
    }

    #[test]
    fn unresponsive_retries_until_the_health_window_then_goes_terminal() {
        let mut r = ReconnectState::new();
        let t0 = Instant::now();
        assert_eq!(r.classify_unresponsive(t0), ReconnectDecision::Retry);
        assert_eq!(r.classify_unresponsive(t0 + Duration::from_secs(60)), ReconnectDecision::Retry);
        assert_eq!(
            r.classify_unresponsive(t0 + HEALTH_WINDOW),
            ReconnectDecision::Terminal(TerminalReason::HealthWindowExpired)
        );
    }

    #[test]
    fn clearing_unresponsive_starts_a_fresh_window_next_time() {
        let mut r = ReconnectState::new();
        let t0 = Instant::now();
        r.classify_unresponsive(t0);
        r.clear_unresponsive();
        // A LATER unresponsive spell, well past the first window's own
        // deadline, must not inherit the old clock.
        let t1 = t0 + HEALTH_WINDOW + Duration::from_secs(1);
        assert_eq!(r.classify_unresponsive(t1), ReconnectDecision::Retry);
    }
}
