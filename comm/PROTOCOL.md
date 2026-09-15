# sot-comm protocol — v1

Session-to-session messaging for Ship of Tools. A fork of the `agent-comm` user skill
with the single-session jail removed and a durable inbox fallback added, so
sessions can address each other across capsule rows and across machines.

This file is the **contract**. Every client — the Claude skill today, a Codex or
Gemini adapter or the in-app Ship of Tools `Tool` plugin later — implements *this*, so
all clients are mutually addressable through the same registry and inboxes.

## Layout (runtime, under `$SOT_COMM_HOME`, default `~/.sot-comm`)

```
~/.sot-comm/
  bin/                     # installed scripts (the reference client)
  registry.json            # who is reachable + liveness  (source of truth for discovery)
  .registry.lock/          # mkdir-based spinlock for registry writes
  inbox/<name>.jsonl       # durable per-recipient queue (append-only)
  read/<name>.cursor       # per-recipient read cursor (ISO ts of last-shown msg)
  self/<host>__<pane>.txt  # this pane's chosen agent name (identity recovery)
```

The registry and inboxes are **data at rest** — discovery and catch-up need a
shared place to publish, not a live broker. In an optional shared-home
deployment, one `~/.sot-comm` serves every host sharing that home.
For cross-machine with no shared FS, point `SOT_COMM_HOME` at a
git-synced directory; the same files then ride the existing bus. (Not auto-wired
in v1 — the `.claude-bus` git loop can still cover separate filesystems.)

## registry.json

```json
{
  "protocol_version": 1,
  "agents": {
    "<name>": {
      "host":       "myhost",                      // hostname -s; used for same-host delivery
      "workspace_id": "ws-…",                      // the row this session runs in (SOT_WORKSPACE_ID); "" in a bare shell
      "repo":       "Ship of Tools",
      "expertise":  ["files", "rust-backend"],
      "status":     "idle",                        // lifecycle: idle | spawning
      "joined":     "2026-05-29T18:00:00Z",
      "last_seen":  "2026-05-29T18:04:00Z",        // heartbeat, bumped on send/poll/join
      "state":      "working",                     // ADE state-nav WORK state: working | idle | blocked | waiting | done
      "summary":    "rebuilding backend + BE suite",// one-line current / just-finished work ("" when none)
      "status_at":  "2026-06-15T19:32:13Z"         // when state/summary were last written; nav ages a stale "working"
    }
  }
}
```

**Liveness** is heartbeat-based, not pane-based: an agent is *live* if
`now - last_seen <= SOT_COMM_STALE_SECS` (default 600). This is what lets a
session on one machine consider a session on another reachable. `host` +
`workspace_id` are what a same-host live delivery types into.

**Work-state** (`state` + `summary`, stamped by `status_at`) powers the ADE
*state-nav* at-a-glance view, and is distinct from the lifecycle `status` above.
It is written by `comm-status.sh <state> ["summary"]` (merge-only; self-gating —
a silent no-op in any session that is not a joined comm agent). Two writers by
design: the model pre-announces long work (`comm-status.sh working "<one-liner>"`
when it judges a turn will run >~30s), and a global `Stop` hook floors each
turn-end to `idle`. The daemon joins these fields onto `workspace.list` (as
`agent_state` / `agent_summary` / `agent_status_at`, keyed by the workspace's
`agent_name`) so the frontend — which cannot read this registry directly —
renders `summary` as the per-session glance, colored by `state` and aged off
`status_at`.

## Message frame (inbox JSONL, one object per line)

```json
{"from": "<name>", "to": "<name>|\"\"", "repo": "<repo>", "msg": "<text>", "ts": "2026-05-29T18:04:00Z"}
```

ISO-8601 UTC timestamps sort lexically — the read cursor is just the `ts` of the
last message shown.

`to` ranks the line for the recipient's inbox Monitor: their own name = directed,
wakes the session; `""` = broadcast copy (relay cc traffic or
`comm-send --broadcast`), files silently for the next `comm-poll`. A line with
NO `to` key is legacy (pre-stamp, before 2026-06-12) and reads as directed —
which is why an unstamped `--broadcast` once woke the whole network at once.

## Delivery — two modes, chosen by reachability, always visible

1. **Durable always:** every send appends the frame to `inbox/<target>.jsonl`.
2. **Live wake — directed sends only:** a directed send to a recipient whose
   registry row names a workspace row on the sender's host is typed into
   that row through the daemon's `pty.input` (Enter appended; the send
   reports `delivered live` only when the daemon confirms `enter_sent`).
   Otherwise the recipient's own inbox Monitor (`/sot-session-start`) polls
   `inbox/<name>.jsonl` and wakes the session; a codex row is woken by
   `codex-watch.sh`, which injects each directed frame through the same
   `pty.input`. The message text is `[<from>:<repo>] <msg>`. **Broadcasts are
   never typed** — text+Enter is a full interrupt (it submits into the
   recipient's claude, costing a model turn), so broadcast copies are
   durable-only and surface on the next `comm-poll`, matching the Monitor's
   demotion rule.

If live delivery isn't possible (no row for the recipient on this host, no
daemon, a row that is not ready) the send reports `queued to inbox (…)` — the
fallback is **stated, never silent**. The recipient sees it on the next `poll`.

**Cross-machine receive** is the relay bridge `comm-listen.sh` starts: a
reconnect loop (`comm-relay.sh bridge --name <name>`) run as a background
child of the session's own process tree, pid recorded in
`state/bridge-<name>.pid`. It lives in the session's capsule leg and dies
with it; `--status`/`--stop` and `comm-leave.sh` follow the pidfile, and a
start reaps any stray bridge for the handle (one without a pidfile) first,
so one frame is never filed twice. A Windows frontend has no bridge — the
frontend files inbound frames itself.

## Verbs (reference client = `bin/*.sh`)

| Verb        | Script           | Notes |
|-------------|------------------|-------|
| join        | `comm-join.sh`   | `--name <n>` `--expertise "a, b"`; writes registry + self file |
| send        | `comm-send.sh`   | `@name "msg"` or `--broadcast "msg"`; recipient is only the first positional `@arg`, so the message may itself begin with `@` |
| poll        | `comm-poll.sh`   | shows inbox entries newer than the read cursor |
| list        | `comm-list.sh`   | all agents + live/stale + (me) marker |
| leave       | `comm-leave.sh`  | removes self from registry; `--name <handle>` removes an orphan row (registry only — `comm-despawn.sh` is full teardown) |

## Naming — everything derives from the repo

One rule, enforced where possible: **names come from the repo, never from the
task**. A task-named anything is unfindable next to its repo-named siblings
(a spawn labeled `edge-classify` hid the MyPackage agent, 2026-06-12).

| Thing | Convention | Example |
|-------|-----------|---------|
| Durable BE peer handle | `<repo-lowercase>-<host>` | `myrepo-myhost` (Ship of Tools on the backend host), `lldevtools-myhost` |
| Spawned agent — repo checkout | `<repo-lowercase>` (bare, **no** descriptor) | `myrepo` |
| Spawned agent — git **worktree** | `<repo>-wt-<shortname>` (the `-wt-` infix is reserved for worktrees and groups them next to the parent; `<shortname>` names the WORKTREE, never the task). Created via the `/worktree` skill. | `MyAnalysis-wt-rotation` (worktree `rotation`) |
| Frontend address | `fe@<host>` — the frontend PROCESS's declared hello `name`, the target `sot-fe --fe <host>` scopes a directed `fe.command`/`open-url` to (two frontends on one box differ by `instance`); the frontend is a client, never a comm peer, and no session derives or joins as this name | `fe@laptop` |
| Workspace label | repo basename (comm-spawn default; task-named labels are **rejected**) | `MyPackage` |
| Workspace slug (the row's name) | derived from the label by the daemon | `mypackage` |
| Second workspace on one repo | `<Repo>-<suffix>` label, deliberately | `MyPackage-2` |

A bare `<repo-lowercase>` handle is the default for a normal repo checkout. For a
**git worktree**, use `<repo>-wt-<shortname>` — the `-wt-` infix is reserved for
worktrees (so they read as worktrees at a glance and the shared `<repo>-` prefix
groups them next to the parent in the sessions list), and `<shortname>` names the
worktree, never the task. Don't hand-roll it: the **`/worktree`** skill
(`comm-worktree-new.sh`) creates the worktree at
`<repo-parent>/worktrees/<repo>-wt-<shortname>` on branch `wt/<shortname>` and
spawns the session with that handle+label, so the slug groups it correctly. (No
host in the worktree handle — the parent is found by repo family, not host.) A deliberate second
workspace is `<repo>-2`. Never a suffix on a plain repo open, and never a task
name (`repo-fix` for a direct checkout was wrong on both counts: a
task-ish descriptor AND a shortened base; the right handle was
`myrepo`).

Task identity lives in `--task` / `--expertise` / the message body. Never reuse
a handle that has a registry row, even a stale-looking one — the owner may be
alive with a lagging heartbeat, and a collision makes two sessions execute the
same briefs in parallel.

## Conventions carried over from agent-comm (keep these)

- **Domain-expertise rule:** state your goal or the problem; suggest a fix if you
  have one, but don't dictate the other session's implementation.
- **Anti-groupthink tags:** `[design]` `[question]` `[breaks]` `[challenge]`
  `[consensus]`. No `[consensus]` without a prior `[breaks]` in the thread.

## Versioning

`protocol_version` is stamped into the registry on creation and checked on
`join`. A mismatch warns loudly (no quiet degradation) and means a machine needs
`ShipTools.update_comm()`. Bump this integer on any breaking change to the schema,
frame, or delivery semantics.
