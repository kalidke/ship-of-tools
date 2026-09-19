# Work-state — mechanics, fixtures, and the turn-end auditor

Read this when the one-paragraph summary in `SKILL.md` isn't enough — e.g.
you're testing the state machinery itself, or a `waiting` row isn't clearing
the way you expect.

## Precedence: the user first

When a turn both needs the user (a go-ahead, a pasted file, a decision) and has
jobs running, the row is `blocked` and the closing marker is `SITREP-QUESTION:`
with the question first and the running jobs listed after it. `waiting` is only
for a turn where nothing needs the user; a purple row that is really waiting on
the owner is a question they never see (owner, 2026-09-19).

## The facts, and the reduction that colours them (ADR 0044 amendment, 2026-09-19)

A row is a small SET OF FACTS, not one state you set directly:

| field | value | set by | cleared by |
|---|---|---|---|
| `floor` | `user` \| `machine` | the `prompt` event, to the origin | the `stop` event |
| `question` | the question text | `blocked "<q>"` | a `prompt` event with origin `user`; explicit `working`/`idle`/`done` |
| `waiting` | the wait summary | `waiting "<s>"` | explicit `working`/`idle`/`done` |
| `done` | `true` | explicit `done`; `stop` (when `floor` was `user` and nothing else is pending) | a `prompt` event with origin `user`; explicit `working`/`idle`/`blocked`/`waiting`; viewing the row |
| `note` | the declaration's own line | any declaration with a summary | a declaration with `""` |

Every write (event or declaration) re-reduces `state` and `summary` from
whatever facts remain, in ONE place, `comm-status.sh`:

```
question set and floor absent  -> blocked   summary = question
floor present                  -> working   summary = note
waiting set                    -> waiting   summary = waiting
done set                       -> done      summary = note
otherwise                      -> idle      summary = note
```

Green (`floor`) and red (`question`) both outrank purple (`waiting`)
*structurally* — a running or answering turn is never held behind a wait —
so "the user comes first" and "a resumed turn is green with jobs in flight"
need no special-case hold logic. `waiting` is cleared only by an explicit
report, never by a timer: a forgotten purple is the session's own word to
retract, the same "no aging" rule blue already follows.

`blocked "<q>"` KEEPS an existing `waiting`: both facts can be true, and
red simply outranks purple in the display until the question is answered,
at which point the wait (if still real) shows again. `waiting "<s>"`
likewise keeps `question`. `working`/`idle`/`done` all clear both.

`AskUserQuestion` (Claude) and the permission prompt (Codex) are the
session yielding to the owner while the harness pauses: their PreToolUse
hooks send `blocked` then `stop` — a real turn end, not a hold. The
answer arrives as that tool's PostToolUse, which sends `prompt` with
origin `user`, exactly like a fresh turn start.

### Viewing clears the `done` fact and the badge, never `question` or `waiting`

`done` is *unread*, and there are two ways to read a session: type into it,
or look at it. Only the first cleared it, so rows the user read and moved
on from stayed blue forever.

**Switching the frontend's view to a workspace clears that row's `done`.**
The frontend already tells the daemon "my view is now this workspace"
(`workspace.activate`); the switches a **person** performs — Sessions-Enter,
Shift+Left/Right cycling — now carry `read: true` on that same signal. The
daemon removes the `done` fact and, ONLY when `state` was `"done"` (the fact
was the display), flips `state` to `"idle"` too — a `done` fact sitting
under a running `floor` or a `waiting` is still unviewed, and removing it
leaves the reduction consistent because nothing below blue exists. The
summary survives (the row reads `idle · last: …`), and `status_at` is
untouched, so reading a parked row does not make it look recently active.

**Both blues clear the same way.** The floor's `done` and an explicit
`comm-status.sh done "<summary>"` mean the same thing — a result you have
not seen — and the registry does not record which writer set it. `question`,
`waiting` and `floor` are never touched — viewing is not answering, and it
is not finishing a job.

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

*No time-based decay still stands.* Only a read clears the `done` fact.

## Closing markers — the turn-end word (2026-09-09)

A reply whose closing block opens a line with `SITREP:`, `SITREP-QUESTION:`
or `SITREP-WAITING:` declares the turn's end state (done / blocked /
waiting) and carries the report that state demands (the `sitrep` skill
holds the three shapes). The `Stop` hook stamps that state **explicitly** —
the matching declaration sets the fact — with the rest of the marker line
as the row summary (the next non-empty line when the marker stands alone).
A marker is followed by `stop` as normal, so the fact it just set survives:
no nudge, no auditor.

Without a marker, a **human** turn that ends `blocked` / `done` gets one
Stop nudge naming the shape it owes; the continuation's marker then stamps
the row. A `waiting` fact is NEVER nudged here — it is not this turn's word,
and a wait carried over from an earlier turn would otherwise nudge every
short exchange on the row; a turn that IS newly waiting still declares
`SITREP-WAITING:` and the marker sets it. A machine wake (relay, Monitor,
notification) never nudges — a peer's ack on a parked row is not a report —
and a plain answer with no explicit state floors exactly as before.

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
