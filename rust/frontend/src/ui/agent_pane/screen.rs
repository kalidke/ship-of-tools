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


impl State {
    pub(in crate::ui) fn session_pane_view(&mut self) -> (PaneScreen, Vec<String>, Option<PaneFacts>) {
        // Borrow the LLM terminal screen for the duration of the draw.
        // `pane_attach_term`'s `vt100-ctt` `screen()` returns a `&Screen`
        // tied to the client; since `terminal.draw` borrows a different
        // field (self.terminal), Rust's split-borrow rules let us hold
        // both at once. The local terminal's screen is borrowed the same
        // way when the Terminal drawer is active. Unconditional on every
        // platform since ADR 0045 decision 1 (a capsule row attaches
        // through its own daemon everywhere).
        //
        // While awaiting a checkpoint, prefer the departing screen when a hold exists. With no hold a live client's initially empty parser can be selected; it cannot produce a presentation receipt. A client that becomes terminal before its first checkpoint falls through to blank.
        // Three separate `let`s (rather than inlining each as a call
        // argument) so the one `&mut` read (`is_dead`) never overlaps
        // the `&ref` reads around it.
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
        // ADR 0030 §8 "Where it is shown", widened by ADR 0045 decision 1
        // (Codex review): paints whenever the client is alive but not
        // honestly attached — dead-uncheckpointed (the original case),
        // a mid-outage `Unreachable` retry, a refusal, or a failure AFTER
        // checkpointing (the frozen screen underneath is real, but
        // stale). Codex review: reads the RETAINED client's own
        // `status_line()` directly, never `self.status` — that field is
        // shared with every other status-bar message in the whole event
        // loop and a later, unrelated write (autostart, a daemon
        // reconnect, a drawer switch) would silently retitle this pane's
        // own explanation to whatever last touched the status bar.
        let pane_attach_status = self.pane_attach_term.as_ref().map(|t| t.status_line());
        let pane_attach_is_attached = pane_attach_status == Some("attached");
        // A daemon that refused this frontend's protocol is the root cause of
        // whatever the client or the dial reports, so its line leads.
        let pane_host = self
            .bl_pane_target
            .as_ref()
            .map(|(h, _)| h)
            .unwrap_or(&self.active_host);
        let pane_terminal_reason: Option<String> = pane_reason_line(
            self.hello_refused
                .get(pane_host)
                .and_then(|m| m.lines().next()),
            pane_terminal_reason_text(
                pane_shows_terminal_reason(pane_attach_has_client, pane_attach_is_attached),
                pane_attach_status,
            ),
        // SHOULD-FIX (Codex review, lane B5 discharge): no live client at
        // all (a dial that never got to attach in the first place) still
        // needs a persistent, non-clobberable reason when this row's
        // host has a known-broken dial — same priority tier as a live
        // client's own failure.
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
        let facts = (pane_screen == PaneScreen::Client)
            .then_some(PaneFacts { checkpointed: pane_attach_checkpointed, attached: pane_attach_is_attached, live: !pane_attach_is_dead });
        (pane_screen, pane_overlay, facts)
    }

    /// Offers a candidate to the owner and logs its outcome. A receipt also gives, each since the request, when its
    /// frame began, had prepared its text, got its drawable, finished encoding and was submitted (`stamps`, in order).
    pub(in crate::ui) fn log_presentation(&mut self, candidate: PresentationCandidate, stamps: [Instant; 5]) {
        let now = Instant::now();
        match self.pane_presentation.complete(candidate, now) {
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

    pub(in crate::ui) fn sync_pane_pty_size(&mut self, pty_size_observed: (u16, u16)) {
        // Track the LLM-pane's size once the first redraw has a real BL
        // content rect, and keep an already-live capsule client's
        // viewport in sync when the rect grows/shrinks — the actual
        // attach/open wire request is `attach_session_to_bl`'s, not
        // this redraw's.
        let (cols, rows) = pty_size_observed;
        if cols >= 2 && rows >= 2 {
            let need_open = self.pty_size.is_none();
            let need_resize = self
                .pty_size
                .map(|prev| prev != (cols, rows))
                .unwrap_or(false);
            // ADR 0042 slice L1b fix 2: `pane_feed`, not the 2-way
            // `pane_is_capsule` this used to branch on — `Pending` must
            // send NOTHING to either backend (a resize routed by
            // guesswork would be indistinguishable from the exact bug
            // finding 2 fixed for input) while still tracking the
            // latest size locally, since whichever backend eventually
            // resolves reads its start/resize from `self.pty_size`.
            match self.pane_feed {
                PaneFeed::Capsule => {
                    // The capsule client's connection is owned entirely
                    // by the `PtyAttachDirect` handler's own
                    // `spawn_pane_attach_term` call (the daemon's own
                    // reply to the `pty.open` `attach_session_to_bl`
                    // always sends) — there is no `pty.open`/`pty.resize` wire
                    // request for a capsule row (the daemon refuses both
                    // with `attach_direct`). This arm only keeps an
                    // ALREADY-LIVE client's viewport in sync with the pane
                    // rect, mirroring the drawer's own attach-client
                    // resize (`term_size_observed`).
                    if need_open || need_resize {
                        if let Some(t) = self.pane_attach_term.as_mut() {
                            t.resize(cols, rows);
                            if need_resize {
                                // A resize reshapes the row map, so the old
                                // offset means nothing: snap to live.
                                t.screen_mut().set_scrollback(0);
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

#[cfg(test)]
#[path = "screen_pane_input_tests.rs"]
mod pane_input_tests;

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
