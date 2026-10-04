# rust/protocol/src/ops: typed payloads of every op (wire)

Every `*Req`, `*Res` and `*Evt` struct serializes into the `payload` of a frame; the codec never sees these types. This
folder holds one file per op family, with the op-name constants in `mod.rs`. Part of wire; charter:
rust/protocol/CLAUDE.md.

## Files
- `mod.rs`: the `op` module of op-name constants and the `mod` / `pub use` lines that keep every payload at `ops::X`
- `session.rs`: the connection and client roster: hello, ping, version.query, fe.command, fe.presence, fe.sessions, topology.set
- `session_tests.rs`: wire tests for the session family (hello, version.query, fe.presence, fe.command)
- `browse.rs`: what a client reads and writes of a workspace: trees, preview, files, concept notes, math, transfers
- `repl.rs`: the persistent Julia REPL: eval, run_file, execute and its streamed frames
- `pty.rs`: a row's agent pane: pty.open and the named-row input and screen ops
- `workspace.rs`: rows and accounts: workspace create, list, activate, destroy, reauth, accounts.list
- `agent.rs`: agent messaging: agent.send, agent.filed, agent.receipt, comm.file, agent.join
- `pages.rs`: browser pages (pluto, video, docs, quarto) and proxy.connect
- `monitor.rs`: the monitor drawer's samples and subscriptions
- `update.rs`: update.check and update.apply
- `lane.rs`: lane.connect, the daemon-bridged capsule lane
- `lease.rs`: the window lease ops and the close lifecycle's bounds and exit codes

## Start here
`mod.rs` for an op's name; the family file for its payload. A new op adds its constant to `op` and its types to the
family that owns it.

## Rules
- Op names live only in `op`; a family file never spells an op string.
- A payload grows only by `#[serde(default)]` fields; any other change raises `PROTOCOL_VERSION` (lib.rs).
- Families are glob re-exported, so two families may not export the same name. `lease` is a public module
  (`ops::lease::X` is in use) and re-exports only its payload types, so its constants stay out of `ops::`.
- lease.rs's bounds are pinned by scripts/tests/installer-state.sh against scripts/lib/sot-daemon.sh,
  scripts/sot-local-daemon.ps1 and scripts/launch-sot.ps1 (`LAUNCH_WAIT`, `DAEMON_LOCK_WAIT`, `LEASE_REPLY_WAIT`,
  `HANDOVER_BOUND` and the `fe.lease` golden line); their order is pinned by `bounds_chain`.
