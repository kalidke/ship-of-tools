# rust/backend/src/comm/mail: delivery in the daemon (messaging)

Mail is delivered when one line is appended to the receiver's inbox file. This folder is the daemon's part of delivery.
Part of messaging; design of record: docs/adr/0049-messaging-on-one-page.md.

## Files
- `bus.rs`: the payloads of the daemon's `agent.message` and `agent.receipt` broadcast buses
- `filer.rs`: comm.file and the filing behind it
- `hub_link.rs`: the link to the hub that files relayed `agent.message` frames into this box's inboxes
- `inbox.rs`: the daemon's append of one frame to an inbox file under the shared lock, and the record of who may append
- `mod.rs`: declares the files
- `relay.rs`: agent.send and agent.filed

## Start here
`hub_link.rs` `run`, spawned once by `server::run`. On a box whose topology relay endpoint to the hub is `ssh:`, it holds
one ssh stdio child to the hub. On that link it files each `agent.message` whose `to` this box's registry lists, through
`filer.rs` `file_comm` (what `comm.file` uses), and answers `agent.filed {id}`.

`inbox.rs` `file_frame` (one append) and `route` (where a filing goes).

## Rules
- `file_frame` appends one line to `inbox/<h>.jsonl` under flock on `inbox/<h>.lock`, the lock comm-lib.sh's
  `sot_inbox_append` takes. It waits at most `inbox_lock_wait` (`SOT_INBOX_LOCK_WAIT_SECS`, 10 by default in both
  languages); otherwise nothing is appended.
- An `Err` is the sentence printed after `FAILED -> @h: `, and it means nothing was appended.
- `append_line` first cuts an unterminated tail back. It syncs the line (`File::sync_data`) before `Ok`, and any error
  cuts the file back.
- This daemon appends locally only when line 1 of `inbox-lock-manager` equals its own `lock_identity` (`route`).
  Otherwise a guest forwards and a hub refuses, naming the recovery (`refusal`).
- Only the hub writes the record, at start (`record_at_start`).
- inbox.rs stays std and serde only outside its macOS arm, because tests/comm_file.rs includes it by path. T13 in
  test-hub-files.sh reads inbox.rs.
- The link reads the topology once, at start (`recipe_for`).
- `hold_link` backs off from 1 s, doubling to 30 s, and starts over after a connection that lasted 60 s. It never
  reconnects once the shutdown signal fires.
- `move_fe_inbox` runs before the first connection. A failed move leaves the old inbox in place.
- `AgentMessage.to == ""` is a broadcast.
