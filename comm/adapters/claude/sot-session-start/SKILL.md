---
name: sot-session-start
description: Declare a session's sot-comm handle and check its unread mail. Run once, when a session first starts. Activates for "comm session start", "comm bootstrap".
---

# sot-session-start

Run once, when a session first starts:

```bash
~/.sot-comm/bin/comm-session-start.sh
```

It declares this session's handle to the daemon.

**What a session is told at start.** `comm-context.sh` prints your handle. Send
with `comm-send.sh @handle "text"` and read its one result. When
`[sot-comm] you have mail` appears, or your end-of-turn check says so, run
`comm-poll.sh`. To wait for a reply, end your turn. Run the session-start step
once, when a session first starts — not again on every resume.

This is ADR 0049's design of record, landing in stages: a send's verdict now is
`filed -> @h` or `FAILED -> @h: <reason>`, except that until B2 a handle the
hub's folder does not list can still get `NOT CONFIRMED: sent for @h; …` or
`filed -> @h (by <filer>, relay)`; the
line typed into a row reads `[sot-comm] new message for @<handle> — run
…/comm-poll.sh` rather than `[sot-comm] you have mail`, and a resumed session
must still re-run this bootstrap — until the daemon does the waking, the
launcher's `--continue` re-runs it for exactly that reason. Any
line the call above prints that orders a Monitor armed or a listener started
is the old mechanism, not this design, and is not to be acted on: there is
nothing to arm, own or re-arm. Mail is read with `comm-poll.sh` regardless of
what any of those lines say.

**Identity**: a pin (`SOT_COMM_NAME`, or a private `SOT_COMM_SELF_FILE`)
always wins; otherwise a validated prior identity; otherwise fresh
derivation — never manufactured from a lower-priority source. A
subagent/lane that doesn't own its ambient identity slot MUST pin both.
`identity=MISMATCH` and a `REFUSED` start (`identity=FAIL`) are different
problems with different fixes — see `references/reclaim-handle.md`.

**Work-state (the nav row colour) is yours to stamp** — the script prints the
rule after every outcome. `comm-status.sh waiting "<what>"` (purple) the moment
you launch a background job, subagent, Codex run or hand work to a peer; it is
sticky across turns until you stamp `working`/`idle`/`done` when the job lands.
A background job never makes you idle; `blocked` (red) only when the user must
act. Precedence when both hold: if ANY item needs the user (a go-ahead, a
file, a decision), the turn ends `blocked` with `SITREP-QUESTION:` and the
question first, running jobs listed after it; `waiting` is only for a turn
where nothing needs the user. Mechanics: the sot-comm skill's
`references/work-state.md`.

`ccb` launches this skill.
