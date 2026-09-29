---
name: sot-comm
description: Session-to-session messaging for Ship of Tools (cross-session, cross-machine). Use for sending/broadcasting, joining/leaving, checking inbox, listing sessions, spawning/despawning agents, driving the frontend, and answering "what FE am I on" / which frontend / its build for THIS box's daemon (run `sot-fe version`; for the whole system use `sotd status` — see the `sot-status` skill). Activates on receiving "[name:repo] ...".
---

# sot-comm

Send messages between Ship of Tools/Claude sessions. Discovery + durable
inboxes live under `~/.sot-comm/`. Full contract: `comm/PROTOCOL.md`.

**What a session is told at start.** `comm-context.sh` prints your handle. Send
with `comm-send.sh @handle "text"` and read its one result. When
`[sot-comm] you have mail` appears, or your end-of-turn check says so, run
`comm-poll.sh`. To wait for a reply, end your turn. Run the session-start step
once, when a session first starts — not again on every resume.

This is ADR 0049's design of record, landing in stages: today the failure verdict
reads `no such handle: <h>` or `NOT CONFIRMED:` rather than `FAILED ->`, and the
line typed into a row reads `[sot-comm] new message for @<handle> — run
…/comm-poll.sh` rather than `[sot-comm] you have mail`. There is nothing to arm,
own or re-arm. A session outside any row is never woken while idle — it sees
new mail only at its own next turn.

**Scripts** (installed by `ShipTools.install_comm()`): `~/.sot-comm/bin/` — always use these, never hand-roll jq/registry logic.

## Verbs

| Intent | Command |
|--------|---------|
| Join (once per session) | `comm-join.sh --name <name> --expertise "a, b"` |
| Who's online | `comm-list.sh` |
| Direct message | `comm-send.sh @<name> "message"` |
| Broadcast | `comm-send.sh --broadcast "message"` |
| Check inbox | `comm-poll.sh` |
| Leave (removes the row) | `comm-leave.sh` |
| Spawn a new agent for a task | `comm-spawn.sh <name> <repo-path> --expertise "..." --task "..."` |
| Tear down a spawned agent | `comm-despawn.sh <name\|slug>` |
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

**The row is a set of facts (floor/question/waiting/done), reduced to one
colour: a running turn (green) or an open question with no turn running
(red) both outrank a wait (purple), which outranks an unviewed result
(blue), which outranks nothing set (gray).** `waiting` is set once; it
survives intervening turns until you report `working`/`idle`/`done` — there
is no time-based self-heal, the same as blue. Blue clears on the user's next
genuine prompt, or on viewing the row — never by age. Mechanics + fixture-
testing rule + turn-end auditor: `references/work-state.md`.

## After you send — end your turn

`send` is one-shot and instant; the *reply* is not — the peer has to wake,
think, and answer (seconds to minutes). Silence is think-time, not failure:
don't re-send or block-wait — set `comm-status.sh waiting "..."` and end the
turn; you'll see the reply at your next turn boundary, or be typed into if
you're sitting at a free prompt when it lands. Re-send only with positive
evidence the message was lost (peer was deaf or restarted).
`comm-send.sh @handle "msg"` reaches anyone, same box or across machines —
it picks the route itself (`comm-relay.sh` is the plumbing underneath, not a
verb you call). A handle this box's registry names is filed locally (`filed
-> @handle` — the file IS
the ack, read at that session's next turn boundary); one it cannot name goes
over the wire, and there the verdict is the FILER'S RECEIPT: `filed -> @handle
(by <filer>, relay)` means that named filer appended your frame. Failures are
loud and specific, all non-zero:

- `no such handle: h` — nothing is attached to the hub at all, so there is
  nowhere for the frame to land. Check the spelling first.
- `NOT CONFIRMED: sent for @h; nobody claimed it within 5s. Attached: …` — the
  frame was sent and may well have been filed; nobody claimed it. A receipt is
  only ever positive (nobody can honestly say "not me" about a handle they do
  not host), so this one line covers a misspelled handle, a frontend too old to
  claim, and a filer that was simply slow. The `Attached:` list is a
  diagnostic, not a delivery.
- `NOT CONFIRMED: this daemon predates filer receipts` — the hub is older than
  the mechanism; nothing on that path can vouch for anything.

The frame is never withdrawn by a failure; only the claim that it landed is.
There is no "only a reply proves it" rule any more.

`filed -> @handle` carries one more factual clause when the registry can
support it — `(working, stamped 12s ago — reply expected at its turn
boundary)`, `(needs its own user, stamped 6m ago: "...")`, `(idle, stamped
32m ago)`, or `(no heartbeat for 8h — may be gone)`, this last overriding
the others. Read it before assuming silence means ignored; a missing clause
means the registry had nothing to say, not that the peer is fine.

## Naming — from the repo, never the task

Durable BE peers `<repo-lowercase>-<host>`; a spawned agent on a repo
checkout is bare `<repo-lowercase>`; a git worktree adds `-wt-<shortname>`
(via `/worktree` — never hand-add it). `fe@<host>` is the frontend
PROCESS's own address (`sot-fe --fe <host>`) for directed
`fe.command`/`open-url` targeting, never a session handle — a session in the FE's own Terminal drawer is a session
like any other, named the same way as everything above. Full table:
`comm/PROTOCOL.md` § Naming.

## Driving the frontend (`sot-fe`)

The show verbs (`preview`, `reveal`, `goto`, `mode`, `notify`, `open-url`)
go to whichever FE the owner is active on (the daemon resolves this itself;
falls back to broadcast when no frontend is active) unless scoped with
`--fe <host>`; `repl`, `type` and `screen` are daemon requests answered to
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

**Never hand-roll a Claude session outside a workspace row** (it inherits
your `CLAUDECODE` env). A durable peer is a capsule row: `comm-spawn.sh`, or a
`workspace.create` from the FE Sessions mode. Endpoint override and worktree
spawning: `references/spawning.md`.

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

## Which frontend am I on

For the WHOLE system (every host, every daemon, every row, every attached
client) use `sotd status` instead — see the `sot-status` skill. For THIS
box's own daemon only, one command, authoritative, no log reading:

```bash
~/.sot-comm/bin/sot-fe version
```

It prints every connected frontend (`fe <label>@<host> <build> active|idle`), the
daemon's build, the deployed comm scripts, and every row's phase. The frontend
the user is on is the `fe` line marked `active`; its build says whether it has a
given fix. Never answer this from sotd.log or the comm registry: a frontend is a
client, not a comm peer, so the registry never lists it.
