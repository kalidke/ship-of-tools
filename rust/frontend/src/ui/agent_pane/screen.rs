//! Which screen the agent pane paints: the held, client and blank choice, the reason overlay and the discard notice.

/// DELETIONS (Codex review, lane B5 discharge): `WorkspaceRuntime` and
/// both `workspace_runtime` caches (`State`'s own and its staging
/// `FreshWorkspaceCaches` twin) are gone — a write-only cache with no
/// reader (ADR 0042 shrink round rule A retired its one reader,
/// `try_attach_capsule_pane`, and ADR 0045 decision 1's `pty.open` +
/// `PtyAttachDirect` path never consulted it either) named no invariant
/// worth a field. `WsAutostart`/`workspace_autostart` (and the
/// `session_name_key` helper that built its key) went the same way
/// post-notmux: the old FE autostart-on-attach launch it fed
/// (`pending_autostart` → `advance_autostart_scan` →
/// `autostart_claude_in_pane`) is retired in favor of the BE tmux
/// start-command wrapper (`attach_session_to_bl`'s own doc), which left
/// it write-only too.

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
    // payload) is tested where it lives — `transport.rs`'s own test
    // module (`is_attach_direct_declines_every_other_response_shape`,
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
