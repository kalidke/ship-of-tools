# comm/mail: the scripts that send and read messages (comm)

A message is delivered when one line is appended, under the inbox lock, to the receiver's inbox. These three scripts are
the sender's and the reader's side of that append; they install flat into `~/.sot-comm/bin` with the other bin folders
and find their siblings (`comm-lib.sh`, `comm-context.sh`) through their own folder there. Part of comm; charter:
comm/CLAUDE.md.

## Files
- `comm-send.sh`: directed and broadcast sends; the registry decides a local append or the wire (it execs `comm-relay.sh send` for a handle it cannot name), and a listed handle that is not live is refused before anything is appended.
- `comm-relay.sh`: the send that rides the daemon: `comm.file` for a handle the hub lists, else `agent.send` and a wait for a filer's receipt; the retired `bridge` verb only sleeps.
- `comm-poll.sh`: shows the inbox lines past the read cursor, then advances it; the only writer of the cursor.

## Start here
`comm-send.sh` for what a send prints and why (`filed -> @h` or `FAILED -> @h: <reason>`); `comm-poll.sh` for reading.

## Rules
- `filed -> @h` is printed only on the appender's word (`sot_inbox_append`, `sot_comm_file` in comm-lib-inbox.sh), and
  only for a live handle: `deliver` refuses a listed handle whose `last_seen` `sot_heartbeat_fresh` does not call fresh.
  The `agent.send` leg of `comm-relay.sh` still ends in `NOT CONFIRMED` when no filer claims the send within 5 seconds.
- Only `comm-poll.sh` moves `read/<h>.cursor`.
- Readers count newline-terminated lines only.
- A message body reaches jq through `sot_jq_rawfile`, never `--arg`.
- Senders type into no row (comm-send.sh's header); the daemon wakes the receiver.
