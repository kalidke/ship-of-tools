---
name: sot-session-start
description: Bootstrap a (re)started Claude session onto sot-comm fast-comm in two phases: join+listen, arm the Monitor, then catch up+selftest. Generic; manual or on --continue. Activates for "comm session start", "comm bootstrap", "rearm comm".
---

# sot-session-start

A (re)started session is deaf: `--continue` kills your harness Monitor, and
the cross-machine relay has no server-side queue. Two calls to one script,
so no message can land before your Monitor exists to catch it.

**Phase 1 — arm:**

```bash
~/.sot-comm/bin/comm-session-start.sh
```

Either nothing to do (`SURVIVED handle=<h>` + a context block — stop here),
or:

```
BOOTSTRAP-ARM handle=<h> listener=up|down|n/a identity=ok|MISMATCH|FAIL MONITOR: <cmd>
```

- `identity=FAIL` with a `REFUSED:` line — the identity slot already names
  someone else's project. Pin `SOT_COMM_NAME` (and, for a subagent/lane, a
  private `SOT_COMM_SELF_FILE`) and re-run; never work around this by hand.
- Otherwise, **arm a persistent harness Monitor** running exactly the
  printed `MONITOR:` command — the one act this script can't do for you —
  then run phase 2.

**Phase 2 — catch up** (only once the Monitor from phase 1 is armed):

```bash
~/.sot-comm/bin/comm-session-start.sh --catch-up
```

```
BOOTSTRAP handle=<h> poll=<n>|ERR selftest=ok|retry|down bus=<n>|n/a identity=ok
```

- `selftest=retry` — cold-start, still connecting; re-run
  `~/.sot-comm/bin/comm-listen.sh --selftest` once, a few seconds later.
- `bus=<n>` is a PEEK, not an acknowledgement — run `bus.sh sync` (or
  `/bus-sync`) to actually see and consume those entries.

The real proof your Monitor works is its own notification —
`[relay] from __selftest__: …` — not the inline selftest text.

**Identity**: a pin (`SOT_COMM_NAME`, or a private `SOT_COMM_SELF_FILE`)
always wins; otherwise a validated prior identity; otherwise fresh
derivation — never manufactured from a lower-priority source. A
subagent/lane that doesn't own its ambient identity slot MUST pin both.
`identity=MISMATCH` and a `REFUSED` start are different problems with
different fixes — see `references/reclaim-handle.md`.

**Work-state (the nav row colour) is yours to stamp** — the script prints the
rule after every outcome. `comm-status.sh waiting "<what>"` (purple) the moment
you launch a background job, subagent, Codex run or hand work to a peer; it is
sticky across turns until you stamp `working`/`idle`/`done` when the job lands.
A background job never makes you idle; `blocked` (red) only when the user must
act. Mechanics: the sot-comm skill's `references/work-state.md`.

A Ship of Tools checkout gets the sot-specific layer (FE ping, bus count)
folded into phase 2 for free — no separate skill. `ccb`/`ccbe` both launch
this skill.
