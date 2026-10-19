# ADR 0044: Blue / gray as unread / read — the turn-end floor writes `done`

**Status:** current — accepted (owner decision 2026-09-08, no aging); implemented in the
comm scripts (deploys via `update_comm`, no release needed).

**Amended 2026-09-18 (owner: "it should be green after the prompt until
something else takes over"):** the hierarchy guard that held a `blocked` or
`done` row through a machine-started turn is deleted. Any prompt paints
green; a machine turn still floors gray, and a question the turn did not
answer comes back red through its closing `SITREP-QUESTION:` marker. The
only remaining hold on a soft `working` is a `waiting` row with a live
sticky marker on a machine turn. The passages below describing the red and
blue hold are historical.

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
- A "seen" mark (the user switched to the workspace but did not type) —
  flagged above as deferred, needing an FE → registry write path that did
  not exist — **shipped 2026-09-08** ("Viewing clears blue"):
  `workspace.activate` carries `read: bool`, set by the frontend only on
  the two person-driven view switches (Sessions-Enter, Shift+Left/Right
  cycling) — and, since the owner's 2026-09-08 evening ruling, only after a
  10 s dwell on that view (a blow-through while cycling never reads a row;
  the frontend times it, the daemon is unchanged); the daemon flips a
  `done` row's registry state to `idle` and
  writes nothing else (summary and `status_at` survive). Both blues clear
  the same way — the registry doesn't record which writer stamped
  `done`. Full mechanism, lock protocol, and the deliberately-left-out
  list: `comm/adapters/claude/sot-comm/references/work-state.md`.
- The strip ordering (PR #222) ranks the tiers red, white (badged), blue,
  green, purple, gray (owner ruling 2026-09-08), so a blue row sits right
  behind the rows that need the user most.
- Codex sessions share the same scripts (ADR 0031), so they get the same
  floor with zero new logic.

## Review round 2 (Codex on #223)

- **Read-decide-write is one critical section.** Every guard read (state,
  sticky age, `turn_origin`) now runs inside `with_lock`, in `status_txn`.
  Before, the reads ran before the lock and the locked jq only checked row
  existence, so a `done` committed while the floor waited was overwritten
  with `idle`, and a machine start committed while a `working/user` floor
  waited was painted blue. Both races are reproduced in the test suite via
  the lock's barrier seam.
- **A failed mutation is a failed script.** The write helpers return the jq
  or `mv` status; the trailing temp-file cleanup no longer masks it, and
  `with_lock` propagates it as the exit code.
- **Deployment-order tolerance.** The Stop hook sends soft `done` only to a
  `comm-status.sh` that has the soft floor (it greps for `soft_floor`), else
  soft `idle` — an old script guarded only `idle` and would have painted a
  blocked or waiting row blue between the two copies landing.
- **History pruned.** `comm-status.sh`'s header now states the current
  contract only; this ADR holds the decision and its history.
- **Regression suite.** `comm/core/tests/test-status-floor.sh` (23 cases:
  the state scenarios, both races, both failed-write paths, the hook
  tolerance) runs in the ubuntu CI leg beside the disambiguation suite.

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

## Update 2026-09-09 — closing markers

The floor is now the fallback, not the word. A reply whose closing block opens
a line with `SITREP:`, `SITREP-QUESTION:` or `SITREP-WAITING:` declares the
turn's end state (done / blocked / waiting) and carries the report that state
demands (the `sitrep` skill holds the three shapes). The Stop hook stamps that
state explicitly, with the rest of the marker line as the row summary, so the
chat and the nav row come from one sentence. Without a marker, a human turn
that ends parked (blocked / waiting / done) gets one nudge naming the shape
it owes; a machine wake never nudges, and a plain answer floors as above.
Seven more scenarios cover it in the same suite. Mechanics: the sot-comm
skill's `references/work-state.md`.

## Amendment 2026-09-19 — the row is a set of facts; the colour is their reduction

**Owner ruling.** A session row can hold several facts at once. The display
shows ONE colour, by priority: white/star (a result badge or an unread
message, cleared by viewing) > red (a question open for the owner AND the
session stopped) > green (the session has the floor) > purple (a launched
job/subagent/peer still running) > blue (an effort closed, unviewed) > gray.
Red and green outrank purple *structurally*, so "the user comes first" and
"a resumed turn is green with jobs in flight" need no hold logic. The
frontend is unchanged: one `state` per row; the badge is its own overlay.

### Decision

**1. Facts, not one state.** The registry row carries these fields, each
serving one invariant:

| field | value | invariant |
|---|---|---|
| `floor` | `user` \| `machine` (absent = stopped) | green = the session is running, whoever prompted it; the value is the turn's provenance (blue and the Stop nudges are reserved for a turn a human asked for — ADR 0044 §3, unchanged) |
| `question` | the question text | red = a question is open for the owner; the row shows which |
| `waiting` | the wait summary | purple = something the session launched is still owed; the row shows what |
| `done` | `true` | blue = a result landed that the owner has not viewed |
| `note` | the declaration's own line | the displayed line returns to it when red or purple lifts |
| `state`, `summary` | the reduction | the daemon and frontend read one display state and one line, as today |
| `status_at` | as today | the stale-green wilt |

Absent all four facts, the row is gray. `turn_origin` folds into `floor`'s
value; `sticky` / `sticky_at` fold into `waiting`. Deleted: the sticky
marker, `soft_floor`, `marker_live`, `write_origin`, the hold ladder, the
2 h sticky self-heal (purple is the session's declaration and ends when it
says `working`/`idle`/`done`; a time-out would hide a session that failed
to report — the same "no aging" ruling blue already has), the Stop hook's
`soft_floor` grep, the heartbeat's promote/demote arithmetic, and the
Stop hook's waiting nudge (a wait carried over from an earlier turn would
nudge every short exchange on that row; the marker stamps stay).

**2. The reduction, computed in ONE place: `comm-status.sh`.** Every write
re-reduces and stores `state` and `summary`:

```
question set and floor absent  -> blocked   summary = question
floor present                  -> working   summary = note
waiting set                    -> waiting   summary = waiting
done set                       -> done      summary = note
otherwise                      -> idle      summary = note
```

Option (a) over reducing in the daemon: the only fact the daemon owns is
"the owner viewed this row", and the badge lives in the frontend's own
pending-result map (ADR 0025 §1). Reducing in Rust would change the registry
contract for no fact the script cannot see; the daemon's one write (§4)
needs no knowledge of the priorities because `done` is the lowest tier.

**3. Writers are events (hooks) or declarations (the model).** The
`COMM_STATUS_SOFT` flag is deleted; the verb says who writes.

- Hook events: `prompt` (`COMM_STATUS_ORIGIN=user|machine`, default
  machine): sets `floor` to the origin; a *user* prompt also clears
  `question` and `done` (typing into the session answers and reads it); a
  machine prompt clears nothing. *Amended (2026-10-14):* the joining turn's
  `prompt` found no row or set a floor that the join's new row replaced, so
  that turn read idle until it ended. `prompt` and `stop` now also write and
  remove the session's turn mark (`state/turn-<self slot>`), row or no row,
  and `comm-join.sh` sends a third event, `join`, which sets `floor` to
  machine only from a mark its own session wrote under five minutes before.
  A session whose hooks are off or missing leaves no mark, so its join sets
  no floor that no Stop would end, and a turn that ended before a join is
  never floored by it.
  `stop` (the Stop hook, sent at EVERY turn
  end, after any marker stamp): sets `done` when `floor` was `user` and
  neither `question` nor `waiting` is set, then clears `floor`. The
  PostToolUse heartbeat writes no fact: it refreshes `status_at` when
  `floor` is set and the stamp is over a minute old (unlocked read first,
  no write otherwise, as today). It never sets a floor — a subagent or lane
  sharing the lead's handle would otherwise paint a stopped, red or purple
  lead green for hours. A hook-less machine wake therefore runs without
  green; its Stop's origin correction and `stop` still close it correctly.
- Declarations: `blocked "<q>"` sets `question` (keeps `waiting`: red
  outranks purple, and the wait returns when the answer turn ends).
  `waiting "<s>"` sets `waiting` (keeps `question`; *Amended (2026-10-01):*
  `waiting` now clears `question`, and a `blocked` or `waiting` with no text
  of its own is refused). `working`, `idle` and
  `done` clear `question` and `waiting`; `done` sets `done`, the other two
  clear it.
- `AskUserQuestion` (Claude) is the session yielding to the owner while the
  harness is paused: its PreToolUse hook sends `blocked` then `stop`, and
  clears the heartbeat's throttle tick so the answer is never swallowed. The
  answer arrives as the tool's PostToolUse: the heartbeat sends `prompt`
  with origin `user` for that tool name (the owner typed it). *Amended
  (2026-10-19):* a Codex permission prompt is no longer this shape (see the
  dialog amendment below); its answer never arrived as a PostToolUse.
- The Stop hook's marker parsing (`SITREP:` / `SITREP-QUESTION:` /
  `SITREP-WAITING:` → `done` / `blocked` / `waiting`) is unchanged; it
  reads `floor` for the turn's origin before `stop` clears it. "Parked
  without a marker" means `question` or `done` set at Stop — both are
  per-turn facts, since a user prompt clears them. The turn auditor's
  stale-waiting check reads `waiting` where it read `sticky`.

**4. Viewing clears `done` and the badge, never `question` or `waiting`.**
The daemon's read-clears-blue write (`clear_comm_unread`) removes the `done`
key whenever a person views the row, and sets `state` to `idle` only when
the state is `done` — a `done` hidden under a running floor or a wait is
still unviewed until then, and removing it leaves the reduction consistent
because nothing below blue exists. The frontend clears its badge on the same
person-driven view switch it does today. Viewing is not answering and not
finishing a job: a red or purple row's facts are untouched.

**5. Stale green.** Unchanged: the heartbeat refreshes `status_at` once a
minute while `floor` is set; the frontend wilts `working` after ten minutes.

**6. Deployment.** No migration block: rows are rewritten at every turn,
the script and hooks deploy together, and the daemon and frontend read
`state` throughout. The first new write deletes the three legacy keys; a
row parked at the instant of deployment keeps its colour until its next
event. The old-hook-on-new-script window is sub-second.

**7. Test suite.** `test-status-floor.sh` becomes the executable spec of
the reduction: one case per table row, the lifecycle cases re-expressed
against the facts, every marker, nudge, audit, race, failed-write and deaf
case kept except the waiting nudge, and the sticky, hold and
deployment-tolerance cases deleted with the code they tested.

### Consequences

The three field-history failures are structural now: a machine prompt
cannot hold purple over a running turn; a session cannot end
SITREP-WAITING on the owner's question without showing red once stopped;
an unanswered question returns red without re-emitting the marker.
`comm-status.sh` trades its hold ladder for a six-line reduction. White
today means the frontend badge only: no "unread message" row fact exists
yet; when one does it joins the badge as an overlay under the same rule.

## Amendment 2026-10-19 — a permission dialog is the `dialog` fact

A row waiting on a permission dialog read green (Claude: no hook fired) or stayed
red after the answer (Codex: `blocked` then `stop`, cleared only by a typed prompt).

- `dialog` is one more event fact: the new line on top of the reduction,
  `dialog set -> blocked, summary = dialog`, reads red over a running turn. The state
  value is the existing `blocked`, so the registry contract and the window do not
  change. It never touches `question`, `waiting`, `done`, `note` or `floor`.
  `dialog "<text>"` sets it and `resumed` clears it; both are events, so a second agent
  or a session with no row writes nothing.
- Set on a Codex row by its `PermissionRequest` hook, `codex-status-blocked.sh`, which
  sends `dialog "codex permission request: <tool>"` and prints no decision, so the
  dialog shows as before.
- Set and cleared on a Claude row by the daemon, from the row's screen (the dialog
  tick, `rust/backend/src/comm/wake/dialog.rs`). Every 5 s it reads each Ready Claude
  capsule row whose entry reads `working` or carries a `dialog` fact to clear, one
  watcher attach each. A row at rest is skipped, on the premise that it shows no
  permission dialog (a limit, below), and one red by its question, an AskUserQuestion
  menu, keeps that summary. `working` is the entry, not the screen: see L0 for the
  row a turn left `working` with no Stop. A watcher attach takes no pen, sends no resize, and
  writes no record frame, no line to the daemon's stderr or tracing (its `sotd.log`) and
  none to the capsule's stderr: a test over twenty capsules with full scrollback shows
  it, and measured one sweep of them at 1.2 to 2.0 s, about 80 ms a read (slowest
  0.33 s), in a debug build on Linux. The figure is load dependent: the same twenty
  rows took 3.2 to 4.1 s, about 180 ms a read, on a box at load average 42 on 32 cores. A sweep is awaited before the next tick, and the
  ticks it missed are skipped, not queued, so a sweep past 5 s never stacks another.
  - `dialog_of` (the wake's `screen.rs`) says a dialog shows only when no main input
    box is on screen and one of the last 30 rows is the dialog's footer: a line that
    starts with "Enter to select" or "Esc to cancel" and a middle dot joining the next
    key hint, as real dialogs draw it (`Esc to cancel · Tab to amend`,
    `Enter to select · Tab/Arrow keys to navigate · Esc to cancel`). Prose that quotes
    the words never starts a line with them and the dot, "Do you want to" alone
    identifies nothing, so a reply that quotes a dialog never reads red, with or without
    an input box below it. The open dialog is keyed by its row, not its text: a menu's
    tab advancing, or auto mode's deny countdown, writes nothing new.
  - The tick writes only where the screen and the registry disagree: `dialog` when a
    dialog shows and the row has no fact, `resumed` when none shows and it has one.
  - It writes through `comm-status.sh` itself, with the entry `SOT_COMM_ROW_HANDLE`,
    the one stamp made from outside the row. The agent gate refuses a process that
    names a row it does not run inside, so the daemon cannot stamp as the row; this
    entry is the exception to "a process outside the row stamps as nobody". A
    process that sets it can paint or clear one row's `dialog` and nothing else: the
    entry takes `dialog` and `resumed` only (any other verb exits 2), derives no
    identity, and writes or removes no marker; the reduction then recomputes that
    row's `state` and `summary` and refreshes its stamp times, as every write does.
    The script honors the entry only for a caller whose parent is sotd (any other
    caller exits 2 with nothing written), and its `dialog` lands only on a row that
    still has a `floor`, so a Stop that came after the daemon read the screen leaves
    the row at rest unwritten.
    The reduction stays in one jq program, and no Claude heartbeat clears the
    daemon's fact.
  - It keeps nothing between ticks, so after a restart its first tick clears a fact
    whose dialog has gone. A row whose leg the probe shows ended (never started, ended
    with no respawn, terminal) gets `resumed`; one that does not answer keeps its fact.
- Cleared by every event that proves the dialog is gone: on a Codex row, the call's
  `PostToolUse` (the heartbeat finds `state/dialog-<key>.marker`, keyed by `SOT_WORKSPACE_ID` or the
  session id, with one `[ -e ]` per tool return, and sends `resumed`); on any row, the
  turn's `stop` and every `prompt` (typed, machine, a join). So `dialog` never outlives its
  turn. `comm-status.sh` owns the marker with the fact: the `dialog` write creates it,
  and a `resumed`, `prompt` or `stop` write removes it, each only after it commits. A
  failed write leaves the marker as the fact is, so a failed `resumed` is retried by
  the next tool return.
- Withheld: the Claude hook. An earlier build of this amendment wired Claude's
  `PermissionRequest` to a dialog hook and `PostToolUseFailure` to the heartbeat;
  neither ships, and an install strips both (`_remove_stale_comm_hooks!`). Claude's
  `PermissionRequest` fires when a request is queued, not when its dialog shows
  (measured 2026-09-28 on Claude Code 2.1.284: two helpers' requests fired 154 ms
  apart, and the second dialog showed 62 s after its hook fired). So queued requests,
  and any other agent's tool return in the same session, would set and clear one
  row-wide fact out of step with the screen. The same wiring was unregistered once
  before for that reason (commit e820cacc3), and the payload carries no `tool_use_id`
  to key a call's own clearing by. Keying it per agent (a helper's request carries
  `agent_id`) and measuring it under auto mode is a 0.6.7 design.
- Limits. (L0) On Windows the daemon does not run the dialog tick (it has no route
  to Git Bash there), so a Claude row waiting on a permission dialog reads green while
  its turn runs, then wilts (§5). Elsewhere a Claude row's red lags its dialog by up to
  one tick plus a read. A `prompt` that clears the fact while the dialog still shows is
  written again on the next tick, because the row is `working`; a `stop` that clears it is
  not, because the row is then at rest and is not read. Any screen of a working Claude row that
  replaces the input box and draws such a footer reads red while it shows. The footers
  are those of two real captures, each a test fixture: a permission dialog's
  (`Esc to cancel · Tab to amend`, with auto mode's deny countdown) and a question
  menu's. A turn that ends with no Stop (Esc, a bare "No", a reauth, an API error)
  leaves the row's floor, so the row reads `working` at rest and, while a person has a
  select menu open there (/model, /resume, a rewind), reads red until the menu closes;
  telling a row at rest from a working one takes the wake's 1.5 s hold, which lives
  inside its typing attempt, per row per tick, and that hold reads a working row whose
  screen holds still as free, which here would end a running turn's floor. The same
  silent exit, with no dialog, reads green and is not cleared by this tick: a 0.6.7
  limit. A row at rest is assumed to show no permission dialog; a background agent's
  dialog that shows after the lead's Stop reads idle or done, a missed red as before
  this amendment. (L1) Codex fires
  no hook at approval, so an approved call reads red until it returns. (L2) The return
  of another call in the same Codex session clears the red early. (L3) On a Codex row
  a denial fires no `PostToolUse`, and "No" or Esc
  that interrupts the turn fires no Stop: the row stays red until the turn's Stop or
  the next prompt. (L4) Another `PermissionRequest` hook that answers the request (a
  user's or a managed policy's) still leaves this stamp: allowed, red until the call
  returns; denied, as L3.
