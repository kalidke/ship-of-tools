# rust/backend/src/comm/mail: delivery in the daemon (messaging)

Mail is delivered when one line is appended to the receiver's inbox file. This folder is the daemon's part of delivery.
Part of messaging; design of record: docs/adr/0049-messaging-on-one-page.md.

## Files
- `bus.rs`: the payloads of the daemon's `agent.message` and `agent.receipt` broadcast buses
- `filer.rs`: comm.file and the filing behind it
- `hub_link.rs`: the link to the hub that files relayed `agent.message` frames into this box's inboxes
- `mod.rs`: declares the files
- `relay.rs`: agent.send and agent.filed

## Start here
`hub_link.rs` `run`, spawned once by `server::run`. On a box whose topology relay endpoint to the hub is `ssh:`, it holds
one ssh stdio child to the hub. On that link it files each `agent.message` whose `to` this box's registry lists, through
`filer.rs` `file_comm` (what `comm.file` uses), and answers `agent.filed {id}`.

## Rules
- The link reads the topology once, at start (`recipe_for`).
- `hold_link` backs off from 1 s, doubling to 30 s, and starts over after a connection that lasted 60 s. It never
  reconnects once the shutdown signal fires.
- `move_fe_inbox` runs before the first connection. A failed move leaves the old inbox in place.
- `AgentMessage.to == ""` is a broadcast.
