# rust/frontend/src/ui/drawer/terminal: the Terminal drawer and the vt100 helpers (fe-ui)

The Terminal tenant of the drawer: a shell in a local pty, hosted by `LocalTerminal`, whose output a vt100 emulator
turns into a screen the window paints. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the files below.
- `backend.rs`: the backend choice (`drawer_uses_attach`), the drawer's scroll (`scroll_drawer_ring`) and the attach client's start and pump (`State::spawn_attach_term`, `State::pump_attach_term`).
- `pty.rs`: `LocalTerminal`, which runs a shell in a pty and feeds a vt100 parser, and `resolve_shell`, which picks the shell.
- `vt.rs`: vt100 helpers: `scroll_ring`, `key_to_pty_bytes`, `paint_terminal` and `vt100_color_to_ratatui`; the agent pane uses them too.
- `pump.rs`: `State::pump_drawer_terminals` (spawn and pump before the draw) and `State::sync_terminal_drawer_size`
  (resize after it).

## Start here
`LocalTerminal::spawn` in pty.rs for how a shell starts and its output is read.

## Rules
- The shell is chosen by `resolve_shell`: a settings override first, then the platform's best available shell.
- The backend is chosen once per window and never swapped (`drawer_uses_attach`).
- The attach-only backend exists only on Windows (`State::spawn_attach_term`).
- The emulator owns the scrollback offset, written only on a user action (`scroll_ring`).
- A Command chord never reaches the pty as text (`key_to_pty_bytes`; macOS delivers Cmd+letter as a plain character).
- Off the Windows attach-only backend, the shell starts the first time the drawer shows the Terminal, and a failed
  spawn closes the drawer (`pump_drawer_terminals`).
