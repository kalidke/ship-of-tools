# agents/spawn: the CLIs that start, end, probe and bootstrap rows (agents)

A session drives its daemon over the same wire the frontend uses; these four scripts are the row-lifecycle half of that.
Each sources `comm-lib.sh` from its own folder, and the installer copies the folder flat into `~/.sot-comm/bin`, so
they run beside the library at its installed name. Part of agents; charter: agents/CLAUDE.md.

## Files
- `comm-spawn.sh`: `workspace.create` for a new row and its agent, a provisional registry row for its handle, an optional task brief typed to the new session.
- `comm-despawn.sh`: ends a row by handle, slug or id: resolves it first, destroys the workspace, then removes the registry row.
- `comm-probe.sh`: the acceptance matrix's responder rows (`up`, `down`, `serve`, `status`); touches only handles that begin `probe`.
- `comm-bootstrap.sh`: types one join-and-reply line into another row by `pty.input`.

## Start here
`comm-spawn.sh` for a change to how a row starts; `comm-despawn.sh` for how one ends. The suites that run them are in
`agents/tests/`.

## Rules
- A rollback deletes a provisional row only while it is still provably ours (`registry_del_if_provisional`, run under
  `with_lock`); a row `workspace.create` answered with is never destroyed by `comm-spawn.sh`.
- Spawn derives a name in mode `fresh` (`claim_derived_handle`): an existing row for the name is a refusal, never a
  reclaim.
- A second agent inside a session neither spawns nor despawns rows (`sot_require_agent`, checked before any write).
- `comm-despawn.sh` resolves the target daemon's declared host before destroying; missing identity or a failed destroy changes no row or self file. A confirmed destroy removes only that target's exact workspace self slot and its registry row. It changes nothing when the name resolves to no workspace.
- Local spawn stores the declared host but derives its handle from HANDLE_HOST; remote spawn derives from the target daemon's declared host and writes no local registry row or inbox.
- `comm-bootstrap.sh` and `comm-probe.sh` are the two scripts that still type into rows (`sot_pty_input`).
- `comm-probe.sh` creates, types into and replies to nothing whose handle does not begin `probe`.
