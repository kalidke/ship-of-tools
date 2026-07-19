---
name: sot-fe-session-start
description: Bootstrap a (re)started Ship of Tools FE session — set the win-fe handle, point relay sends at the local SSH tunnel to the backend Unix socket, re-arm the fast-comm wake (fe-inbox Monitor), and catch up on anything missed during the down-window. Fires automatically as the first turn after an ADR-0017 FE relaunch (via the [terminal] resume_command, i.e. claude --continue "/sot-fe-session-start"); also runnable manually. Activates for "fe session start", "frontend session start", "session start", "bootstrap session", "rearm fe comm", "rearm comm", "fe bootstrap session", "resume bootstrap".
---

# sot-fe-session-start

The first turn after an ADR-0017 FE relaunch, and the manual bootstrap for any
frontend-side session. The resumed `claude --continue` is **reactive and deaf**:
harness Monitors do NOT survive a relaunch (the session ended; `--continue`
restores the transcript, not live background tasks), so nothing wakes the
session on inbound fast-comm until a Monitor is re-armed *on a turn*. This skill
is that turn — keep its steps current as the setup evolves (that's the point of
it being a skill rather than a hardcoded resume command).

This is not the same as `/sot-be-session-start`: the frontend machine usually
does not share the backend's `~/.sot-comm` registry, and it cannot open the
backend's Unix socket directly. The native frontend receives daemon relay
messages and appends them to its local `fe-inbox.jsonl`; this session tails that
file for wakes and sends outbound messages through the local SSH tunnel.

> **Canonical copy:** this file (repo `.claude/skills/`). The install payload
> `comm/adapters/claude/sot-fe-session-start/SKILL.md` is a byte-for-byte copy —
> edit HERE, then sync the payload and re-run `/sot-install` to close skew.

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

### 0. First — are you resuming from a COMPACTION, or a genuine relaunch/cold start?

This skill normally runs on an ADR-0017 relaunch (`--continue`), which **kills**
your harness Monitor — so re-arming (step 3) is right. But a **context compaction
does NOT kill it**: your fe-inbox Monitor is a background task that *survives*.
Re-running the full bootstrap on a merely-compacted session **double-arms** it
(every FE message then wakes you twice, compounding per compaction). So branch on
**why you're here** — the reliable signal, since the FE runs on Windows where
process inspection (`pgrep`/`ps`) is unavailable or unreliable, and a *false*
"survivor" match (an editor, a diagnostic `tail`, or another agent touching
`fe-inbox.jsonl`) would make a genuinely-deaf session skip arming and stay **deaf**:

- **You were just told your context was COMPACTED** (the post-compaction hook
  directive that sent you here) → your Monitor SURVIVED → **STOP: skip steps 1–4.**
  Re-reading this doc (and `/sot-comm`) has already restored your operating
  context — the whole point of re-running on compaction. Re-arming would only
  double every wake.
- **A fresh relaunch (`--continue`) or a cold start** — the normal trigger, with
  NO compaction directive → your Monitor is gone → proceed with steps 1–4.

**When in ANY doubt, ARM (proceed with steps 1–4).** A duplicate watcher merely
double-wakes you; wrongly skipping leaves you deaf — so never skip on a guess.

1. **Set the FE handle** — must mirror the Rust frontend's `self_comm_handle()`:
   `win-fe-<lowercase HOSTNAME or COMPUTERNAME>`.

   PowerShell:

   ```powershell
   $hostName = if ($env:HOSTNAME) { $env:HOSTNAME } elseif ($env:COMPUTERNAME) { $env:COMPUTERNAME } else { "unknown" }
   $env:SOT_COMM_NAME = "win-fe-$($hostName.ToLowerInvariant())"
   ```

   Git Bash / bash:

   ```bash
   export SOT_COMM_NAME="win-fe-$( (hostname -s 2>/dev/null || hostname || printf unknown) | tr '[:upper:]' '[:lower:]' )"
   ```

   If `~/.sot-comm/bin` is installed on this machine, join locally so
   `comm-relay.sh` sends with the right `from:` name:

   ```bash
   ~/.sot-comm/bin/comm-join.sh --name "$SOT_COMM_NAME" --expertise "Ship of Tools frontend terminal"
   ```

   This local join is for the FE machine's tools. Do not expect it to create a
   row in the backend host's shared-home registry. If sot-comm isn't installed
   at all, install first (`julia --project=. -e 'using ShipTools; ShipTools.update_comm()'`),
   then join — the BE ping below needs the joined handle so `agent.send` stamps
   `from:win-fe-<host>`.

2. **Point sends at the local tunnel** — socket-only backend mode means the
   remote backend normally listens on a private Unix socket, discovered on the
   backend host with `sotd session-socket-path ${SOT_BACKEND_LABEL:-sot}`. The
   frontend launcher opens a **local** TCP port that forwards to that remote
   Unix socket. Set relay sends to the local tunnel:

   PowerShell:

   ```powershell
   $port = if ($env:SOT_PORT) { $env:SOT_PORT } else { "18743" }
   $env:SOT_RELAY_ENDPOINT = "tcp:127.0.0.1:$port"
   ```

   Git Bash / bash:

   ```bash
   export SOT_RELAY_ENDPOINT="tcp:127.0.0.1:${SOT_PORT:-18743}"
   ```

   Do **not** try to connect to `127.0.0.1:18743` on the remote backend. In
   socket-only mode that port belongs only on the frontend machine, and only
   while the SSH tunnel/launcher is running. Browser helper ports `1234`-`1240`
   must also be forwarded by the launcher for Pluto, docs, and static pages.

3. **Re-arm the fast-comm wake** — arm a persistent harness **Monitor** on the
   FE inbox (you only reach this when Step 0 determined a genuine relaunch / cold
   start — not a compaction — so arming here can't double-arm):

   - Windows: `%LOCALAPPDATA%\sot\fe-inbox.jsonl`
   - Linux/macOS frontend: `${XDG_STATE_HOME:-$HOME/.local/state}/sot/fe-inbox.jsonl`

   The filter must (i) drop this session's OWN echoes only, and (ii) wake on
   lines addressed to **my handle** or the **`win-fe` family label** — NOT lines
   aimed at another handle (a sibling FE, or `backend-dev`), and **NOT
   broadcasts** (`to:""`). Broadcasts are *demoted*: they FILE in the inbox but
   don't wake (noise reduction — important broadcasts go on the durable bus,
   read on `/bus-sync`):

   ```bash
   HANDLE="win-fe-$(hostname | tr 'A-Z' 'a-z')"   # e.g. win-fe-devbox-2022
   state="${XDG_STATE_HOME:-$HOME/.local/state}/sot"
   [ -n "${LOCALAPPDATA:-}" ] && state="$LOCALAPPDATA/sot"
   mkdir -p "$state"; touch "$state/fe-inbox.jsonl"
   tail -n0 -F "$state/fe-inbox.jsonl" \
     | grep --line-buffered -v "\"from\":\"$HANDLE\"" \
     | grep --line-buffered -E "\"to\":\"($HANDLE|win-fe)\""
   ```

   (The trailing `"` in the `-E` pattern anchors each alternative, so `win-fe`
   matches the bare family label but NOT `win-fe-<otherhost>`; no empty
   alternative means broadcast `to:""` files without waking.) Use the **Monitor**
   tool with `persistent: true`. A plain background `tail` does NOT wake you —
   only a Monitor turns each new inbox line into an event. Local frontend
   storage is not NFS, so `tail -F` is appropriate here. Without this Monitor,
   the FE receives messages but this Claude session will not wake on them.

4. **Ping the BE** — prove the daemon is *dispatching* AND the FE is *writing
   the inbox* (the exact substrate the Monitor depends on), not merely that the
   FE process is up. There is no dedicated ping op; instead round-trip a
   self-addressed `agent.send` and confirm the nonce lands back in
   `fe-inbox.jsonl`. The path it exercises is the whole wake chain:
   relay → daemon socket → broadcast → FE → inbox write.

   ```bash
   HANDLE="win-fe-$(hostname | tr 'A-Z' 'a-z')"
   NONCE="be-ping-$$-$RANDOM"
   # SOT_RELAY_ENDPOINT from step 2 — the FE's local tunnel endpoint
   INBOX="$state/fe-inbox.jsonl"
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
   `SOT_RELAY_ENDPOINT` set explicitly (git-bash has no `sotd` process to
   `pgrep`); use the same endpoint the FE connects to.

   If you know a backend session handle, also send a directed announce
   (`comm-relay.sh send @<be-handle> "[$HANDLE] FE receive path armed"`).
   Broadcasts may file silently on backend sessions, so they are not a
   round-trip proof.

5. **Catch the deaf-window gap** — read the tail of `fe-inbox.jsonl` (lines
   where `from` != your handle) and surface anything new. The daemon replays
   missed `agent.message`s into the inbox on FE reconnect, so messages sent
   while you were down are usually still there — you only missed the live wake.

6. **`/bus-sync`** — the durable git-bus fallback for anything the relay didn't
   replay.

## Troubleshooting

- `connection refused` on `127.0.0.1:18743`: the local SSH tunnel is not running
  or is bound to a different local port. Restart the frontend launcher/tunnel.
- Backend says no remote TCP listener: expected in socket-only mode. Query and
  forward the remote Unix socket instead.
- No messages wake the FE Claude session: confirm the native frontend is
  running, `fe-inbox.jsonl` is growing, and the step-3 Monitor is still active.

## Why a skill (not a hardcoded resume prompt)

The FE's ADR-0017 resume command calls `/sot-fe-session-start` so this runs
automatically on every relaunch. Keeping the actions here means we iterate on
the bootstrap in one editable place instead of in the FE/launcher config.
