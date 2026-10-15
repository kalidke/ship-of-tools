//! Presence and the read mark: `fe.presence` throttling and the dwell that clears a row's blue after a person chose the view.

use super::*;

/// A person switched the view to a workspace and has not switched away:
/// when `at` arrives with the same view still up, the frontend sends
/// `workspace.activate { read: true }` once (ADR 0044 "Viewing clears
/// blue" — the 10 s dwell, owner decision 2026-09-08). Any other switch
/// drops the mark, so a blow-through while cycling never counts as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct ReadMark {
    pub(in crate::ui) host: HostKey,
    pub(in crate::ui) workspace_id: Option<String>,
    pub(in crate::ui) at: std::time::Instant,
}

/// How long a person must stay on a row before it counts as read.
pub(in crate::ui) const READ_DWELL: std::time::Duration = std::time::Duration::from_secs(10);

/// Minimum gap between `fe.presence` sends while input keeps coming (design point A): how stale the daemon's "used
/// most recently" may be when a person moves between windows (SHOW-ROUTE). Well under its five-minute window.
const PRESENCE_THROTTLE: std::time::Duration = std::time::Duration::from_secs(5);

/// How close two presses must be for the second to report a person: one stray press reports nothing (SHOW-ROUTE).
const PRESENCE_PAIR: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// This window's last real press and last `fe.presence` send: `None` until the first of each.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) struct PresenceClock {
    last_press: Option<std::time::Instant>,
    last_sent: Option<std::time::Instant>,
}

/// Whether input at `now` sends `fe.presence`, updating `clock`: a press (not a release or a key repeat) within
/// `PRESENCE_PAIR` of the press before it, at most once per `PRESENCE_THROTTLE`.
fn presence_due(clock: &mut PresenceClock, press: bool, now: std::time::Instant) -> bool {
    if !press {
        return false;
    }
    let paired = clock.last_press.is_some_and(|at| now.saturating_duration_since(at) <= PRESENCE_PAIR);
    clock.last_press = Some(now);
    let due = paired && clock.last_sent.is_none_or(|at| now.saturating_duration_since(at) >= PRESENCE_THROTTLE);
    if due {
        clock.last_sent = Some(now);
    }
    due
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadMarkAction {
    /// No mark, or not due yet: nothing to do.
    Keep,
    /// The view moved on before the dwell elapsed: drop the mark, send nothing.
    Cancel,
    /// Due, and the same view is still up: send the read flag and drop the mark.
    Fire,
}

fn read_mark_decision(
    mark: Option<&ReadMark>,
    active_host: &str,
    active_workspace_id: Option<&str>,
    now: std::time::Instant,
) -> ReadMarkAction {
    let Some(m) = mark else {
        return ReadMarkAction::Keep;
    };
    if m.host != active_host || m.workspace_id.as_deref() != active_workspace_id {
        return ReadMarkAction::Cancel;
    }
    if now < m.at {
        return ReadMarkAction::Keep;
    }
    ReadMarkAction::Fire
}

impl State {
    /// Send `fe.presence` if this input is due (`presence_due`; design point A). Called ONLY from `window_event`'s real
    /// `KeyboardInput`/`MouseInput` arms, never from a command file, `--capture-cycle` or any simulated path: every op
    /// the daemon once stamped from had an automated producer. A harness run (`--ephemeral`/`--capture`) is excluded.
    pub(in crate::ui) fn report_presence(&mut self, press: bool) {
        if self.ephemeral || !presence_due(&mut self.presence, press, std::time::Instant::now()) {
            return;
        }
        // EVERY connected host, not just `active_host` (2026-09-08 review correction): each daemon's agents route
        // commands through it, so each must know when a person is at this window. One clock covers the fan-out.
        for (host, _) in &self.conns {
            if let Err(e) = self.send_to(host, OutgoingReq::FePresence) {
                tracing::warn!(error = %e, %host, "drop fe.presence — channel closed");
            }
        }
    }

    /// Runs every redraw (the idle clock wakes once a second, so a due mark
    /// fires within a second of its deadline with no timer of its own):
    /// send the read flag for a row the user has stayed on for
    /// `READ_DWELL`, or drop a mark whose view has moved on.
    pub(in crate::ui) fn fire_due_read_mark(&mut self) {
        match read_mark_decision(
            self.read_mark.as_ref(),
            &self.active_host,
            self.active_workspace_id.as_deref(),
            std::time::Instant::now(),
        ) {
            ReadMarkAction::Keep => {}
            ReadMarkAction::Cancel => self.read_mark = None,
            ReadMarkAction::Fire => {
                let Some(m) = self.read_mark.take() else { return };
                tracing::info!(host = %m.host, ws = ?m.workspace_id, "read mark: dwell elapsed — clearing blue");
                let _ = self.send_to(
                    &m.host,
                    crate::net::transport::OutgoingReq::WorkspaceActivate {
                        workspace_id: m.workspace_id,
                        read: true,
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod read_mark_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn mark(at: Instant) -> ReadMark {
        ReadMark { host: "h".into(), workspace_id: Some("ws".into()), at }
    }

    #[test]
    fn no_mark_is_keep() {
        assert_eq!(read_mark_decision(None, "h", Some("ws"), Instant::now()), ReadMarkAction::Keep);
    }

    #[test]
    fn not_due_yet_is_keep_due_is_fire() {
        let t0 = Instant::now();
        let m = mark(t0 + READ_DWELL);
        assert_eq!(read_mark_decision(Some(&m), "h", Some("ws"), t0 + Duration::from_secs(3)), ReadMarkAction::Keep);
        assert_eq!(read_mark_decision(Some(&m), "h", Some("ws"), t0 + READ_DWELL), ReadMarkAction::Fire);
    }

    #[test]
    fn a_different_view_cancels_even_when_due() {
        let t0 = Instant::now();
        let m = mark(t0);
        assert_eq!(read_mark_decision(Some(&m), "h", Some("other"), t0 + Duration::from_secs(1)), ReadMarkAction::Cancel);
        assert_eq!(read_mark_decision(Some(&m), "elsewhere", Some("ws"), t0 + Duration::from_secs(1)), ReadMarkAction::Cancel);
        assert_eq!(read_mark_decision(Some(&m), "h", None, t0 + Duration::from_secs(1)), ReadMarkAction::Cancel);
    }
}

#[cfg(test)]
mod presence_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_lone_press_or_a_release_reports_nothing_a_second_press_reports_a_person() {
        let t0 = Instant::now();
        let at = |secs| t0 + Duration::from_secs(secs);
        let mut clock = PresenceClock::default();
        assert!(!presence_due(&mut clock, false, at(0)), "a release is not a press");
        assert!(!presence_due(&mut clock, true, at(1)), "one stray press reports nothing");
        assert!(!presence_due(&mut clock, false, at(2)), "nor does its release");
        assert!(presence_due(&mut clock, true, at(3)), "a second press within five minutes reports a person");
        assert!(!presence_due(&mut clock, true, at(4)), "throttled");
        assert!(presence_due(&mut clock, true, at(8)), "past the throttle");
        assert!(!presence_due(&mut clock, true, at(8 + 301)), "after five idle minutes a press is lone again");
        assert!(presence_due(&mut clock, true, at(8 + 302)), "and the next one reports");
    }
}
