---
name: sot-session-start
description: Bootstrap or repair a backend Codex session via comm-session-start.sh's two-phase flow (arm, then catch-up) so codex-watch.sh exists before the selftest proves it. Use after a restart, manual tmux attach, or comm repair.
---

# sot-session-start

`ccx` normally runs this bootstrap before Codex starts. Run it manually only
when the session was started without `ccx`, was resumed in an existing pane,
or comms need repair.

## Phase 1 — arm

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

Either `SURVIVED handle=<h>` (nothing to do — your own `codex-watch.sh` is
recognized as a live watcher, ownership-checked against the registry; stop
here) or:

```
BOOTSTRAP-ARM handle=<h> listener=up|down|n/a identity=ok|MISMATCH|FAIL MONITOR: <cmd>
```

`identity=FAIL` with a `REFUSED:` line means the identity slot already names
a different project — re-run with a more specific `$SOT_COMM_NAME` (you
already set one above; this only fires if that name itself collides).
Otherwise, start the Codex wake helper for the printed handle NOW, before
anything else — the selftest in phase 2 needs it alive to prove the wake:

```bash
[ -n "${TMUX_PANE:-}" ] && nohup ~/.sot-comm/bin/codex-watch.sh "$SOT_COMM_NAME" "$TMUX_PANE" >/dev/null 2>&1 &
```

## Phase 2 — catch up

```bash
~/.sot-comm/bin/comm-session-start.sh --catch-up
```

```
BOOTSTRAP handle=<h> poll=<n>|ERR selftest=ok|retry|down bus=<n>|n/a identity=ok
```

`selftest=retry` means the bridge is still connecting on a cold start — wait
a few seconds and re-run `~/.sot-comm/bin/comm-listen.sh --selftest` once.
The real wake proof is a typed `[relay] from __selftest__:` line from
`codex-watch.sh`, not the inline selftest text. `bus=<n>` is a peek, not an
acknowledgement — run `bus.sh sync` for real to see and consume those
entries.
