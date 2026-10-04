# rust/backend/src/comm: the daemon's half of messaging (backend)

Messaging between sessions is a pair of halves: the comm scripts at the repo's `comm/` and, here, the daemon's part of
delivery, of the address book and of the wake. Part of the backend; charter: comm/CLAUDE.md.

## Files
- `mail/`: delivery: `comm.file`, the forward to the hub, the hub link, the relay of `agent.send` and `agent.filed`
- `mod.rs`: declares the three folders and holds the comm folder rule (`sot_comm_home`)
- `registry/`: the address book: which handle names which session, and the registry's lock, writes and poll
- `wake/`: the wake: types a line into a session's free prompt when mail is unread

## Start here
`mail/` for delivery, `registry/` for the address book, `wake/` for the wake of idle sessions.

## Rules
- A folder here holds the daemon's side only; the scripts' side stays in the repo's `comm/`.
- `sot_comm_home` is the one comm folder rule: `$SOT_COMM_HOME` when set and non-empty, else `$HOME/.sot-comm`, else
  `$USERPROFILE/.sot-comm`, else none.
