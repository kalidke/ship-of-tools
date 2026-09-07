---
name: sot-be-session-start
description: Bootstrap or repair a Ship of Tools backend Codex session by running sot-session-start's comm-session-start.sh, then ping the frontend. Use for BE session start, backend comm repair, or after restarting a backend Codex pane.
---

# sot-be-session-start

Run `sot-session-start` first (its two-phase arm/catch-up flow) —
`comm-session-start.sh --catch-up` already runs the sot layer (FE ping,
`bus.sh sync` peek) whenever it detects this repo, so there is nothing
sot-specific left to do here beyond that.

## Frontend Ping

Attached FEs receive daemon relay broadcasts even with no `win-fe-*` row in
this backend host's registry. The bootstrap already sends one `@win-fe`
advisory ping; a later FE reply arrives as a directed `[relay] from
win-fe-<host>:` line if your Codex wake path is armed. Do not block on `ask`
during bootstrap.

## Report State

Best-effort report: the verdict line's `selftest`/`bus` fields, plus
anything `comm-poll.sh` surfaced, and whether you are blocked, waiting, or
ready.
