# Work-state — mechanics, fixtures, and the turn-end auditor

Read this when the one-paragraph summary in `SKILL.md` isn't enough — e.g.
you're testing the state machinery itself, or a `waiting` row isn't clearing
the way you expect.

## Sticky waiting — the actual mechanism

`comm-status.sh waiting "..."` writes a sticky marker. Session hooks then
behave differently while it's live:

- `UserPromptSubmit` writes a *soft* `working` (you show green while actively
  processing — true in the moment).
- `Stop` writes a *soft* `done` (the turn-end floor, below), but the sticky
  marker **demotes you straight back to purple**, restoring your `waiting`
  summary. You do NOT need to re-assert `waiting` at every turn end — it
  survives intervening turns on its own.
- An explicit `blocked` also preserves the marker underneath it (precedence:
  blocked > waiting > idle); answering the question drops you back to
  purple, not green.

The marker clears two ways:
1. **You explicitly report** `working` / `idle` / `done` (i.e. you ran
   `comm-status.sh` yourself, not a hook) when the job actually lands — this
   is the accurate signal and the one to prefer.
2. **Self-heal**: a marker older than **2h** is dropped by the next turn-end,
   so a forgotten purple can't lie forever. Re-assert `waiting` yourself for
   a genuinely longer job.

## Blue / gray — the turn-end floor (owner decision 2026-09-08)

Blue and gray are an **unread / read** pair, stamped by hooks, not by you:

- The `Stop` hook floors every turn end with a *soft* `done`. `comm-status.sh`
  turns it **blue** only when the row was `working` from a **genuine human
  prompt** (`turn_origin == user`, written by the `UserPromptSubmit` hook's soft
  `working`): "this session finished a turn you asked for and you have not
  been back since".
- A turn a **machine** started — a relay message, a Monitor event, a task
  notification — floors to **gray** `idle`, whatever it did: a peer's ack must
  not paint a parked row blue. If such a turn landed a real result, report
  `comm-status.sh done "<summary>"` yourself — that explicit blue is the
  accurate signal and the skill rule already asks for it.
- Blue clears on the user's **next genuine prompt** (→ green) or any explicit
  report. There is **no time-based decay**: a parked blue row is an honest
  "you never came back", not a bug. A `blocked`/`waiting`/explicit `done` row
  is never touched by the floor (same guards as the old soft idle).

### Viewing clears blue (owner decision 2026-09-08)

Blue is *unread*, and there are two ways to read a session: type into it, or
look at it. Only the first cleared blue, so rows the user read and moved on
from stayed blue forever.

**Switching the frontend's view to a workspace clears that row's blue.** The
frontend already tells the daemon "my view is now this workspace"
(`workspace.activate`); the switches a **person** performs — Sessions-Enter,
Shift+Left/Right cycling — now carry `read: true` on that same signal. The
daemon flips a `done` row to `idle` and **writes nothing else**: the summary
survives (the row reads `idle · last: …`), and `status_at` is untouched, so
reading a parked row does not make it look recently active.

**Both blues clear.** The floor's blue and an explicit `comm-status.sh done
"<summary>"` mean the same thing — a result you have not seen — and the
registry does not record which writer stamped it. Telling them apart would
mean a new field that serves no other invariant. One rule: `done` → `idle`.
`blocked`, `waiting` and `working` are never touched — viewing is not
answering, and it is not finishing a job.

**A 10 s dwell (owner decision 2026-09-08 evening).** A switch alone does
not count: the frontend arms a mark when a person switches to a row and sends
`read: true` only if that same view is still up ten seconds later. Any other
switch drops the mark, so a blow-through while cycling never reads a row. The
daemon side is unchanged — the flag arrives on the same `activate` it always
did, just later.

**Not the user's every arrival.** An agent driving the view (`sot-fe switch`,
a cross-workspace `show-result`), a `workspace.create` auto-switch, a destroy
bounce and a reconnect re-announce all send `activate` with `read: false`.
The daemon never infers that a person looked.

*No time-based decay still stands.* Only a read clears blue.

`turn_origin` is a registry field on the row, written only by the soft
`working` write; nothing else reads it. Absent provenance fails gray, so a box
still on the old hooks never paints blue by accident.

## Closing markers — the turn-end word (2026-09-09)

A reply whose closing block opens a line with `SITREP:`, `SITREP-QUESTION:`
or `SITREP-WAITING:` declares the turn's end state (done / blocked /
waiting) and carries the report that state demands (the `sitrep` skill
holds the three shapes). The `Stop` hook stamps that state **explicitly** —
`waiting` sets the sticky marker, `done` paints blue — with the rest of the
marker line as the row summary (the next non-empty line when the marker
stands alone). A marker ends the hook: no floor, no auditor, no nudge.

Without a marker, a **human** turn that ends with a `blocked` / `waiting` /
`done` row gets one Stop nudge naming the shape it owes; the continuation's
marker then stamps the row. A machine wake (relay, Monitor, notification)
never nudges — a peer's ack on a parked row is not a report — and a plain
answer with no explicit state floors exactly as before.

**Effort vs exchange (2026-09-10).** The parked-row nudge only ever reached a
session that had already stamped itself; the sessions that never stamp ended
every turn green and were never reminded. So a **human** turn that was an
effort — at least 8 tool calls, or 5 minutes of wall time since the prompt,
measured from the transcript — and ends green with no marker gets one SOFT
nudge: close with `SITREP:` if the turn closed an effort, end normally if it
was a step in a live back-and-forth (the owner's ruling: no formal block
during an exchange). Short turns never trip it, whatever they did.

**No nudge on a turn that has its block.** A plain-language lint that sent a
block carrying identifiers back to be rewritten lasted one afternoon: a Stop
send-back can only append a continuation, so the rewrite landed as a second,
different report under the first. The marker stamps whatever the block says;
the language rules are the skill's, not the hook's. The marker line tolerates
a heading prefix and bold that closes before or after the colon.

## Testing the state machinery — never against your live row

Registering fixture states on your own handle paints real colors on the
user's session strip — a stale test row reads as a real stuck agent. Join a
**scratch handle** for fixtures, and mutate the registry ONLY through
`comm-status.sh`. A raw `jq ... > tmp && mv` skips the registry lock and can
lose writes racing against other hook writers on shared filesystems.

## The turn-end auditor

The `Stop` hook runs a tiered auditor (`comm/core/scripts/comm-turn-auditor.sh`):
cheap deterministic filters first, then **one** conservative `claude -p`
Haiku judgment only when a filter trips. It checks the ending turn for three
misses:

- a real turn-ending question without `blocked` set,
- a user-facing artifact (plot/PDF/screenshot) never surfaced via
  `show-result`,
- a background job armed without `waiting` set.

A confirmed finding comes back as a Stop nudge ("Turn-end audit: …"); you
still gate on it — act on real findings, end the turn normally on false
ones. The same finding won't re-fire within 30 minutes.

Kill switch: `SOT_TURN_AUDITOR=0` (env) or `touch ~/.sot-comm/auditor.off`
(falls back to the legacy `?`-grep nudge).
