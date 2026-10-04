# rust/frontend/src/ui/control: agent control of the window (fe-ui)

The ways an agent or a script drives the window: the daemon's `fe.command` evt, a `sot_ui` envelope in agent text, and
command files dropped in the state directory (ADR 0019), with fe-state.json written back for them to read. Every route
ends in `State::dispatch_fe_command`. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the folder and re-exports what the rest of ui names.
- `command.rs`: `route_fe_command` (an fe.command evt to an `FeCommand`) and the `FeCommand` enum, with their tests.
- `dispatch.rs`: `dispatch_fe_command` and `drain_fe_commands`, plus `preview_targets_active_ws` and `badge_host_key`.
- `envelope.rs`: `parse_nav_envelope`, `NavEnvelope` and `handle_nav_envelope` (ADR 0025's same-workspace open).
- `file_channel.rs`: `fe_commands_dir`, `fe_state_path` and `maybe_write_fe_state`.

## Start here
`route_fe_command` for what the wire accepts; `dispatch_fe_command` for what a command does to the window.

## Rules
- Agents act through the same methods the keys call, so commands inherit their routing (`dispatch_fe_command`).
- Only a command addressed to this window may force-show, and a broadcast relaunch is refused (`route_fe_command`).
- `open_url` is http(s) only and a preview caption is sanitized and capped at `CAPTION_MAX_CHARS` on the wire route
  (`route_fe_command`).
- Showing a result never steals the user's view: a preview for another workspace badges that workspace's row, and only an
  urgent one switches to it (`dispatch_fe_command`).
- Present limit: the file channel deserializes `FeCommand` directly, so `route_fe_command`'s checks do not apply to it.
- Present limit: a `Preview` command compares the workspace slug without its host (`preview_targets_active_ws`).
