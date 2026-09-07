# Work-state — mechanics, fixtures, and the turn-end auditor

Read this when the one-paragraph summary in `SKILL.md` isn't enough — e.g.
you're testing the state machinery itself, or a `waiting` row isn't clearing
the way you expect.

## Sticky waiting — the actual mechanism

`comm-status.sh waiting "..."` writes a sticky marker. Session hooks then
behave differently while it's live:

- `UserPromptSubmit` writes a *soft* `working` (you show green while actively
  processing — true in the moment).
- `Stop` writes a *soft* `idle`, but the sticky marker **demotes you straight
  back to purple**, restoring your `waiting` summary. You do NOT need to
  re-assert `waiting` at every turn end — it survives intervening turns on
  its own.
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
