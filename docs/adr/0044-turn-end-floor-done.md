# ADR 0044: Blue / gray as unread / read — the turn-end floor writes `done`

**Status:** accepted (owner decision 2026-09-08, no aging); implemented in the
comm scripts (deploys via `update_comm`, no release needed).

## Context

The session work-state (ADR 0023 state-nav; `comm-status.sh`) has five
colours: working green, waiting purple, blocked red, done blue, idle gray.
Two of them carried no information:

- `done` was written only when the model remembered to run
  `comm-status.sh done`. Blue therefore meant "the model remembered", not
  anything about the work.
- The `Stop` hook floored every turn end to soft `idle`. Gray therefore meant
  "not mid-turn" — true of every session the user is not typing into,
  whether it just produced a result or was parked three days ago.

The owner asked for better use of both. The bottom session strip now orders
rows by activity (PR #222), which makes a meaningful resting-state split more
visible, not less.

## Decision

Blue and gray become an **unread / read** pair, stamped deterministically by
hooks:

1. **The `Stop` hook floors with a soft `done`** instead of a soft `idle`
   (`comm-status-idle.sh`, `turn_floor`).
2. **`comm-status.sh` turns a soft `done` blue only for a row that was
   `working` from a genuine human prompt.** Every other row floors to gray
   `idle` as before. The guards of the old soft idle are unchanged: a
   `blocked`, an explicit `done`, or a live `waiting` row is never touched;
   an expired sticky marker still self-heals.
3. **Turn provenance is one registry field, `turn_origin`** (`user` |
   `machine`), written only by the soft `working` write. The
   `UserPromptSubmit` hook is the one writer that can tell a human prompt from
   a wake (its existing machine-turn case: system notifications, task
   notifications, relay messages, teammate messages), so it classifies the
   prompt and passes `COMM_STATUS_ORIGIN=user|machine` — and nothing else.
   The 2026-07-04 hierarchy guard (a machine turn must not flip a `blocked`
   or `done` row to green) moves from the hook into `comm-status.sh`'s soft
   working path, beside the sticky-waiting hold it already had, so that a
   held state **still records the origin**. Without that, a machine wake on
   a red or blue row left a stale `user` behind, and an explicit model
   `working` later in that turn ended blue (review finding on #223). Nothing
   else reads the field. **Absent provenance fails gray**: a row stamped
   `working` before the field existed (a not-yet-updated box, a mid-update
   turn) or by any writer other than the prompt hook floors to `idle`, and a
   soft `working` write without an explicit origin records `machine`. Blue
   is opt-in. The invariant the field serves: *blue is reserved for a turn a
   human asked for.*
   Without it every peer ack would paint a parked row blue and the colour
   would be noise again.
4. **Blue clears on the next genuine prompt** (the working hook, unchanged)
   or any explicit report. **No time-based decay** (owner: honest, no aging).
   A parked blue row means "you never came back".

## Consequences

- A session the user is actively talking to cycles green → blue → green. The
  active row being blue between turns is correct: it has a reply the user has
  not yet acted on.
- A **machine-started turn that did real work ends gray.** This is the
  accepted residual: the peer that tasked it gets a comm report anyway, and
  the model reports `done "<summary>"` explicitly when a job lands — the
  skill rule that already existed, now with a colour that means something.
- A "seen" mark (the user switched to the workspace but did not type) would
  need an FE → registry write path, which does not exist. Deferred; ship
  without it and evaluate.
- The strip ordering (PR #222) keeps `done` in the resting tier for now. A
  follow-up may promote it to a needs-you tier beside `blocked`; that is a
  one-line `activity_rank` change once this has been lived with.
- Codex sessions share the same scripts (ADR 0031), so they get the same
  floor with zero new logic.

## Verification

Fourteen scenarios run against a scratch registry (`SOT_COMM_HOME` pointed at a
throwaway home, a scratch handle, mutations only through `comm-status.sh` and
the two hooks): user turn → blue; machine turn → gray; blocked / explicit done
/ sticky waiting untouched by the floor; explicit idle then floor → gray;
legacy soft idle unchanged; explicit done cleared by the next genuine prompt;
blocked answered → green → blue; a machine wake on a red, blue or sticky-purple
row holds the colour and records `machine`, so an explicit `working` in that
turn ends gray; a genuine prompt on a sticky-purple row records `user` and an
explicit `working` in that turn ends blue; a pre-field `working` row and an
origin-less soft write both floor gray.
