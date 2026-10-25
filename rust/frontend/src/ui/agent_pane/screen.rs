//! Which screen the agent pane paints: the held, client and blank choice, the reason overlay and the discard notice.

use super::*;
use std::time::Instant;

/// ADR 0042 slice L1b fix 2, narrowed post-notmux: which state the
/// session pane's input routes to RIGHT NOW — tracked independently of
/// `Option<FeAttachClient>` because "no client yet" is genuinely
/// ambiguous: `Pending` (attach unresolved) for a freshly-selected row
/// whose `pty.open` is in flight and hasn't yet come back as an
/// `attach_direct` refusal (confirms capsule — the only kind of row this
/// build has). Treating "no client" as "already attached" during that
/// window sent live keystrokes to whatever the pane was PREVIOUSLY
/// showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum PaneFeed {
    // ADR 0045 decision 1: assigned by the (now unconditional)
    // `PtyAttachDirect` handler on every platform — a capsule row
    // attaches through its own daemon's lane bridge wherever the daemon
    // runs, not only from a Windows frontend.
    Capsule,
    Pending,
}

/// LU6a: the pane's last-known screen content, captured the instant a
/// departing capsule client is dropped for an incoming switch — held
/// until the new attach client's checkpoint lands, so the pane never
/// repaints an empty parser mid-switch (see `attach_session_to_bl`'s
/// capture sites and `pane_screen_choice`'s own doc). An owned clone is
/// the smallest shape that works: `pane_attach_term`'s `.screen()`
/// already returns `&vt100::Screen`, so holding one is just keeping a
/// copy of whichever was live a moment ago — no timers, no diffing.
pub(in crate::ui) struct HeldPaneScreen(pub(in crate::ui) vt100::Screen);

impl HeldPaneScreen {
    pub(in crate::ui) fn screen(&self) -> &vt100::Screen {
        &self.0
    }
}

/// LU6a: which source paints the session pane — pulled out of the draw
/// site (`pty_screen`, just above the checkpoint-restore doc) as a pure
/// function so the branches are unit-tested without a live `State`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum PaneScreen {
    /// The capsule attach client's own parser screen.
    Client,
    /// `pane_hold`'s captured screen — the pane's last content, held
    /// until the new attach client's checkpoint lands.
    Hold,
    /// Nothing worth painting yet (no client has ever attached, or the
    /// last one died before ever checkpointing) — the pane draws blank
    /// at its current size.
    Empty,
}


/// A blank screen at the given size — what the session pane shows when
/// `PaneScreen::Empty` resolves (no client has ever attached, or the
/// last one died before checkpointing). Every row is a capsule on this
/// build, so there is no tmux emulator to fall back to; a freshly
/// constructed, never-fed `vt100::Parser` is blank by construction.
pub(in crate::ui) fn blank_pane_screen(cols: u16, rows: u16) -> vt100::Screen {
    vt100::Parser::new(rows, cols, 0).screen().clone()
}

/// LU6a: a capsule switch must not paint the new client's freshly
/// constructed, still-empty parser before its checkpoint lands (the
/// visible clear this lane fixes) — so a live, not-yet-checkpointed
/// client, or a `Pending` feed (it isn't even known yet whether a client
/// is coming), defers to whatever the pane held from before. Falls back
/// to blank only once there is nothing held (a first-ever attach, or a
/// hold already cleared): the client's own screen while one exists,
/// blank while none does — exactly what the old unconditional
/// `pane_attach_term.map(...).unwrap_or_else(...)` drew.
///
/// A client that reaches a TERMINAL failure
/// (`is_dead`) before it EVER checkpointed is a dead end, not a stall —
/// neither a hold (the departed row's screen) nor the dead client's own
/// (blank) screen is right to keep showing, so this falls all the way
/// through to blank instead, same as no client existing at all. A
/// client that checkpointed and only later died keeps painting its own
/// (now-frozen, but real) last content — the `checkpointed` branch above
/// still wins regardless of `is_dead`.
///
/// `has_client`/`checkpointed`/`is_dead`/`has_hold` mirror
/// `pane_attach_term.is_some()`/`FeAttachClient::is_checkpointed`/
/// `FeAttachClient::is_dead`/`pane_hold.is_some()`.
pub(in crate::ui) fn pane_screen_choice(
    has_client: bool,
    checkpointed: bool,
    is_dead: bool,
    feed: PaneFeed,
    has_hold: bool,
) -> PaneScreen {
    if has_client && checkpointed {
        PaneScreen::Client
    } else if has_client && is_dead {
        PaneScreen::Empty
    } else if has_client || feed == PaneFeed::Pending {
        if has_hold {
            PaneScreen::Hold
        } else if has_client {
            PaneScreen::Client
        } else {
            PaneScreen::Empty
        }
    } else {
        PaneScreen::Empty
    }
}

/// ADR 0030 §8 "Where it is shown", widened by ADR 0045 decision 1
/// (Codex review, lane B5 discharge): originally only the dead-
/// uncheckpointed case (`pane_screen_choice` resolving to `PaneScreen::
/// Empty` with no content of its own worth painting) showed a reason —
/// EXCLUDING a client that is still alive but CURRENTLY failing or
/// retrying (typed `Unreachable` mid-outage, a refusal, a failure after
/// checkpointing), which left the pane's only signal a shared, easily-
/// clobbered `self.status` write, subordinate to a stale retained
/// "attached to leg…" notice (see `pump_pane_attach_term`'s own fix).
/// Now: any moment the client is not honestly `is_attached` gets the
/// persistent overlay, checkpointed or not — the frozen screen
/// underneath (when one exists) is real, but stale, and the user must
/// see that a LIVE problem exists, not just its last-known-good moment.
pub(in crate::ui) fn pane_shows_terminal_reason(has_client: bool, is_attached: bool) -> bool {
    has_client && !is_attached
}

/// The text `pane_shows_terminal_reason`'s own overlay paints, given
/// `client_status` — the RETAINED client's own `status_line()`, read
/// directly at the call site. Deliberately takes no `self.status`
/// parameter at all (Codex review): that field is shared with every
/// other status-bar message in the event loop, so a function that could
/// read it would be a function a LATER, unrelated write could silently
/// retitle this pane's own explanation through — this signature makes
/// that impossible by construction, not merely untested.
pub(in crate::ui) fn pane_terminal_reason_text(shows_reason: bool, client_status: Option<&str>) -> Option<String> {
    shows_reason.then(|| client_status.unwrap_or_default().to_string())
}

/// The agent pane's reason line. A daemon's protocol refusal leads: it is
/// the root cause of whatever the retained client or the dial reports.
/// Then the client's own reason, then the row's dial error.
pub(in crate::ui) fn pane_reason_line(
    mismatch: Option<&str>,
    client: Option<String>,
    dial_error: Option<&str>,
) -> Option<String> {
    mismatch
        .map(str::to_owned)
        .or(client)
        .or_else(|| dial_error.map(str::to_owned))
}

/// The agent pane's overlay, one row each, top down: why it is not live,
/// then the discarded-input count. Separate rows, so a long reason never
/// truncates the count off the pane and the count never hides the reason.
pub(in crate::ui) fn pane_overlay_lines(reason: Option<String>, notice: Option<String>) -> Vec<String> {
    reason.into_iter().chain(notice).collect()
}

/// The agent pane's discarded-input notice: the frontend's own count plus
/// the attach client's. Singular at one; `None` at none.
pub(in crate::ui) fn pane_discard_notice(pane: usize, client: usize) -> Option<String> {
    match pane + client {
        0 => None,
        1 => Some("1 keystroke discarded, not sent".to_string()),
        n => Some(format!("{n} keystrokes discarded, not sent")),
    }
}


/// A frame-local, opaque claim for the exact request and eligible client lent to the draw.
pub(in crate::ui) struct PaintedClient(PresentationCandidate);

impl<C: PaneClient> AgentPane<C> {
    pub(in crate::ui) fn view(&mut self, mismatch: Option<&str>) -> (PaneScreen, Vec<String>, Option<PaintedClient>) {
        // Pump precedes this operation. Drawing then borrows the chosen screen immutably,
        // releases that borrow, synchronizes geometry and offers this frame's opaque claim.
        let pane_attach_has_client = self.pane_attach_term.is_some();
        let pane_attach_checkpointed =
            self.pane_attach_term.as_ref().is_some_and(|t| t.is_checkpointed());
        let pane_attach_is_dead = self.pane_attach_term.as_mut().is_some_and(|t| t.is_dead());
        let pane_screen = pane_screen_choice(
            pane_attach_has_client,
            pane_attach_checkpointed,
            pane_attach_is_dead,
            self.pane_feed,
            self.pane_hold.is_some(),
        );
        // The retained client's reason is independent of State's shared chrome status.
        let pane_attach_status = self.pane_attach_term.as_ref().map(|t| t.status_line());
        let pane_attach_is_attached = pane_attach_status == Some("attached");
        // A daemon that refused this frontend's protocol is the root cause of
        // whatever the client or the dial reports, so its line leads.
        let pane_terminal_reason: Option<String> = pane_reason_line(
            mismatch,
            pane_terminal_reason_text(
                pane_shows_terminal_reason(pane_attach_has_client, pane_attach_is_attached),
                pane_attach_status,
            ),
            self.pane_dial_error.as_deref(),
        );
        let pane_overlay = pane_overlay_lines(
            pane_terminal_reason,
            pane_discard_notice(
                self.pane_inputs_discarded,
                self.pane_attach_term.as_ref().map_or(0, |t| t.inputs_discarded()),
            ),
        );
        // Choosing a screen does not complete presentation. The frame carries a qualified candidate to the presentation owner and completes it only after submit/present.
        let facts = (pane_screen == PaneScreen::Client).then(|| PaneFacts {
            checkpointed: pane_attach_checkpointed, attached: pane_attach_is_attached, live: !pane_attach_is_dead,
        });
        let facts = facts.and_then(|f| self.pane_presentation.candidate(f, (1, 1))).map(PaintedClient);
        (pane_screen, pane_overlay, facts)
    }

    pub(in crate::ui) fn painted_screen(&self, choice: PaneScreen) -> Option<&vt100::Screen> {
        match choice {
            PaneScreen::Client => self.pane_attach_term.as_ref().map(|c| c.screen()),
            PaneScreen::Hold => self.pane_hold.as_ref().map(|h| h.screen()),
            PaneScreen::Empty => None,
        }
    }

    pub(in crate::ui) fn painted_candidate(&self, claim: PaintedClient, area: (u16, u16)) -> Option<PresentationCandidate> {
        (area.0 > 0 && area.1 > 0).then_some(claim.0)
    }

    /// Called only after the frame has been submitted and presented.
    pub(in crate::ui) fn presented(&mut self, candidate: PresentationCandidate, now: Instant) -> Presentation {
        self.pane_presentation.complete(candidate, now)
    }

    pub(in crate::ui) fn sync_size(&mut self, pty_size_observed: (u16, u16)) {
        // Geometry alone never sends an open. Pending remembers the latest valid size;
        // Capsule resizes only an installed selected client, resetting scroll on a reshape.
        let (cols, rows) = pty_size_observed;
        if cols >= 2 && rows >= 2 {
            let need_open = self.pty_size.is_none();
            let need_resize = self
                .pty_size
                .map(|prev| prev != (cols, rows))
                .unwrap_or(false);
            match self.pane_feed {
                PaneFeed::Capsule => {
                    if need_open || need_resize {
                        if let Some(t) = self.pane_attach_term.as_mut() {
                            t.resize(cols, rows);
                            if need_resize {
                                // A resize reshapes the row map, so the old
                                // offset means nothing: snap to live.
                                t.reset_scroll();
                            }
                        }
                        self.pty_size = Some((cols, rows));
                    }
                }
                PaneFeed::Pending => {
                    if need_open || need_resize {
                        self.pty_size = Some((cols, rows));
                    }
                }
            }
        }
    }
}

impl State {
    pub(in crate::ui) fn session_pane_view(&mut self) -> (PaneScreen, Vec<String>, Option<PaintedClient>) {
        let target = self.agent_pane.target();
        let host = target.as_ref().map(|(h, _)| h).unwrap_or(&self.active_host);
        let mismatch = self.hello_refused.get(host).and_then(|m| m.lines().next());
        self.agent_pane.view(mismatch)
    }

    /// Offers a candidate to the owner and logs its outcome. A receipt also gives, each since the request, when its
    /// frame began, had prepared its text, got its drawable, finished encoding and was submitted (`stamps`, in order).
    pub(in crate::ui) fn log_presentation(&mut self, candidate: PresentationCandidate, stamps: [Instant; 5]) {
        let now = Instant::now();
        match self.agent_pane.presented(candidate, now) {
            Presentation::Receipt { elapsed, .. } => {
                let since = |at: Instant| elapsed.saturating_sub(now.saturating_duration_since(at)).as_millis();
                tracing::info!(
                    since_request_ms = elapsed.as_millis() as u64,
                    since_request_ns = elapsed.as_nanos() as u64,
                    redraw_ms = since(stamps[0]) as u64,
                    prepared_ms = since(stamps[1]) as u64,
                    drawable_ms = since(stamps[2]) as u64,
                    encoded_ms = since(stamps[3]) as u64,
                    submitted_ms = since(stamps[4]) as u64,
                    "session pane: capsule screen presented"
                )
            }
            Presentation::NoOrigin => tracing::warn!("session pane: capsule screen presented without a request origin"),
            Presentation::Stale => {}
        }
    }

}

#[cfg(test)]
#[path = "screen_pane_input_tests.rs"]
mod pane_input_tests;

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
