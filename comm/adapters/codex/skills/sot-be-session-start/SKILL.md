---
name: sot-be-session-start
description: Bootstrap or repair a Ship of Tools backend Codex session by running sot-session-start's comm-session-start.sh, then ping the frontend. Use for BE session start, backend comm repair, or after restarting a backend Codex pane.
---

# sot-be-session-start

Run `sot-session-start` first (its two-phase arm/catch-up flow) —
`comm-session-start.sh --catch-up` already runs the sot layer (FE ping,
`bus.sh sync` peek) whenever it detects this repo, so there is nothing
sot-specific left to do here beyond that.

## Frontends

A frontend is a client of its daemon, never a comm peer: it has no
registry row and no handle to `ask`. Drive it with `sot-fe` (`--fe <host>`
scopes a command to the frontend on one host).

## Report State

Best-effort report: the verdict line's `selftest`/`bus` fields, plus
anything `comm-poll.sh` surfaced, and whether you are blocked, waiting, or
ready.
