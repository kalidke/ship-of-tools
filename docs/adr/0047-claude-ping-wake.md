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
  shares the same request) and only types when a line is exactly the prompt
  glyph. Typing into an open permission dialog or menu can answer it, so an
  unclear screen delays the wake rather than risk that.
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
- **`--owner <pid>` is mandatory for `--deliver ping`** (exit 2 without it) and
  the marker `state/<handle>.watch` is a start-time mutex: a live pid in it
  refuses a second watcher (exit 4). The `pgrep` dedupe
  `comm-session-start.sh` did from the outside is gone with it — a process
  match could never tell whose session armed a watcher. `--deliver full`
  (Codex's leg) keeps the optional tie: its documented start line passes no
  pid, and where Codex's owner pid comes from is not settled here.
- **The prompt-free gate is one implementation**, `comm-lib.sh`'s
  `sot_prompt_free`, shared with the sender's poke (`sot_pty_input_gated`), so
  "free prompt" cannot come to mean two different things.

The consequence above — "a message can sit typed-but-unread for up to 10
minutes" — no longer holds: an unread ping delays nothing past the recipient's
next turn boundary. This whole script is scheduled for deletion in 0.6.7, once
the bridge pokes inline and the daemon files for its own rows.
