# ADR 0047: Claude sessions wake on a ping, not a harness Monitor

**Status:** accepted; implemented in the comm scripts (deploys via
`update_comm`, no release needed).

## Context

A Claude session in a capsule row is woken today by a harness Monitor
running `comm-watch.sh <handle>`. The harness kills every Monitor after 30
minutes, so an idle session spends a model turn re-arming it — about 48
empty turns a day per idle row, visible as spam in the LLM pane and billed
as usage for no work done.

Codex has no Monitor primitive at all, so it already solved this outside the
harness: `codex-watch.sh` polls the handle's inbox and types each new
directed frame straight into the row's capsule via the daemon's `pty.input`
op — a plain background process, costing nothing to keep alive.

## Decision

Generalize the Codex mechanism into `comm-wake.sh <handle> --deliver
full|ping`, shared by both agents. `codex-watch.sh` becomes a two-line shim
to `--deliver full` (Codex's behaviour, unchanged). Claude sessions get
`--deliver ping`:

- **One fixed notice, never the message.** A batch of new directed frames
  types a single line (`[sot-comm] new message for @<handle> — run
  comm-poll.sh`, or a selftest-specific line when every new frame is the
  wake-proof selftest) — the session reads the real backlog itself. A burst
  of N messages costs one wake, not N.
- **Prompt-free gate.** Before typing, it reads the row's current screen
  (`pty.screen`, lifted into `comm-lib.sh`'s `sot_pty_screen` so `sot-fe`
  shares the same request) and only types when the CURSOR is sitting at the
  start of an empty input line. Typing into an open permission dialog or menu
  can answer it, so an unclear screen delays the wake rather than risk that.
  The rule itself is stated once, below, under the prompt-free gate.
- **Coalescing.** An already-typed, not-yet-read ping (the session's poll
  cursor hasn't moved past it) suppresses a second one; new lines simply
  wait for the outstanding wake, capped at 10 minutes in case the ping is
  ever missed entirely.
- **Self-ending lifetime.** It finds the owning `claude`/`codex` process once
  at startup and exits the moment that process is gone, instead of running
  forever as an orphan. It writes the same liveness marker `comm-watch.sh`
  does, so `_survived` and the heartbeat hook need no changes.
- **`comm-session-start.sh` starts it automatically** for a capsule row
  (workspace id resolves) instead of printing something for the session to
  arm by hand — there's no Monitor tool call to make. Outside a capsule row,
  or for Codex (which starts its own `--deliver full` watcher from its own
  skill), the existing `MONITOR:` line and Monitor-arming flow are
  unchanged.

## Consequences

An idle Claude session in a capsule row is genuinely deaf-while-idle no
longer, and costs nothing to stay that way — no more 30-minute re-arm turns.
The cost moves to the wake path: a message can sit typed-but-unread for up
to 10 minutes if the session never polls, and a screen stuck on an open
dialog delays (never drops) the wake until it clears. Sessions outside a
capsule row keep paying the Monitor's re-arm cost, unchanged.

## Revision — 2026-09-26: the ping is a poke, not the delivery

The messaging ruling moves delivery off this mechanism entirely, which changes
what a missed ping costs and therefore what this watcher is allowed to do.

- **Delivery is the inbox append, and the ack is FILE.** A send to a handle
  this host's registry names is a local append; `comm-relay.sh` takes that
  route itself and answers `filed -> @handle`. "The relay is live-only and only
  a reply proves the path" is **withdrawn** — it pushed a design defect onto
  every caller.
- **A busy session is reached at its turn boundary**, in band: its `Stop` hook
  (`comm-status-idle.sh`) reads its own inbox and blocks the turn-end with "run
  comm-poll.sh" while directed mail is newer than the read cursor. No process,
  no keystrokes, no human. That is what makes a missed ping harmless.
- **Coalescing is deleted.** `_comm_wake_ping_outstanding` withheld a ping for
  up to 600s while an earlier one sat unread, so one stalled session went deaf
  to everything queued behind the message it never read. A genuinely new line
  now always pings. The one suppression left is content-based: a pending line
  the cursor's own ts already covers was read through a real poll.
- **Every leg is owned, and the owner is DISCOVERED.** `sot_owner_pid`
  (comm-lib.sh) walks up to the nearest `claude`/`codex` ancestor, so
  `comm-wake.sh` and `comm-listen.sh` find their own owner and REFUSE to start
  when none can be found (exit 2). `--owner <pid>` remains only as an override
  for a caller with a better vantage — `comm-session-start.sh` passes it to the
  watcher it backgrounds, because a watcher whose parent exits first reparents
  to init and its own walk would find nothing. Nothing depends on a flag a
  caller could forget, which is what lets the rule cover Codex's `--deliver
  full` leg too.
- **The marker `state/<handle>.watch` is a start-time mutex, verified by
  identity.** A live pid in it refuses a second watcher (exit 4) — but only
  after `sot_watcher_pid_for` confirms that pid IS a watcher for this handle.
  The marker outlives reboots on a shared home, and `codex-watch.sh` writes the
  same file, so trusting `kill -0` alone would let a reused pid refuse every
  start for that handle while the survival check reported healthy. The `pgrep`
  dedupe `comm-session-start.sh` did from the outside is gone: a process match
  could never tell whose session armed a watcher.
- **The read cursor is a line offset, not a timestamp.** Stamps are
  second-resolution and every comparison was strictly-greater, so a frame filed
  in the same second as one already read was never shown and never announced
  while its sender printed success. `comm-poll.sh` writes a line count;
  `sot_cursor_offset` converts a legacy stamp once, on read.
- **The prompt-free gate is one implementation**, `comm-lib.sh`'s
  `sot_prompt_free`, shared with the sender's poke (`sot_pty_input_gated`), so
  "free prompt" cannot come to mean two different things. It tests the
  CURSOR sitting at the input's start, not the prompt line's text, because a
  grey prompt suggestion is byte-identical to a typed draft once `pty.screen`
  strips every attribute — the text alone cannot tell them apart. This
  replaced a glyph-only test that held every wake for a day on a row showing
  a suggestion (2026-09-25).

The consequence above — "a message can sit typed-but-unread for up to 10
minutes" — no longer holds: an unread ping delays nothing past the recipient's
next turn boundary. This whole script is scheduled for deletion in 0.6.7, once
the bridge pokes inline and the daemon files for its own rows.

## Revision — 2026-09-28: the watcher reads BOTH inboxes, so Windows needs no Monitor

A frontend box runs no relay bridge: its frontend files every inbound frame
into its own `fe-inbox.jsonl` (shared by every handle on the box), while a send
from a session on the SAME box still lands in `inbox/<handle>.jsonl`. This
watcher read only the second file, so on such a box it could never see the mail
that arrives from anywhere else — and a session there stayed on the harness
Monitor, which already read both. `comm-wake.sh` now polls each source with its
own in-memory cursor and its own admission rule (`.to` equal to our handle for
the shared file, any directed frame for the per-handle one), exactly the pair
`comm-watch.sh` applies. `sot_fe_inbox_path` remains the one place the platform
branch lives, so off Windows there is a single source and nothing changes.

The DECISION stays one per cycle, not one per source: the ping says only that
mail exists, so a frame in each inbox in the same two seconds is one typed
notice, one row resolution and one probe of the prompt-free gate — which is
also what keeps the five-probe give-up budget a count of cycles. `--deliver
full` is unchanged and still per line: it types each message itself, so a
second source is simply a second batch.

Two smaller things fell out of the same path, both of which kept a frontend box
on the Monitor or hid mail from it:

- `sot_owner_pid` could not name an ancestor under git-bash (no `ps -o`, no
  `<pid>/comm`), so the bootstrap reported "no owning claude/codex ancestor
  found" and fell back. It now reads `Name:` from `<pid>/status`, the same file
  it already read `PPid:` from, and tolerates a `.exe` suffix.
- The bootstrap's catch-up carried its own copy of the frontend-inbox read,
  with a THIRD cursor file that no other reader has ever opened — so catch-up
  marked frontend mail read where `comm-poll.sh` and the turn-end hook could not
  see it. Deleted; `comm-poll.sh` is the one reader on every platform.

A SILENT DAEMON NO LONGER COSTS A BOX ITS WAKE PATH, at either end of the
mechanism. The watcher used to exit after five unanswered `workspace.list` or
`pty.screen` requests, and the bootstrap used to require a live `pty.screen`
before it would spawn one at all — so a daemon quiet for one second printed
`MONITOR:`, and since nothing re-arms a watcher, that session stayed on the
Monitor for the rest of its life. The watcher now slows its poll to 30s after
five silences and keeps waiting, and the bootstrap arms it without probing.
The immortal-watcher reason for that exit is gone: `_comm_wake_owner_alive`
ends the process with the agent it serves. A row that is GONE
(`unknown_workspace`) still ends the watcher — that is a different fact from a
daemon that did not answer. Nothing about delivery changes: the inbox append
is the delivery, and the recipient's Stop hook blocks its turn end on unread
directed mail, so an outage costs the wake and nothing else.

The Monitor itself is unchanged and still the fallback for a session in no
capsule row, on any platform: there is no pane to type into.
