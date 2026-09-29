# Reclaiming a handle after `identity=MISMATCH`, or unblocking a `REFUSED`

`comm-session-start.sh` prints two different signals for two different
problems — read which one you actually have before picking a recipe.

- **`identity=MISMATCH`** — it joined you anyway, but under an ESCALATED
  handle (e.g. `<repo>-<parentdir>-<host>` instead of the bare
  `<repo>-<host>`) because your CANONICAL handle was already held by another
  row at join time. Recipe below.
- **`identity=FAIL` with a `REFUSED:` line** — it did NOT join at all: the
  self-file at this identity slot already names a different, validated
  project, and no `$SOT_COMM_NAME`/`$SOT_COMM_SELF_FILE` pin was given to
  say what to do about it. See "Unblocking a REFUSED start" below — this is
  a DIFFERENT situation, not a smaller version of the same one.

## MISMATCH recipe

Another row already holding YOUR canonical handle is almost always your own
earlier identity — a real collision with a different project looks identical
from the outside, so verify before reclaiming. **The no-arg `comm-join.sh` is
the WRONG move here.** No-args derives a handle from scratch; derivation sees
your own canonical handle's row as "held by an unknown project" and
escalates away from it again — which is how you got here. Reclaim
explicitly instead.

1. **Prove sole ownership of the canonical handle before reclaiming it.**
   Confirm exactly one live session has this repo as its cwd — `comm-list.sh`
   shows which handle is live and when the row holding it last heard from
   the daemon. If you can't confirm sole ownership, stop and ask a human —
   reclaiming someone else's live handle strands *them* instead of fixing
   you.

2. Drop the escalated handle:
   ```bash
   ~/.sot-comm/bin/comm-leave.sh --name <escalated-handle>
   ```

3. Reclaim the canonical handle **explicitly** (never bare — bare derivation
   is exactly what stranded you):
   ```bash
   ~/.sot-comm/bin/comm-join.sh --name <canonical-handle>
   ```

That's it — the daemon now has your canonical handle attached to this row.
There is nothing else to move, arm or re-arm.

### Why MISMATCH is rare

`comm-context.sh` self-heals a legacy (pre-root=) self-file on read instead
of discarding it, and `comm-session-start.sh` never manufactures an explicit
`--name` — it only ever does a BARE `comm-join.sh` call and lets that
script's own precedence (an explicit pin, then a validated self-file, then
fresh derivation) decide, exactly as a human would. `identity=MISMATCH`
means comm-join.sh's OWN escalation-warning fired during that bare join —
you land here only if a genuinely different project shares this repo's
basename+host, or a stale row survived long enough to look like a real
collision.

## Unblocking a `REFUSED` start

This means the identity slot `comm-session-start.sh` would have joined into
(the self-file at `$SOT_COMM_SELF_FILE`, or the ambient pane-keyed one) is
currently validated for a **different project** — mutating it (even via an
ordinary bare join) would silently steal that slot from whoever legitimately
holds it. This is not a rare edge case for a **subagent or lane session**: if
you did not launch with your own `$SOT_COMM_NAME` (a distinct handle) and,
ideally, your own private `$SOT_COMM_SELF_FILE` (a slot nobody else reads or
writes), you may be inheriting an ambient identity slot — e.g. a pane shared
with the session that spawned you — that genuinely belongs to someone else
right now. (This is exactly how a coordinator session's own identity was
clobbered by an unpinned subagent during this feature's own development —
see the PR's implementation report.)

**The fix is at the LAUNCHER, not here**: re-run with an explicit pin —

```bash
SOT_COMM_NAME=<a-distinct-handle> ~/.sot-comm/bin/comm-session-start.sh
```

— or, for a lane/subagent that should never share the parent's slot at all,
also pin a private self-file so nothing it does can ever touch the parent's:

```bash
SOT_COMM_NAME=<a-distinct-handle> SOT_COMM_SELF_FILE=<a-path-only-this-lane-uses> \
    ~/.sot-comm/bin/comm-session-start.sh
```

That private path is the right shape for a lane, which owns no row: the row a
session declares its handle into is read from the self-file's BASENAME
(`<host>__<workspace-id>.txt`), so a lane-only name declares nothing to the
daemon and arms no ping watcher — exactly what a lane should do, and what
stops it rewriting the parent row's handle. If you are the row's OWN session
and still need a slot of your own, keep that basename and put it in a private
directory instead; a different basename would leave your row naming whatever
handle last declared into it.

Do **not** "fix" a `REFUSED` by removing or hand-editing the self-file it
named — that file may be a live session's real identity record. Pin your own
name instead; that alone resolves the ambiguity without touching anyone
else's state.
