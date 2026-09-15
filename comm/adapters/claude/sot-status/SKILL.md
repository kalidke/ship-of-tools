---
name: sot-status
description: One command for the whole Ship of Tools system — every host, its daemon, its rows, its attached clients. Use for "system state", "sot status", "host info", "show me the hosts", "which frontends", "what is attached".
---

# sot-status — the whole system, one command

```bash
sotd status
```

Declared (from `hosts.toml`, the one list) fused with LIVE (asked of every
reachable daemon right now): HOST, DECLARED (hub/daemon/frontend/shell/
sampled/monitor-only), DAEMON (build + up, or unreachable WITH the reason —
a declared host is never just missing from the table), ROWS (count by
phase), CLIENTS (each attached client as `role@host`, the active frontend
marked `ACTIVE`), then a final line naming the hub's own active frontend.
`--json` for a script.

**Read the table — never a log, never a systemd unit state.** A session
being "live on the relay" is derived from the hub's own client roster
(a `bridge`-role connection declaring that host), which is exactly what
`sotd status` already did for you; a unit can be green while the thing
behind it is dead.

`sotd topology status` is a DIFFERENT, narrower command — declared only, no
network I/O, safe to run from a shell profile. Reach for `sotd status` for
"what's actually happening"; reach for `topology status` only when you want
the raw declared list with nothing asked.

`sot-fe version` still answers the single-daemon question ("what build is
THIS box's daemon, who's attached to it") — see the `sot-comm` skill.
