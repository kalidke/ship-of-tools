# Reclaiming a handle after `identity=MISMATCH`, or unblocking a `REFUSED`

`comm-session-start.sh` prints two different signals for two different
problems — read which one you actually have before picking a recipe.

- **`identity=MISMATCH`** — it joined you anyway, but under an ESCALATED
  handle (e.g. `<repo>-<parentdir>-<host>` instead of the bare
  `<repo>-<host>`) because your CANONICAL handle was live under your uid at
  join time: its `last_seen` was under ten minutes old, stamped by a
  session's own activity or by the daemon running its row. Recipe below.
- **`identity=FAIL` with a `REFUSED:` line** — it did NOT join at all: the
  self-file at this identity slot already names a different, validated
  project. See "Unblocking a REFUSED start" below — this is a DIFFERENT
  situation, not a smaller version of the same one.

A session outside any row never takes or reads a handle a row holds: when its
self file names one, it steps past it to its own derived handle and says so
(`@<h> belongs to row ...`), which is not a FAIL.

This is ADR 0049's design of record: the daemon wakes the row, and there is no
Monitor or watcher to arm, move or stop.

## MISMATCH recipe

A live canonical handle is usually your own earlier identity, but a running
row of a different project with the same basename on this box looks
identical from the outside, so verify before reclaiming. No command names a
handle (decision 0031): the join derives, and it takes the canonical handle
again only once no other entry holds it.

1. **Prove sole ownership of the canonical handle before reclaiming it.**
   Confirm exactly one live session has this repo as its cwd and no other row
   on this box holds the canonical handle. `comm-list.sh` shows which handles
   are live, not who stamped them: an idle row's daemon stamps its handle every
   minute, so a live handle may be a running row rather than a session that
   just acted. If you can't confirm sole ownership, stop and ask a human —
   reclaiming someone else's live handle strands *them* instead of fixing
   you.

2. Drop the escalated handle and the stale canonical entry (only once step 1
   shows it is no live session's):
   ```bash
   ~/.sot-comm/bin/comm-leave.sh --name <escalated-handle>
   ~/.sot-comm/bin/comm-leave.sh --name <canonical-handle>
   ```

3. Join again, bare: with the canonical handle free, derivation takes it.
   ```bash
   ~/.sot-comm/bin/comm-join.sh
   ```

There is nothing armed to move: the daemon wakes the row by the handle the row
declares.

### Why MISMATCH is rare

`comm-context.sh` self-heals a legacy (pre-root=) self-file on read instead
of discarding it, and `comm-session-start.sh` only ever does a bare
`comm-join.sh` call, whose precedence (a validated self-file, then fresh
derivation) decides, exactly as a human would. `identity=MISMATCH`
means comm-join.sh's OWN escalation-warning fired during that bare join —
you land here only if a genuinely different project shares this repo's
basename+host, or a stale row survived long enough to look like a real
collision.

## Unblocking a `REFUSED` start

This means the identity slot `comm-session-start.sh` would have joined into
(the self-file at `$SOT_COMM_SELF_FILE`, or the ambient pane-keyed one) is
currently validated for a **different project** — mutating it (even via an
ordinary bare join) would silently steal that slot from whoever legitimately
holds it. A session outside any row meets this most: every no-workspace
shell on this host shares one derived slot (`<host>__nopane.txt`), so
another project's session may hold it right now. Only a session holds an
identity, a row's own session or a session outside any row; nothing started
inside a session (a subagent, `codex exec`, `claude -p`) joins, because it
uses no mail. (An unpinned subagent once clobbered a session's own
identity this way, during this feature's own development.)

**The fix is at the LAUNCHER, not here**: re-run with a private self-file, so
that nothing you do can touch the shared slot; the join then derives this
session's own handle into it:

```bash
SOT_COMM_SELF_FILE=<a-path-only-this-session-uses> ~/.sot-comm/bin/comm-session-start.sh
```

That private path is the right shape for a session outside any row: the row
a session declares its handle into is read from the self-file's BASENAME
(`<host>__<workspace-id>.txt`), so a private self-file is never named that
way, and declares nothing to the daemon. If you are a
row's OWN session and still need a slot of your own, keep that basename and
put it in a private directory instead; a different basename would leave your
row naming whatever handle last declared into it.

Do **not** "fix" a `REFUSED` by removing or hand-editing the self-file it
named — that file may be a live session's real identity record. A private
self-file of your own resolves the ambiguity without touching anyone else's
state.
