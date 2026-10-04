# rust/frontend/src/net/transport/ops: the window's daemon ops, one file per family (fe-net)

Each op the window sends has a `send_<op>` here: it writes the request frame and records the
`PendingKind` its reply needs. `send_request` (../request.rs) picks the function by `OutgoingReq`
variant; for a reply, `handle_response_frame` (../reply.rs) removes the pending entry and calls
`on_<op>` by `PendingKind`, which turns the frame into an `IncomingEvt`. An op family's file also holds the types its reply becomes (e.g. `WorkspaceInfo` in workspace.rs); mod.rs re-exports them to the UI. Part of fe-net; charter: rust/frontend/src/net/CLAUDE.md.

## Files
- `mod.rs`: declares the families and re-exports their functions to the transport
- `tree.rs`: tree.children, tree.root, nav.toggle_hidden, directory.list
- `preview.rs`: preview.get, preview.set_scale, image.crop, math.render
- `concept.rs`: concept.read, concept.write
- `files.rs`: file.read, file.write, file.delete, dir.create, file.download, file.upload
- `kernel.rs`: kernel.request ops: project.scan, markdown.tokenize, file.parse, function.methods
- `workspace.rs`: workspace.activate, .create, .list, .destroy, accounts.list, fe.presence, fe.sessions, pty.open, agent.send
- `repl.rs`: repl.eval, repl.interrupt, repl.run_file
- `pages.rs`: pluto.open, video.open, docs.open, quarto.open
- `monitor.rs`: monitor.subscribe, monitor.unsubscribe, monitor.history

## Start here
A new op: its `OutgoingReq` variant (../request.rs), a `send_<op>` in its family's file, and one
arm in `send_request`; and an `on_<op>` with its `PendingKind` variant and arm in
`handle_response_frame`.

## Rules
- A `send_<op>` writes its frame, then inserts its `PendingKind` under the same id. A send that
  inserts none is fire-and-forget, and its reply reaches the UI as `IncomingEvt::Event`:
  `send_toggle_hidden`, `send_workspace_activate`, `send_fe_presence`, `send_fe_sessions`,
  `send_repl_interrupt`, `send_monitor_unsubscribe`, `send_agent_send`.
- `send_figure_get` alone inserts before it writes, so `PendingGuard`'s drop reports a figure whose
  write failed (`FigureGetFailed`).
- `on_file_download` alone puts its entry back, with its open file, until the chunk marked eof (one
  request id, many replies).
