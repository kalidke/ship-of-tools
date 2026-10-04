# rust/frontend/src/ui/agent_pane: the agent pane (fe-ui)

The agent pane shows one row's capsule screen through a client dialed to the endpoint the control connection already
resolved. The code also calls it the "BL pane" and the "session pane". Part of the window (fe-ui); charter:
rust/frontend/src/ui/CLAUDE.md. Record: ADR 0042 and 0045.

## Files
- `mod.rs`: declares the three files and re-exports their items to `ui`.
- `screen.rs`: which screen the pane paints (`PaneFeed`, `HeldPaneScreen`, `PaneScreen`, `pane_screen_choice`), the
  reason overlay and the discard notice.
- `attach.rs`: the attach client (`PaneAttachClient`), `State::attach_session_to_bl`, its event pump, and the warm pool
  of parked clients (`WarmAttachPool`).
- `input.rs`: `State::send_pane_input` and the pane's mouse selection (`llm_cell_at_px`, `copy_llm_selection`).

## Start here
attach.rs `attach_session_to_bl`, for how a selected row becomes a pane client; screen.rs `pane_screen_choice`, for what
the pane paints while that happens.

## Rules
- Input reaches only the selected row's client or is counted as discarded (`send_pane_input`).
- Never two pane clients alive: the old one is shut down off the UI thread before the new attach
  (`spawn_pane_attach_term`).
- The pane never paints a new client's empty screen before its checkpoint (`pane_screen_choice`).
- The warm pool keeps at most the host's row count, capped at `WARM_ATTACH_CAP`, per host (`WarmAttachPool::park`).
- The reason overlay never reads the shared status line (`pane_terminal_reason_text`).
