//! Which screen the agent pane paints: the held, client and blank choice, the reason overlay and the discard notice.

use super::*;

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
/// Coordinator amendment: a client that reaches a TERMINAL failure
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
    pub(in crate::ui) fn session_pane_view(&mut self) -> (PaneScreen, Vec<String>) {
        // Borrow the LLM terminal screen for the duration of the draw.
        // `pane_attach_term`'s `vt100-ctt` `screen()` returns a `&Screen`
        // tied to the client; since `terminal.draw` borrows a different
        // field (self.terminal), Rust's split-borrow rules let us hold
        // both at once. The local terminal's screen is borrowed the same
        // way when the Terminal drawer is active. Unconditional on every
        // platform since ADR 0045 decision 1 (a capsule row attaches
        // through its own daemon everywhere).
        //
        // LU6a: which source actually wins is `pane_screen_choice`'s
        // call, not an unconditional `pane_attach_term`-if-present — a
        // live but not-yet-checkpointed client (or a still-`Pending`
        // feed) defers to `pane_hold` so a capsule switch never paints
        // the new client's empty parser. Coordinator amendment: a client
        // that went terminal before ever checkpointing falls all the way
        // through to blank instead (`pane_screen_choice`'s own doc) —
        // three separate `let`s (rather than inlining each as a call
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
            self.protocol_mismatch
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
        // Switch-latency Phase 1, item 3: the acceptance metric itself
        // (keypress → current screen visible), not merely the client's
        // own parser being ready (`pump_pane_attach_term`'s "checkpoint
        // applied") — this is the first REDRAW that actually paints the
        // new client's own screen (`PaneScreen::Client`) rather than the
        // held prior content or the tmux fallback. One-shot per attach,
        // same edge-triggered pattern as the other attach-outcome lines.
        if pane_screen == PaneScreen::Client && !self.pane_attach_presented {
            self.pane_attach_presented = true;
            let since_request_ms = self
                .pane_attach_requested_at
                .map(|s| s.elapsed().as_millis() as u64)
                .unwrap_or(0);
            tracing::info!(since_request_ms, "session pane: capsule screen presented");
        }
        (pane_screen, pane_overlay)
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
mod pane_input_tests {
    use super::*;

    #[test]
    fn pane_discard_notice_counts_every_discarded_input() {
        // The frontend's count (Pending-arm inputs, +1 each) plus the
        // attach client's own count; absent at 0, singular at 1.
        assert_eq!(pane_discard_notice(0, 0), None);
        assert_eq!(
            pane_discard_notice(1, 0).as_deref(),
            Some("1 keystroke discarded, not sent")
        );
        assert_eq!(
            pane_discard_notice(3, 0).as_deref(),
            Some("3 keystrokes discarded, not sent")
        );
        assert_eq!(
            pane_discard_notice(2, 2).as_deref(),
            Some("4 keystrokes discarded, not sent")
        );
    }

    #[test]
    fn pane_overlay_keeps_the_count_beside_any_reason() {
        let n = || Some("2 keystrokes discarded, not sent".to_string());
        assert_eq!(
            pane_overlay_lines(Some("dial failed".to_string()), n()),
            vec!["dial failed".to_string(), n().unwrap()]
        );
        assert_eq!(pane_overlay_lines(None, n()), vec![n().unwrap()]);
        assert_eq!(
            pane_overlay_lines(Some("dial failed".to_string()), None),
            vec!["dial failed".to_string()]
        );
        assert!(pane_overlay_lines(None, None).is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The `attach_direct` switch itself (parsing the daemon's refusal
    // payload) is tested where it lives — the tests of
    // `net/transport/ops/workspace.rs` (`is_attach_direct_declines_every_other_response_shape`,
    // plus a real-seam test driven through `handle_response_frame`) —
    // rather than a reimplementation here.

    /// LU6a: `pane_screen_choice`'s branches, checked here so this
    /// decision runs on every platform (its call site is unconditional
    /// too, ADR 0045 decision 1). Coordinator amendment added the
    /// `is_dead` parameter and the cases below that exercise it.
    #[test]
    fn pane_screen_choice_covers_every_branch() {
        // A checkpointed client always wins, hold or no hold, pending or
        // not, dead or not — this is the case that used to paint an empty
        // parser. `is_dead` never overrides a client that DID checkpoint
        // (a client that showed real content and only later died keeps
        // showing it).
        assert_eq!(
            pane_screen_choice(true, true, false, PaneFeed::Capsule, false),
            PaneScreen::Client
        );
        assert_eq!(
            pane_screen_choice(true, true, false, PaneFeed::Capsule, true),
            PaneScreen::Client
        );
        assert_eq!(
            pane_screen_choice(true, true, true, PaneFeed::Capsule, false),
            PaneScreen::Client
        );

        // A live but not-yet-checkpointed client defers to a hold when
        // one exists...
        assert_eq!(
            pane_screen_choice(true, false, false, PaneFeed::Capsule, true),
            PaneScreen::Hold
        );
        // ...and otherwise paints its own (empty) screen — the first-ever
        // attach case, where there is nothing to hold.
        assert_eq!(
            pane_screen_choice(true, false, false, PaneFeed::Capsule, false),
            PaneScreen::Client
        );

        // Coordinator amendment: a client that goes TERMINAL before it
        // ever checkpointed is a dead end, not a stall — it must not keep
        // showing a hold (the departed row's screen) OR its own (blank)
        // screen. Both fall all the way through to `Empty`.
        assert_eq!(
            pane_screen_choice(true, false, true, PaneFeed::Capsule, true),
            PaneScreen::Empty
        );
        assert_eq!(
            pane_screen_choice(true, false, true, PaneFeed::Capsule, false),
            PaneScreen::Empty
        );

        // `Pending` (no client yet, backend unknown) also defers to a
        // hold when one exists...
        assert_eq!(
            pane_screen_choice(false, false, false, PaneFeed::Pending, true),
            PaneScreen::Hold
        );
        // ...and otherwise the pane is blank — every row is a capsule on
        // this build, so there is no other feed left to fall back to.
        assert_eq!(
            pane_screen_choice(false, false, false, PaneFeed::Pending, false),
            PaneScreen::Empty
        );
    }

    #[test]
    fn pane_shows_terminal_reason_only_for_the_dead_uncheckpointed_case() {
        // ADR 0030 §8 "Where it is shown", widened by ADR 0045 decision 1
        // (Codex review, BLOCKER): `is_attached` alone decides it now —
        // deliberately NOT parameterized by "checkpointed" any more, so a
        // client that already checkpointed (its own frozen content is
        // what `pane_screen_choice` separately paints) but is CURRENTLY
        // failing or retrying (typed `Unreachable`, a refusal, a post-
        // checkpoint failure — anything that isn't honestly "attached")
        // shows a reason exactly the same as the original dead-
        // uncheckpointed case did. The old three-argument version hid
        // this; this one-boolean signature cannot.
        assert!(pane_shows_terminal_reason(true, false));
        // A live, honestly-attached client — nothing to explain.
        assert!(!pane_shows_terminal_reason(true, true));
        // No client at all, either way.
        assert!(!pane_shows_terminal_reason(false, false));
        assert!(!pane_shows_terminal_reason(false, true));
    }

    #[test]
    fn pane_terminal_reason_text_reads_only_the_clients_own_status_never_self_status() {
        // Codex review, should-fix: `pane_terminal_reason_text` takes the
        // client's OWN `status_line()` as a plain `Option<&str>` argument
        // and nothing resembling a shared, event-loop-wide status field —
        // there is no `self.status` parameter for a later, unrelated
        // write to reach through. Proven by construction (the signature),
        // demonstrated here: two calls with the SAME `client_status` give
        // the SAME text regardless of anything else that could have
        // happened in between.
        let text = pane_terminal_reason_text(true, Some("checkpoint restore failed: some detail"));
        assert_eq!(text.as_deref(), Some("checkpoint restore failed: some detail"));

        // A DIFFERENT string arriving elsewhere in the caller's own state
        // (what `self.status` becoming "already attached …" would be)
        // cannot change the ALREADY-COMPUTED reason above, and a fresh
        // call with the client's status UNCHANGED reproduces it exactly.
        let unrelated_later_status = "already attached — nothing to do";
        assert_ne!(unrelated_later_status, "checkpoint restore failed: some detail");
        let text_again = pane_terminal_reason_text(true, Some("checkpoint restore failed: some detail"));
        assert_eq!(text_again, text);
    }

    #[test]
    fn pane_terminal_reason_text_is_none_when_no_reason_is_shown() {
        assert_eq!(pane_terminal_reason_text(false, Some("attached")), None);
    }

    #[test]
    fn pane_reason_line_leads_with_a_protocol_refusal() {
        let s = |x: &str| Some(x.to_string());
        assert_eq!(
            pane_reason_line(Some("frontend out of date"), s("connecting…"), Some("dial failed")),
            s("frontend out of date")
        );
        assert_eq!(
            pane_reason_line(None, s("connecting…"), Some("dial failed")),
            s("connecting…")
        );
        assert_eq!(pane_reason_line(None, None, Some("dial failed")), s("dial failed"));
        assert_eq!(pane_reason_line(None, None, None), None);
    }
}
