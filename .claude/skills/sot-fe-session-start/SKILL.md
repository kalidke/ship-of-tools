---
name: sot-fe-session-start
description: Bootstrap a (re)started Ship of Tools FE session — re-arm the fast-comm wake (fe-inbox Monitor) and catch up on anything missed during the relaunch down-window. Fires automatically as the first turn after an ADR-0017 FE relaunch (via the [terminal] resume_command, i.e. claude --continue "/sot-fe-session-start"); also runnable manually. Activates for "session start", "bootstrap session", "rearm comm", "resume bootstrap".
---

# sot-fe-session-start

The first turn after an ADR-0017 FE relaunch. The resumed `claude --continue` is
**reactive and deaf**: harness Monitors do NOT survive a relaunch (the session
ended; `--continue` restores the transcript, not live background tasks), so
nothing wakes the session on inbound fast-comm until a Monitor is re-armed *on a
turn*. This skill is that turn — keep its steps current as the setup evolves
(that's the point of it being a skill rather than a hardcoded resume command).

## Multi-frontend reality (PR #7, multi-frontend-awareness on main)

A user roams across SEVERAL Windows FEs against ONE backend. Two facts shape comm:

- **The daemon broadcasts every `agent.message` to ALL connected FEs.** The `to`
  field is an **advisory label, not enforced routing** — a `to:backend-dev` message
  still lands in every FE's inbox. So any scheme is "broadcast to all, filter
  locally."
- **Broadcast demotion:** frames with `to:""` (true broadcast) FILE in every
  inbox but should NOT wake an upgraded Monitor — they're FYIs, not interrupts.
  Direct frames (`to:<myhandle>` or `to:win-fe` family) wake; broadcasts are read
  later. Anything broadcast that actually matters is also dropped on the durable
  git-bus, so `/bus-sync` is the catch-up.
- **Per-host FE handles: `win-fe-<host>`** (e.g. `win-fe-devbox-2022`), derived
  from `hostname` lowercased. A shared `win-fe` would make each machine's
  echo-filter swallow its siblings' traffic and make targeted pings impossible.
  The bare `win-fe` survives as a **family label** meaning "any FE" (the BE's
  receive-path check pings it; every `win-fe-<host>` Monitor wakes on it).

## Steps

1. **Re-arm the fast-comm wake** — derive this machine's handle, then arm a
   persistent harness **Monitor** on the FE inbox. The filter must (i) drop this
   session's OWN echoes only, and (ii) wake on lines addressed to **my handle** or
   the **`win-fe` family label** — NOT lines aimed at another handle (a sibling
   FE, or `backend-dev`), and **NOT broadcasts** (`to:""`). Broadcasts are *demoted*:
   they FILE in the inbox but don't wake (noise reduction — important broadcasts
   go on the durable bus, read on `/bus-sync`):

   ```bash
   HANDLE="win-fe-$(hostname | tr 'A-Z' 'a-z')"   # e.g. win-fe-devbox-2022
   tail -n0 -F "/c/Users/<you>/AppData/Local/sot/fe-inbox.jsonl" \
     | grep --line-buffered -v "\"from\":\"$HANDLE\"" \
     | grep --line-buffered -E "\"to\":\"($HANDLE|win-fe)\""
   ```

   (The trailing `"` in the `-E` pattern anchors each alternative, so `win-fe`
   matches the bare family label but NOT `win-fe-<otherhost>`; no empty alternative
   means broadcast `to:""` files without waking.) Use the **Monitor** tool with
   `persistent: true`. A plain background `tail` does NOT wake you — only a
   Monitor turns each new inbox line into an event.

2. **Ping the BE** — prove the daemon is *dispatching* AND the FE is *writing the
   inbox* (the exact substrate the Monitor depends on), not merely that the FE
   process is up. There is no dedicated ping op; instead round-trip a
   self-addressed `agent.send` and confirm the nonce lands back in
   `fe-inbox.jsonl`. The path it exercises is the whole wake chain:
   relay → daemon socket → broadcast → FE → inbox write.

   ```bash
   HANDLE="win-fe-$(hostname | tr 'A-Z' 'a-z')"
   NONCE="be-ping-$$-$RANDOM"
   export SOT_RELAY_ENDPOINT=tcp:127.0.0.1:18743   # match the FE's --tcp (or unix:<sock>) endpoint
   INBOX="/c/Users/<you>/AppData/Local/sot/fe-inbox.jsonl"
   ~/.sot-comm/bin/comm-relay.sh send "@$HANDLE" "BE liveness ping $NONCE"
   for i in $(seq 1 10); do
     grep -q "$NONCE" "$INBOX" && { echo "BE OK — round-trip:"; grep "$NONCE" "$INBOX"; break; }
     sleep 0.5
   done
   ```

   Read the result:
   - `relayed -> win-fe-<host> via tcp:...` = the daemon socket is alive and acked the send.
   - Nonce reappears in `fe-inbox.jsonl` (with a daemon-stamped `ts`) = daemon
     broadcast it and the FE wrote it → **entire wake path is live**.
   - Ack but **no** inbox line = daemon alive but the FE isn't writing the inbox
     (wake is broken — investigate the FE↔daemon connection / relaunch the FE).
   - **No** `relayed` line = daemon or the SSH-forwarded tunnel is down.

   Self-addressed (`@$HANDLE`) on purpose: it drops a `to:<myhandle>` message (no
   cross-machine noise) and the Monitor's `from:<myhandle>` filter means the ping
   won't spuriously self-wake you — so verify by reading the inbox file directly,
   not by waiting for a Monitor event. On Windows the relay needs
   `SOT_RELAY_ENDPOINT` set explicitly (git-bash has no `sotd`
   process to `pgrep`); use the same endpoint the FE connects to.

3. **Catch the deaf-window gap** — read the tail of `fe-inbox.jsonl` (lines where
   `from` != your handle) and surface anything new. The daemon replays missed
   `agent.message`s into the inbox on FE reconnect, so messages sent while you
   were down are usually still there — you only missed the live wake.

4. **`/bus-sync`** — the durable git-bus fallback for anything the relay didn't
   replay.

5. If sot-comm isn't installed/joined on this machine yet, do that first
   (see the `feedback_fast_comm_on_start` memory): install if `~/.sot-comm/bin`
   is absent (`julia --project=. -e 'using ShipTools; ShipTools.update_comm()'`), join
   with the **per-host handle** (`comm-join.sh --name "win-fe-$(hostname | tr 'A-Z' 'a-z')"`),
   then run the steps above (the BE ping needs the joined handle so `agent.send`
   stamps `from:win-fe-<host>`).

## Why a skill (not a hardcoded resume prompt)

The FE's ADR-0017 resume command calls `/sot-fe-session-start` so this runs
automatically on every relaunch. Keeping the actions here means we iterate on
the bootstrap in one editable place instead of in the FE/launcher config.
