# rust/backend/src/comm/mail: delivery in the daemon (messaging)

Mail is delivered when one line is appended to the receiver's inbox file; this is the daemon's part of it. Part of
messaging; charter: comm/CLAUDE.md.

## Files
- `bus.rs`: the payloads of the daemon's `agent.message` and `agent.receipt` broadcast buses
- `filer.rs`: `comm.file`: the verdict on one frame, and the append or forward behind it
- `forward.rs`: a guest daemon's `comm.file` forward to the hub, bounded by a deadline and the shutdown signal
- `hub_link.rs`: the link to the hub that files relayed `agent.message` frames into this box's inboxes
- `inbox.rs`: the daemon's append of one frame to an inbox file under the shared lock, and the record of who may append
- `inbox_tests.rs`: the tests of `inbox.rs`
- `mod.rs`: declares the files, and `record_at_boot`, the boot-time lock record
- `relay.rs`: `agent.send` and `agent.filed`: one broadcast each to every connection

## Start here
`filer.rs` `comm_file_verdict` for what `comm.file` accepts or refuses; `inbox.rs` `file_frame` (one append) and `route`
(where a filing goes). `hub_link.rs` `run`, spawned once by `server::run`, holds one ssh stdio child to the hub and
files each `agent.message` whose `to` this box's registry lists through `file_comm`, answering `agent.filed {id}`.

## Rules
- `file_frame` appends one line to `inbox/<h>.jsonl` under flock on `inbox/<h>.lock`, the lock comm-lib-inbox.sh's
  `sot_inbox_append` takes, waiting at most `inbox_lock_wait` (`SOT_INBOX_LOCK_WAIT_SECS`, 10 by default in both
  languages). The lock is released when its guard drops (`InboxLock`'s `Drop`), not when the last copy of its descriptor
  closes. `append_line` first cuts an unterminated tail back, syncs the line (`File::sync_data`) before `Ok`, and
  cuts the file back on any error. An `Err` is the sentence printed after `FAILED -> @h: `: nothing was appended.
  `handle_comm_file` answers `ok` only when `file_frame` returned `Ok`, so only after the sync.
- `route` appends locally only when line 1 of `inbox-lock-manager` equals this daemon's own `lock_identity`; otherwise
  a guest forwards and a hub refuses, naming the recovery (`refusal`). Only the hub writes that record, at start
  (`record_at_start`, called by `record_at_boot` from main).
- `comm_file_verdict` lists a handle only when `.agents[h].host` is non-empty. On the append route it then needs a
  live holder: a `last_seen` under `LIVE_SECS` (600) (`heartbeat_fresh`, the twin of the shell's
  `sot_heartbeat_fresh`), stamped by the session and by the daemon running its row (`comm/registry/liveness.rs`). A
  forward skips both tests and returns the hub's answer verbatim.
- `forward_comm_file` waits `inbox_lock_wait` plus `COMM_FORWARD_SLACK` (5 s) at most, or until the shutdown signal
  fires; either way it kills the ssh child before returning.
- `handle_agent_send` broadcasts one `AgentMessage` (`to == ""` is a broadcast); `handle_agent_filed` broadcasts one
  `AgentReceipt` whose `filer` is the answering connection's hello name, and refuses `bad_filer` when it has none.
- inbox.rs stays std and serde only outside its macOS arm: tests/comm_file.rs includes it by path.
- The link reads the topology once, at start (`recipe_for`). `hold_link` backs off from 1 s, doubling to 30 s, starts
  over after a connection that lasted 60 s, and never reconnects once the shutdown signal fires. `move_fe_inbox` runs
  before the first connection; a failed move leaves the old inbox in place.
