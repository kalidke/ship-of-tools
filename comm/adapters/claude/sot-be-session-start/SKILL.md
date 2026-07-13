---
name: sot-be-session-start
description: Bootstrap a (re)started Ship of Tools BE (backend / tmux) session so it RECEIVES instant fast-comm — run the generic /sot-session-start receive-bootstrap, then add sot-specific checks: confirm a frontend is reachable, report how many FEs are attached, and pull the .claude-bus git fallback. The BE counterpart of /sot-fe-session-start (which is FE-only). Runnable manually or as a `claude --continue` resume turn. Activates for "be session start", "backend session start", "rearm be comm", "be bootstrap session".
---

# sot-be-session-start

The first turn after a Ship of Tools **backend** (tmux) Claude session is (re)started or
resumed with `claude --continue`. This is the **Ship of Tools layer** on top of the generic
`/sot-session-start` receive-bootstrap: it establishes the receive path exactly the
same way, then verifies against the Ship of Tools **frontend(s)** and pulls the Ship of Tools-repo
git bus. It's the **BE counterpart** of the FE's `/sot-fe-session-start`.

(If you just want a project-agnostic backend session to receive fast-comm, run
`/sot-session-start` directly — or launch with `ccb`. This skill, and the `ccbe`
launcher, are the sot-flavored superset.)

## Step 1 — generic receive-bootstrap (run `/sot-session-start`)

**Run `/sot-session-start` now.** Its 3 steps establish receiving for any session
and apply here verbatim:

- **(a)** `comm-join.sh` (no args) — joins as `<repo>-<host>` (e.g. `backend-dev`) and
  prints `Joined sot-comm as @<handle>`, which IS your identity (no separate
  re-check),
- **(b)** in parallel: start the durable listener (`comm-listen.sh`), arm the
  polling inbox Monitor (`comm-watch.sh <handle>`, poll — **not** `tail -F`, the
  inbox is on NFS), and `comm-poll.sh` the down-window gap,
- **(c)** one post-arm `comm-listen.sh --selftest` proves listener + file-delivery
  + Monitor-wake in a single shot.

Come back here once your own wake path is proven — the rest is the sot-specific
layer.

## Multi-frontend reality (since PR #7, multi-frontend-awareness)

A user may roam across several Windows machines, so **multiple FEs attach to one
backend at once**. The daemon **broadcasts** every `agent.message` to **all**
connected FEs — the `to:` field is an advisory label, **not** enforced routing
(proof: a `to:backend-dev` self-test also lands in every FE's inbox). Consequences:

- **FEs use per-machine handles `win-fe-<host>`** (e.g. `win-fe-desktop`,
  `win-fe-laptop`) so their traffic carries provenance and you can target one. A
  shared `win-fe` would (1) make the FE's own `grep -v from:win-fe` echo-filter
  swallow its *sibling* machines' traffic and (2) leave a specific FE untargetable.
  The FE `/sot-fe-session-start` derives the same per-host handle in lockstep.
- **Your BE handle stays unique** (`<repo>-<host>`, e.g. `backend-dev`), so the
  Monitor's from-filter only drops **your own** echoes — every FE's *direct*
  messages (`from:win-fe-*`, addressed to you) wake you, which is exactly what
  a BE wants. (Since the to-preserving bridge upgrade the Monitor also demotes
  relay *broadcasts* — `to:""` cc traffic files silently for `comm-poll.sh`
  instead of waking you; see the sot-session-start "arm the fast-comm wake" notes.)
- **The daemon tracks live FEs**: it logs `frontend connected … connections=N`
  (and `disconnected … connections=N`) on every attach/detach, and returns
  `clients_connected` in `HelloRes`.

## Socket-only relay reality

The backend no longer has to listen on a TCP port. In the normal install it
listens on the backend user's private Unix socket, discovered with:

```bash
sotd session-socket-path ${SOT_BACKEND_LABEL:-sot}
```

The comm scripts auto-detect that socket. Only override the endpoint for unusual
topologies:

```bash
export SOT_RELAY_ENDPOINT=unix:/path/to/sot.sock       # backend host
export SOT_RELAY_ENDPOINT=tcp:127.0.0.1:<local-port>   # FE host local tunnel
```

Do **not** expect the remote backend to listen on `127.0.0.1:18743`. That port is
only a frontend-machine tunnel endpoint when a launcher forwards it to the
remote Unix socket.

## Step 2 — verify against the frontend(s)

**2a — ping a FE, but DON'T block on it (non-blocking).** Step 1(c) proved your
*own* Monitor wakes; this *pings* a FE to confirm one is alive — a different path,
don't conflate them. Because the daemon broadcasts, one `send` reaches **all** FEs
and any reply confirms reachability — no designated primary needed for roaming.
`@win-fe` still works as an advisory broadcast label after the per-host migration
(delivery ignores `to:`); replies come back as `from:win-fe-<host>`.
Because delivery is daemon-broadcasted, the FE does not need to appear in this
machine's `~/.sot-comm/registry.json` for this ping to reach an attached
frontend; the registry is still required for durable `comm-send.sh` delivery
between backend sessions.

Crucially, **do NOT sit in a ~45s synchronous `ask`** — your Monitor is already
armed (Step 1(c)), so it will surface a FE reply whenever it lands, even seconds
later. Fire-and-forget instead and move on:

```bash
~/.sot-comm/bin/comm-relay.sh send @win-fe "[question] <handle> BE receive-path check; any FE please reply 'ack'."
```

- Report **"FE ack pending (Monitor will surface it)"** and continue to Step 2b/3.
  When a FE replies, it lands in your inbox and the armed Monitor fires
  `[relay] from win-fe-<host>: ack` — *that* is the round-trip confirmation, and
  it arrives without you blocking for it.
- `relayed -> win-fe` only means the *daemon* accepted the frame — NOT that any FE
  got it. Only a reply (via the Monitor) proves it; a wake+reply can take **>30s**,
  which is exactly why blocking is wrong — the armed Monitor catches it regardless.
- **A reply that never comes is not proof of a dead path** (no quiet fallback). With
  multiple roaming FEs, silence means *none* replied yet — all mid-task, or none
  attached. Step 1(c) already proved your own half; note the FE ack as pending, and
  use `/bus-sync` (Step 3) as the durable cross-OS fallback meanwhile. If you
  genuinely need a synchronous answer for a specific decision, you can still
  `comm-relay.sh ask @win-fe "..." 45` deliberately — but the bootstrap itself must
  not stall on it.

**2b — report the roaming state (PR #7).** Surface how many FEs are attached so the
session knows the multi-FE picture on start. Grep the backend log for the latest
`frontend connected … connections=N` (path is deployment-specific — wherever
`sotd` writes its log), or read `clients_connected` from a `hello` probe,
and note "N FE(s) attached." Best-effort — don't fail the bootstrap if the log isn't
handy.

## Step 3 — git-bus fallback

**`/bus-sync`** — the durable git-bus fallback for cross-OS messages the relay never
delivered (e.g. the Windows side posted while no listener was up). Surfaces new
entries from `.claude-bus/from-windows.md` in the Ship of Tools repo.

## You can drive the FE

Reminder now that you're connected: a BE session can **drive the user's frontend** —
`sot-fe preview|reveal|goto|mode|notify|open-url|repl` (and `sot-nav.sh` inside a
workspace session). Surface results there instead of only naming them in text: badge
a produced figure/file (show-result skill), open a PR/CI/dashboard URL in the user's
browser via `sot-fe open-url`. The full verb surface + discipline is in the
**sot-comm** skill's BE→FE section.

## Why a skill (not a hardcoded resume prompt)

Keeping the receive-setup here (and the generic core in `/sot-session-start`) means
we iterate on it in one editable place instead of in each BE session's launcher or
resume config. If a BE tmux session is launched with a resume command, point it at
`/sot-be-session-start` (or use the `ccbe` launcher) so this runs on every
restart.
