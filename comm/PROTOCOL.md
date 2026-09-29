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
  read/<name>.cursor       # per-recipient read cursor (COUNT of inbox lines shown)
  self/<host>__<pane>.txt  # this pane's chosen agent name (identity recovery)
```

The registry and inboxes are **data at rest** — discovery and catch-up need a
shared place to publish, not a live broker. In an optional shared-home
deployment, one `~/.sot-comm` serves every host sharing that home.

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

ISO-8601 UTC timestamps sort lexically, but they are NOT the read cursor: the
cursor is the NUMBER of inbox lines already shown. Stamps are second-resolution
and every comparison was strictly-greater, so a frame filed in the same second
as one already read was shown to nobody while its sender was told it had
landed. A count cannot lose a frame that way. `comm-poll.sh` is the only writer;
a legacy timestamp cursor is converted on first read (the count of lines at or
before it).

`to` equal to the handle is directed mail and counts as unread; `""` is a
broadcast copy, filed and read at the next poll, never counted as unread. A
line with no `to` key is legacy (pre-stamp, before 2026-06-12) and reads as
directed.

## Delivery

This section is ADR 0049's design of record. The mechanism below lands in
stages; until each stage does, the two-mode delivery, ping watcher and relay
bridge this replaces stay in place.

**Sending** picks one of two routes, by whether the sender's own comm folder
lists the receiver:

1. It can reach the inbox (same box, or a box sharing the home): it adds the
   line itself.
2. It cannot: it hands the message to the hub, which offers it to every
   daemon linked to it. The daemon whose comm folder holds that inbox adds
   the line and says "filed" (the hub itself, for its own home). The hub
   passes that back.

A liveness check runs first: a row still runs a session with that handle, or
the session was active in the last ten minutes.

**The one result**, nothing else:

- `filed -> @h` (exit 0).
- `FAILED -> @h: <reason>` (exit 1): no box knows that handle; no live
  session holds it; the hub cannot be reached; or no daemon said "filed"
  within 5 seconds. Nothing was added. Retrying or reporting is the sender's
  call.

Nothing is queued anywhere and there is no second route. To get an answer,
send, end the turn, and be woken.

**Waking.** Every two seconds each daemon looks at every row it runs. If the
row's handle has unread mail and the row sits at a free prompt, the daemon
types one fixed line, `[sot-comm] you have mail: run comm-poll.sh`, and
Enter. The daemon does this, not the frontend or the sender — several
frontends can show one row and each would type, a closed window would leave
the row deaf, and a pasted message is never marked read so it would show
again. One line per new batch of mail; one more if it is still unread ten
minutes later at a free prompt. A busy session needs no typing: its
end-of-turn check will not let a turn finish while unread mail waits. This
is the only wake — no per-session watcher, listener, bridge or Monitor
exists.

**Cases.**

- A Claude or Codex row gets the same line and check; the free-prompt test
  knows each tool's prompt.
- A row that just restarted, compacted or cleared re-arms nothing: the
  handle stays with the row and the count is a file.
- A session outside any row (a bare terminal, or one on a box with no
  daemon) has a handle and an inbox, and its end-of-turn check reads new
  mail before a turn ends. While idle it sees mail only at its next turn,
  and after ten idle minutes sends to it fail as "no live session".
- A subagent is part of its parent session: it may send under the parent's
  handle and never reads the inbox.

## Verbs (reference client = `bin/*.sh`)

| Verb        | Script           | Notes |
|-------------|------------------|-------|
| join        | `comm-join.sh`   | `--name <n>` `--expertise "a, b"`; writes registry + self file. Refuses (exit 3) when the self-file slot is already claimed for a DIFFERENT project — the slot is keyed by the workspace row in the environment while the identity comes from the shell's cwd, and a row that comes to name another project's session reads that session's mail. `--repin` is the deliberate override |
| audit slots | `comm-self-audit.sh` | compares each workspace-keyed slot's key against the `repo=` it carries; reports the ones naming a different project (exit 1), passes a suffixed or path-disambiguated name |
| send        | `comm-send.sh`   | `@name "msg"` or `--broadcast "msg"`; recipient is only the first positional `@arg`, so the message may itself begin with `@`. **Either verb routes**: a directed target this box's registry names is filed locally (`comm-relay.sh send` execs here), and one it cannot name goes to the wire (this execs `comm-relay.sh send`). The triggers are mutually exclusive, so a session never has to know which verb reaches a peer |
| poll        | `comm-poll.sh`   | shows the inbox lines past the read cursor, then advances it |
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
