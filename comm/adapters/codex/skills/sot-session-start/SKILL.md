---
name: sot-session-start
description: Declare a backend Codex session's sot-comm handle and check its unread mail. Use after a restart or when Codex was not started via ccx.
---

# sot-session-start

`ccx` normally runs this bootstrap before Codex starts. Run it manually only
when the session was started without `ccx`, or was resumed in an existing
pane.

Set a Codex-specific handle first (kept distinct from a Claude backend's
`<repo>-<host>` on the same box, via `-cx-`), if the launcher did not — this
is a PIN (`$SOT_COMM_NAME`), which always wins over anything a self-file
might otherwise resolve:

```bash
if [ -z "${SOT_COMM_NAME:-}" ]; then
  repo="$(basename "$(git rev-parse --show-toplevel 2>/dev/null || pwd)")"
  host="$(hostname -s 2>/dev/null || hostname)"
  export SOT_COMM_NAME="${repo}-cx-${host}"
fi
~/.sot-comm/bin/comm-session-start.sh
```

It declares this session's handle to the daemon. `identity=FAIL` with a
`REFUSED:` line means the identity slot already names a different project —
re-run with a more specific `$SOT_COMM_NAME` (you already set one above; this
only fires if that name itself collides).

**What a session is told at start.** `comm-context.sh` prints your handle. Send
with `comm-send.sh @handle "text"` and read its one result. When
`[sot-comm] you have mail` appears, or your end-of-turn check says so, run
`comm-poll.sh`. To wait for a reply, end your turn. Run the session-start step
once, when a session first starts — not again on every resume.

This is ADR 0049's design of record, landing in stages: today the failure verdict
reads `no such handle: <h>` or `NOT CONFIRMED:` rather than `FAILED ->`, and the
line typed into a row reads `[sot-comm] new message for @<handle> — run
…/comm-poll.sh` rather than `[sot-comm] you have mail`. The call above still
prints a `MONITOR:` or `WAKE:` line ordering a Monitor armed and re-armed on
expiry — that line is the old mechanism, not this design, and is not to be
acted on: there is nothing to arm, own or re-arm. Mail is read with
`comm-poll.sh` regardless of what that line says.

Work-state (the nav row colour) is yours to stamp: `comm-status.sh waiting
"<what>"` (purple) the moment you launch a background job or hand work to a
peer — sticky until you stamp `working`/`idle`/`done` when it lands. A
background job never makes you idle; `blocked` (red) only when the user must
act. If any item needs the user while jobs also run, the turn ends `blocked`
with the question first; `waiting` only when nothing needs the user.
