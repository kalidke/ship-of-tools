# rust/backend/src/comm/mail: delivery in the daemon (messaging)

Mail is delivered when one line is appended to the receiver's inbox file; this is the daemon's part of it. Part of
messaging; charter: comm/CLAUDE.md.

## Files
- `bus.rs`: the payload of the daemon's `agent.message` bus: the UI notifications `agent.send` publishes
- `filer.rs`: `comm.file`: the verdict on one frame, and the append or forward behind it
- `hub_link.rs`: this box's link to the hub (role `link`), which files the `comm.file` requests the hub offers into this
  box's inboxes and answers each; and the one-time move of the old Windows frontend inbox
- `inbox.rs`: the daemon's append of one frame to an inbox file under the shared lock, and the record of who may append
- `inbox_tests.rs`: the tests of `inbox.rs`
- `links.rs`: the hub's linked daemons: a `comm.file` its own folder does not list is offered to each, and theirs is the
  answer
- `mod.rs`: declares the files, and `record_at_boot`, the boot-time lock record
- `relay.rs`: `agent.send`: one UI notification published to every connection; mail is refused

## Start here
`filer.rs` `comm_file_verdict` for what `comm.file` accepts or refuses; `inbox.rs` `file_frame` (one append) and `route`
(where a filing goes); `handle_comm_file` offers what this folder does not list to `links::Links`. `hub_link.rs`
`run`, spawned once by `server::run`, holds one ssh stdio child to the hub as a `link` connection and answers each
`comm.file` request the hub sends on it with `file_offer`'s verdict.

## Rules
- `file_frame` appends one line to `inbox/<h>.jsonl` under flock on `inbox/<h>.lock`, the lock comm-lib-inbox.sh's
  `sot_inbox_append` takes, waiting at most `inbox_lock_wait` (`SOT_INBOX_LOCK_WAIT_SECS`, 2 by default in both
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
- The comm forward (`dial_and_call`) waits `inbox_lock_wait` plus `COMM_FORWARD_SLACK` (1 s) at most, or until the shutdown signal
  fires; either way it kills the ssh child and what it started before returning.
- `handle_comm_file` offers a request this folder answers `not_here` to every linked daemon at once (`Links::offer`),
  unless it arrived `forwarded`, giving each `inbox_lock_wait` plus `COMM_FORWARD_SLACK`. The answer is the first `ok`;
  else the first refusal that is not `not_here`, by host; else a link that did not answer in time; else `not_here`,
  naming the boxes `hub_link::unlinked` finds (a box that holds a link by `link_target`'s rule and has none attached).
  An offer is marked `forwarded`. A linked daemon answers it with `filer::file_offer`: `not_here` unless its own
  registry lists the handle under its own host, and only then the route, liveness and append; it never offers or
  forwards it again. A link is attached before its connection is on the roster and leaves with the connection, failing
  what it still owes.
- `handle_agent_send` publishes one `AgentMessage` when `to` is empty and answers `ok`; a non-empty `to` is mail and is
  refused `mail_requires_comm_file`, publishing nothing.
- inbox.rs stays std and serde only outside its macOS arm: tests/comm_file.rs includes it by path.
- The link reads the topology once, at start (`recipe_for`). `hold_link` waits on platform's `Redial` from 1 s, doubling to 30 s,
  starting over only after a link that lasted `STABLE` (60 s), and never reconnects once the shutdown signal fires; the link's ssh starts through `Signal::spawn`, so its tree dies at
  the fire. `move_fe_inbox` runs
  before the first connection; a failed move leaves the old inbox in place.
- Once its hello is answered the link pings the hub every 30 s (`PING_EVERY`), and it ends when no frame has come from
  the hub for three intervals (`SILENT_PINGS`, 90 s, the hello's reply included), logging `hub link dropped: nothing
  from the hub in 90s` with no ssh line after it: ssh is up, so its last line is stale. ssh's ServerAlive covers only
  a dead network or sshd, not a hub daemon or `sotd stdio-bridge` stopped behind a live sshd. Frames count, not bytes:
  nothing the hub sends a `link` connection carries a blob. The hub reaps the link, as it reaps any connection that
  has pinged, after 90 s with no frame from it.
- The silence rule holds because the hub answers `hello` and `ping` in its connection loop, and the link sends no
  other request. A request the hub could make wait must not ride this link: a busy hub would be cut as a silent one.
  The link files each `comm.file` request in a task of its own (`offer_answer`) and answers it when that filing ends,
  so one held lock delays no other answer.
