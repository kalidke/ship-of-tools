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

/// Minimum gap between `fe.presence` sends while real input keeps coming
/// (2026-09-08 review rework, design point A) — matches the daemon's own
/// `ACTIVE_WINDOW_SECS` order of magnitude without needing to agree on the
/// exact number: any throttle well under the daemon's activity window keeps
/// a person who is genuinely still typing/clicking from ever expiring out.
const PRESENCE_THROTTLE: std::time::Duration = std::time::Duration::from_secs(30);

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
    /// Send `fe.presence` if this is real input and the last send is stale
    /// by more than `PRESENCE_THROTTLE` (2026-09-08 review rework, design
    /// point A). Call ONLY from `window_event`'s real `KeyboardInput`/
    /// `MouseInput` arms — never from command-file dispatch,
    /// `--capture-cycle`, or any other simulated path, which is exactly
    /// what makes this signal trustworthy where the daemon-side inference
    /// it replaces wasn't (every op the daemon used to stamp from turned
    /// out to have an automated producer too). A harness run
    /// (`--ephemeral`/`--capture`) has no person at the keyboard even when
    /// it synthesizes input, so it's excluded outright. No timer, no
    /// heartbeat: idle input sends nothing at all.
    pub(in crate::ui) fn report_presence(&mut self) {
        if self.ephemeral {
            return;
        }
        let now = std::time::Instant::now();
        if self
            .presence_last_sent
            .is_some_and(|last| now.duration_since(last) < PRESENCE_THROTTLE)
        {
            return;
        }
        self.presence_last_sent = Some(now);
        // EVERY connected host, not just `active_host` (2026-09-08 review
        // correction) — a person is present for every daemon THIS frontend
        // is attached to, backend included: the backend's own agents route
        // commands through its daemon, so if the person spends an hour on
        // a local row, the backend's stamp for this frontend would go
        // stale and its commands would broadcast or fail to reach here.
        // One throttle covers the whole fan-out (`presence_last_sent` is
        // per-frontend, not per-host) — same pattern as Sessions mode's
        // `workspace.list` re-announce just above. Invariant: every daemon
        // this frontend is attached to knows when a person is at it.
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
                    crate::transport::OutgoingReq::WorkspaceActivate {
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
