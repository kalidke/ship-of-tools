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

ISO-8601 UTC timestamps sort lexically, but they are NOT the read cursor: the
cursor is the NUMBER of inbox lines already shown. Stamps are second-resolution
and every comparison was strictly-greater, so a frame filed in the same second
as one already read was shown to nobody while its sender was told it had
landed. A count cannot lose a frame that way. `comm-poll.sh` is the only writer;
a legacy timestamp cursor is converted on first read (the count of lines at or
before it).

`to` ranks the line for the recipient's inbox Monitor: their own name = directed,
wakes the session; `""` = broadcast copy (relay cc traffic or
`comm-send --broadcast`), files silently for the next `comm-poll`. A line with
NO `to` key is legacy (pre-stamp, before 2026-06-12) and reads as directed —
which is why an unstamped `--broadcast` once woke the whole network at once.

## Delivery — two modes, chosen by reachability, always visible

1. **The append IS the delivery:** every send appends the frame to
   `inbox/<target>.jsonl`, and that is the acknowledgement — `filed -> @name`,
   exit 0. A filed frame is read by the recipient's next turn boundary, because
   its own end-of-turn hook reads the inbox and will not let a turn end while
   directed mail sits past the cursor. That is how a BUSY session is reached: no
   process, no keystrokes, no human.
2. **The poke — directed sends only:** a directed send to a recipient whose
   registry row names a workspace row on the sender's host is ALSO typed into
   that row through the daemon's `pty.input` (Enter appended), so a session
   sitting idle at its prompt does not wait for its next turn. It is typed only
   when the row's current screen shows a free prompt — keystrokes would
   otherwise land in an open dialog, menu or half-written draft. The send
   reports `+woken` or `not woken: <reason>` as a DIAGNOSTIC; the verdict is the
   filing either way. The message text is `[<from>:<repo>] <msg>`.
   **Broadcasts are never typed** — text+Enter is a full interrupt (it submits
   into the recipient's claude, costing a model turn), so broadcast copies are
   durable-only and surface on the next `comm-poll`.

**Ping delivery (Claude, capsule rows, ADR 0047):** a harness Monitor costs a
model turn every ~30 minutes just to re-arm, so a Claude session in a capsule
row wakes instead via `comm-wake.sh <handle> --deliver ping` — the same
`codex-watch.sh` mechanism generalized, but it types ONE fixed notice line
(never the message text) and lets the session read the real backlog with
`comm-poll.sh` on the turn the ping wakes it. A whole batch of new directed
frames costs one wake, not one per frame; a batch the read cursor already covers
costs none. There is no other suppression — an earlier unread ping does NOT
withhold a later one, because one stalled session would then go deaf to
everything queued behind the message it never read. It only types when the row's
current screen shows a free prompt — typing into an open dialog or menu could
answer it — so a busy screen delays the poke, never drops a frame: the frame is
in the inbox and the turn boundary reads it regardless. The watcher is OWNED: it
discovers the claude/codex process it belongs to, refuses to start without one
or against a live watcher for the same handle, and exits when its owner does.
`comm-session-start.sh` starts it automatically for a capsule row; outside one,
the harness Monitor is unchanged.

If the poke isn't possible (no row for the recipient on this host, no daemon, a
row that is not at a free prompt) the send says so — `filed -> @name — not
woken: <reason>` — because the reason is **stated, never silent**. The frame is
filed either way, and the recipient reads it at its next turn boundary or on its
next `poll`. A send that could file NOWHERE is the one failure: `no such handle`
(or `ERROR: unreachable, nothing filed`) with a non-zero exit.

**The recipient annotation (messaging ruling, 2026-09-26):** a filed frame is
not a reply, and a sender with no reply yet cannot tell working from
waiting-on-its-human from gone — so `filed -> @name` gains one short factual
parenthetical about the RECIPIENT, read off the same registry entry that
resolved the handle (never a second file, never the daemon):

```
filed -> @X (working, stamped 12s ago — reply expected at its turn boundary)
filed -> @X (needs its own user, stamped 6m ago: "<question, truncated to 60 chars>")
filed -> @X (idle, stamped 32m ago)                # or any other state — its own word
filed -> @X (no heartbeat for 8h — may be gone)    # overrides every other clause
```

A stale heartbeat (`last_seen` older than `SOT_COMM_STALE_SECS`, default 600s)
always wins, because a stamp from a dead session is the misleading one.
Otherwise a `blocked` state (a `question` open with no `floor`) means the
recipient is stopped waiting on its OWN human, not the sender — `floor` being
`"user"` means the opposite (the session is *actively running* a
human-started turn: `floor` present always reduces to `state: working`, ADR
0044's amendment table). A missing or malformed entry — or any field it
needs — prints NOTHING extra: the clause is a courtesy, never a guess, and
never turns a successful file into a failure. This never delays, blocks or
refuses a send; it only informs. An `ask` that times out with no reply
carries the same facts in its own `TIMEOUT:` line, for the same reason.

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
