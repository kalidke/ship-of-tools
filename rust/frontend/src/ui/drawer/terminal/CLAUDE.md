# rust/frontend/src/ui/drawer/terminal: the Terminal drawer and the vt100 helpers (fe-ui)

The Terminal tenant of the drawer: a shell in a local pty, hosted by `LocalTerminal`, whose output a vt100 emulator
turns into a screen the window paints. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the files below.
- `pty.rs`: `LocalTerminal`, which runs a shell in a pty and feeds a vt100 parser, and `resolve_shell`, which picks the shell.

## Start here
`LocalTerminal::spawn` in pty.rs for how a shell starts and its output is read.

## Rules
- The shell is chosen by `resolve_shell`: a settings override first, then the platform's best available shell.
