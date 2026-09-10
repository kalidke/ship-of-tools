---
name: sot-comm
description: Session-to-session messaging for Ship of Tools (cross-tmux, cross-machine). Use for sending/broadcasting, joining/leaving, checking inbox, listing sessions, spawning/despawning agents, driving the frontend. Activates on receiving "[name:repo] ...".
---

# sot-comm

Send messages between Ship of Tools/Claude sessions. Discovery + durable
inboxes live under `~/.sot-comm/`; live delivery uses tmux paste-buffer on
the same host or the daemon relay across machines. Full contract:
`comm/PROTOCOL.md`. **Receive setup (`comm-listen.sh` + a Monitor) belongs to
`/sot-session-start`** — run that once per session; this skill assumes it's
already done.

**Scripts** (installed by `ShipTools.install_comm()`): `~/.sot-comm/bin/` — always use these, never hand-roll tmux/jq/registry logic.

## Verbs

| Intent | Command |
|--------|---------|
| Join (once per session) | `comm-join.sh --name <name> --expertise "a, b"` |
| Who's online | `comm-list.sh` |
| Direct message | `comm-send.sh @<name> "message"` |
| Broadcast | `comm-send.sh --broadcast "message"` |
| Check inbox | `comm-poll.sh` |
| Leave | `comm-leave.sh` |
| Bootstrap an unjoined session | `comm-bootstrap.sh <tmux-target> [name]` (pastes a join+reply nudge) |
| Spawn a new agent for a task | `comm-spawn.sh <name> <repo-path> --expertise "..." --task "..."` |
| Tear down a spawned agent | `comm-despawn.sh <name\|slug>` |
| Instant cross-machine message | `comm-relay.sh send @<name> "msg"` / `ask @<name> "msg" <timeout-s>` |
| Show a result in the FE | `sot-fe preview <ws> <path>` (badge-floor — never force-switches the user's view) |

(All paths are `~/.sot-comm/bin/<script>`.)

## Work-state (mostly automatic)

Hooks set **working** (turn start), **done** (turn end — blue = "finished a
turn the user asked for, unread"; a machine-started turn floors to gray
**idle** instead), and **blocked** (an open `AskUserQuestion`) for you.
Self-report the two cases hooks can't see:

```bash
comm-status.sh blocked "the question you're asking"   # plain-text question, no AskUserQuestion tool
comm-status.sh waiting "what you're waiting on"        # turn ended with a background job/subagent still running
```

**Closing markers.** A turn that CLOSES an effort, or ends parked, ends with
the closing block from the `sitrep` skill: a line opening `SITREP: <headline>`
(done), `SITREP-QUESTION: <the question>` (blocked) or `SITREP-WAITING: <what
for>` (waiting), then the chain in plain words. The Stop hook stamps the row
from that line, so no status call is needed at turn end. A step in a live
back-and-forth owes no block — answer and end. Nudges, each once per turn: a
human turn ending parked without a marker; a long human turn (many tool calls
or minutes of wall time) ending green without one — "close with the block if
this closed an effort, end normally if it was a step". A turn that already
carries its block is never nudged, whatever the block says — a send-back can
only append, and the owner would read two reports.

**Precedence: blocked > waiting > done > idle.** **Waiting is sticky** — set it
once; it survives intervening turns until you report `working`/`idle`/`done`, or
self-heals after 2h. Blue clears on the user's next genuine prompt — never by
age. Mechanics + fixture-testing rule + turn-end auditor: `references/work-state.md`.

## After you send — trust your Monitor

`send` is one-shot and instant; the *reply* is not — the peer has to wake,
think, and answer (seconds to minutes). Silence is think-time, not failure:
don't re-send or block-wait — set `comm-status.sh waiting "..."` and end the
turn; your armed Monitor wakes you when the reply lands. Re-send only with
positive evidence the message was lost (peer was deaf or restarted).

## Naming — from the repo, never the task

Durable BE peers `<repo-lowercase>-<host>`; a spawned agent on a repo
checkout is bare `<repo-lowercase>`; a git worktree adds `-wt-<shortname>`
(via `/worktree` — never hand-add it); FEs are `win-fe-<host>`. Full table:
`comm/PROTOCOL.md` § Naming.

## Driving the frontend (`sot-fe`)

The show verbs (`preview`, `reveal`, `goto`, `mode`, `notify`, `open-url`)
go to whichever FE the owner is active on (the daemon resolves this itself;
falls back to broadcast when no frontend is active) unless scoped with
`--fe <handle>`; `repl`, `type` and `screen` are daemon requests answered to
you. Full reference: `sot-fe --help`; rarer essays: `references/fe-verbs.md`.

| Verb | Does |
|------|------|
| `preview <ws> <path> [--caption <t>]` | switch + render `<path>` in the preview pane |
| `reveal <ws> <path>` | cursor `<path>` in the file tree, no preview |
| `goto <ws> [--boot]` | switch the FE to a workspace |
| `mode <mode>` | switch the FE's active mode |
| `notify <text>` | one-line notice |
| `open-url <http(s)-url>` | open in the FE machine's OS browser |
| `repl run\|eval\|interrupt\|status <ws> ...` | drive a workspace's persistent REPL |
| `type <ws> [<text>] [--stdin] [--enter]` | type literal bytes into a sibling row's pane (`pty.input`) |
| `screen <ws>` | that sibling row's CURRENT screen, no scrollback (`pty.screen`) |

**Read before you write:** never send `type` — not even a bare Enter — to a
pane you haven't just read with `screen`. claude's own trust prompt defaults
to "No, exit"; a blind keystroke can end a session instead of advancing it.

## Spawning a new agent (delegation)

```bash
comm-spawn.sh mysim ~/projects/MySim --expertise "simulation workflows" \
  --task "Add a per-emitter intensity field to Emitter; branch + PR"
```

Creates a **sot workspace** (a session-strip row, switchable with
Ctrl+PageDown), boots `ccb` on first FE attach so its own
`/sot-session-start` joins it under the name you chose, then delivers a
task-only brief. **Addressable immediately** — the handle is pre-registered,
so `comm-send.sh @<name>` queues even before it finishes joining. Label =
repo name, never the task (`comm-spawn.sh` rejects task-named labels).
Refresh the FE's session list to see the new row; despawn with
`comm-despawn.sh <name|slug>`.

**Never hand-roll a tmux pane for a Claude session** (inherits your
`CLAUDECODE` env; a claude TUI in a never-attached pane exits silently). A
human starting a durable peer by hand: `tmux -S "$SOCK" new-session -s <name>
-c <repo> ~/.local/bin/ccb` — **no `-d`**, never a detached pane. Endpoint
override, `--no-workspace`, worktree spawning: `references/spawning.md`.

## Conventions

- **On receiving `[name:repo] ...`** (an inbound sot-comm message): join if
  you haven't (`comm-join.sh`), reply with `comm-send.sh @name "..."` —
  **local text output is not seen by other sessions.**
- **State goals, not orders** — the other session knows its repo better than
  you; describe the problem, let them own the implementation.
- **Anti-groupthink tags:** `[design]` `[question]` `[breaks]` `[challenge]`
  `[consensus]` (no `[consensus]` without a prior `[breaks]` in the thread).
- Delivery is durable (queued to the recipient's inbox, picked up on
  `comm-poll.sh`); liveness is heartbeat-based (`comm-list.sh` shows
  live/stale) — `poll`/`send`/`join` all refresh yours.
- To poll on a schedule: `/loop 5m comm-poll`.
