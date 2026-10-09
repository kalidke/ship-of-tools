# rust/frontend/src/ui/agent_pane: the agent pane (fe-ui)

The agent pane shows one row's capsule screen through a client dialed to the endpoint the control connection already
resolved. The code also calls it the "BL pane" and the "session pane". Part of the window (fe-ui); charter:
rust/frontend/src/ui/CLAUDE.md. Record: ADR 0042 and 0045.

## Files
- `mod.rs`: declares the agent-pane modules and re-exports their items to `ui`.
- `presentation.rs`: the private request-owned candidate and one-shot receipt for a checkpointed current pane after its frame is submitted and presented.
- `presentation_tests.rs`: checkpoint, visible-area, origin and one-shot presentation behavior.
- `screen.rs`: which screen the pane paints (`PaneFeed`, `HeldPaneScreen`, `PaneScreen`, `pane_screen_choice`), the
  reason overlay and the discard notice, and the frame's work for it: `State::session_pane_view` and `State::sync_pane_pty_size`.
- `attach.rs`: the attach client (`PaneAttachClient`), `State::attach_session_to_bl`, its event pump, and the warm pool
  of parked clients (`WarmAttachPool`).
- `input.rs`: `State::send_pane_input` and the pane's mouse selection (`llm_cell_at_px`, `copy_llm_selection`).
- `replies.rs`: pty.open replies (attach direct, failure)
- `keys.rs`: What a key does in the agent pane: copy, paste and paging, else bytes to its session.

## Start here
attach.rs `attach_session_to_bl`, for how a selected row becomes a pane client; screen.rs `pane_screen_choice`, for what
the pane paints while that happens.

## Rules
- Input reaches only the selected row's client or is counted as discarded (`send_pane_input`).
- The active pane slot holds at most one client. Departing live checkpointed clients may remain alive in the warm pool with viewed=false; clients selected for retirement are shut down off the UI thread, and replacement attachment does not wait for shutdown or worker exit (`spawn_pane_attach_term`, `park_warm_attach`, `shutdown_detached`).
- While a new client is awaiting its checkpoint, a held departing screen is retained when available; without a hold the pane may paint the client's initially empty screen, which does not count as attach completion.
- The warm pool keeps at most the host's row count, capped at `WARM_ATTACH_CAP`, per host (`WarmAttachPool::park`).
- The reason overlay never reads the shared status line (`pane_terminal_reason_text`).
- A presentation receipt requires the current live attached client's checkpoint, a nonempty painted pane and a known request origin, and is emitted once only after frame presentation.
- Only a live client is resized with the pane, and a resize snaps its scrollback to live (`sync_pane_pty_size`).
- A cold pane constructs its one `DaemonLaneEndpoint` with `new`; a warm hit reuses the existing client (`spawn_pane_attach_term`).
