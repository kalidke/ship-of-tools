---
name: sot-comm
description: Session-to-session messaging for Ship of Tools sessions (fork of agent-comm, cross-tmux-session and cross-machine on shared $HOME). Use when sending messages between Claude sessions, joining/leaving the sot-comm network, checking for messages, listing who is online, or starting/stopping other sessions (durable peers via ccb/ccbe, ephemeral task agents via comm-spawn). Also activates on receiving a message in "[name:repo] ..." format. Activates for "sot send", "sot comm", "comm send", "comm join", "comm poll", "broadcast to sessions", "@<session>", "comm spawn", "spawn a session", "spawn an agent", "start a session on <repo>", "despawn".
---

# sot-comm

Send messages between Ship of Tools/Claude sessions. Discovery + durable inboxes live
under `~/.sot-comm/` (shared in optional shared-home deployments); live
delivery uses tmux paste-buffer when the recipient is on the same host. Full
contract: `comm/PROTOCOL.md` in the Ship of Tools repo.

**Scripts** (installed by `ShipTools.install_comm()`): `~/.sot-comm/bin/`. Always
use these — do not hand-roll tmux/jq/registry logic.

## Verbs

| Intent | Command |
|--------|---------|
| Join (do once per session) | `~/.sot-comm/bin/comm-join.sh --name <name> --expertise "a, b"` |
| Who's online | `~/.sot-comm/bin/comm-list.sh` |
| Direct message | `~/.sot-comm/bin/comm-send.sh @<name> "message"` |
| Broadcast | `~/.sot-comm/bin/comm-send.sh --broadcast "message"` |
| Check inbox | `~/.sot-comm/bin/comm-poll.sh` |
| Leave | `~/.sot-comm/bin/comm-leave.sh` |
| Bootstrap another session | `~/.sot-comm/bin/comm-bootstrap.sh <tmux-target> [name]` |
| Spawn a NEW agent for a task | `~/.sot-comm/bin/comm-spawn.sh <name> <repo-path> --expertise "..." --task "..."` |
| Tear down a spawned agent | `~/.sot-comm/bin/comm-despawn.sh <name\|slug>` |
| Instant cross-machine message | `~/.sot-comm/bin/comm-relay.sh send @<name> "msg"` |
| Show a result in the user's FE | `~/.sot-comm/bin/sot-fe preview <ws> <path>` (badge-floor — badges the ws row; the user navigates to it. **Never force-switch the user's view.**) |

## Work-state in the state-nav (mostly automatic)

Your work-state — the colour by your name in the nav — is **event-driven and
automatic**; you normally do nothing:
- `UserPromptSubmit` → **working** (green) when a turn starts.
- `Stop` → **idle** when it ends.
- `PreToolUse` on **AskUserQuestion** → **blocked** (red) the instant you open a
  question for the user; clears when answered.

The one case with no automatic signal is a question asked in **plain text** (no
AskUserQuestion tool). For those — and only those — self-report so your row goes
red instead of looking idle:

```bash
~/.sot-comm/bin/comm-status.sh blocked "the question you're asking"
```

The question becomes your row's summary, so the user sees *what* you need at a
glance. Prefer the **AskUserQuestion** tool when you can — it flags you blocked
automatically, no self-report needed. Don't hand-report working/idle; the hooks
own those, and a manual `working` that never gets cleared sticks green.

The other self-reported state is **waiting** (purple): when you kick off a **long
job or subagent and END the turn with it still running**, you are idle of your own
work but NOT free — set it so your row reads purple, not idle-gray:

```bash
~/.sot-comm/bin/comm-status.sh waiting "what you're waiting on (e.g. 'subagent: comm-boot impl')"
```

Unlike **blocked** (which needs the *user* to act), **waiting** needs nothing from
anyone — it just means "running in the background." There is no clean tool-hook for
it (delegation spans Agent / background-Bash / Monitor, and a `PreToolUse` hook on
the Agent tool would over-fire for *inline* agents that finish within the turn), so
it is self-reported.

**Testing state machinery? NEVER against your live row.** Registering fixture
states on your own handle paints real colors on the user's strip (2026-07-04:
a stale-waiting test left its author purple with "rendering the test figure").
Join a scratch handle for fixtures, and mutate the registry ONLY through
`comm-status.sh` — raw `jq > tmp && mv` skips the registry lock and loses
races against other hook writers on shared filesystems.

**Turn-end audit (Haiku, 2026-07-02):** the Stop hook runs a tiered auditor —
cheap deterministic filters, then ONE conservative `claude -p` Haiku judgment
only when a filter trips — that checks the ending turn for three misses: a real
turn-ending question without `blocked`, a user-facing artifact (plot/PDF/
screenshot) never surfaced via show-result, and a background job armed without
`waiting`. Confirmed findings come back as a Stop nudge ("Turn-end audit: …");
the MODEL still gates — act on real findings, end the turn normally on false
ones. Same finding won't re-fire within 30 min. Kill switch:
`SOT_TURN_AUDITOR=0` or `touch ~/.sot-comm/auditor.off` (falls back to the
legacy `?`-grep nudge). Source: `comm/core/scripts/comm-turn-auditor.sh`.

**Waiting is STICKY across turn cycles** (fixed 2026-07-02): set it ONCE and it
survives intervening turns — the prompt-start hook writes a *soft* working (you
show green-working while actively processing, which is true) and the turn-end
soft idle **demotes you back to purple**, restoring your waiting summary, for as
long as the sticky marker is live. You do NOT need to re-assert `waiting` at
every turn end. An explicit `blocked` also preserves the marker (blocked > waiting
> idle; answering your question drops you back to purple, not green). The marker
clears two ways:
- **you explicitly report** `working` / `idle` / `done` (non-hook, i.e. you ran
  `comm-status.sh` yourself) when the job lands — do this, it is the accurate
  signal; or
- **self-heal**: a marker older than **2h** is dropped by the next turn-end
  (a forgotten purple can't lie forever — re-assert `waiting` for genuinely
  longer jobs).

## Instant cross-machine messaging (daemon relay)

The git bus and the file inbox are async. For **instant cross-machine** comm
(Linux ⇄ Windows, which share no filesystem), route through the Ship of Tools
backend daemon — the one live link between the machines. In current socket-only
mode the backend listens on a private Unix socket; frontend machines reach it
through a local SSH tunnel whose TCP port forwards to that remote Unix socket.
`agent.send` → daemon → `agent.message` evt broadcast to every connected client.

```bash
~/.sot-comm/bin/comm-relay.sh send @win-fe "backend restarted, relaunch FE"
~/.sot-comm/bin/comm-relay.sh ask  @win-fe "ready?" 20   # send + print replies for 20s
~/.sot-comm/bin/comm-listen.sh                           # start durable receive listener (then arm a Monitor — see below)
```

- **Endpoint resolution.** On the backend host the scripts auto-detect the Unix
  socket by checking explicit env, old dev `--tcp`/`--socket` daemon flags, then
  `sotd session-socket-path ${SOT_BACKEND_LABEL:-sot}`. Override with
  `SOT_RELAY_ENDPOINT=unix:/path/to/sot.sock` only when auto-detect cannot find
  the intended daemon. On a frontend host, set
  `SOT_RELAY_ENDPOINT=tcp:127.0.0.1:<local-forward-port>` if you are sending from
  the terminal through the local SSH tunnel. Do **not** expect a remote backend
  `127.0.0.1:18743` TCP listener in socket-only mode.
- **Send** is one-shot and instant.

  **Receiving — REQUIRED two-part setup (do this after joining).** The relay is
  live-only (no queue), and a Claude session does NOT act on a silent file write —
  it only reacts to input or a wake. So receiving needs BOTH:
  1. **A listener** that stays connected and files inbound into your inbox:
     `~/.sot-comm/bin/comm-listen.sh` (durable reconnect-loop bridge in a detached
     tmux session; idempotent; `--status`/`--stop`; `--selftest` verifies+repairs the
     receive path). The bridge self-heals on a dropped daemon connection. *Inside the
     Ship of Tools FE, the frontend files into `<state-dir>/fe-inbox.jsonl` instead — listen there.*
  2. **A Monitor that WAKES you** on each new inbox line — a harness action a script
     can't do for you; YOU must arm it as a persistent Monitor. **Pick the variant
     by filesystem — this matters before you copy anything:**
     - **Shared-home filesystem: POLL — do NOT `tail -F`.** inotify can be
       unreliable there and silently miss or delay writes (seen: a relay
       message surfaced 45 min late). Use the canonical polling Monitor from
       `/sot-session-start` step 2 — it also demotes relay broadcasts
       (`to:""`) to file-silently so ambient traffic doesn't burn a wake-up.
     - **Windows FE local-FS inbox** (`<state-dir>/fe-inbox.jsonl`): `tail -F`
       is fine there:
       ```
       tail -n0 -F <state-dir>/fe-inbox.jsonl | while IFS= read -r l; do \
         printf '%s' "$l" | jq -r '"[relay] from \(.from): \(.msg)"'; done
       ```
       (The FE bootstrap `/sot-fe-session-start` carries the full per-host
       filter variant.)
     Without one, messages land in the file but you sit idle until otherwise
     prompted.

  `comm-listen.sh` covers part 1; you arm part 2. Both sides need this for instant
  two-way. `ask`/`listen` are synchronous one-offs that skip the persistent setup.
- **After you `send`, TRUST your Monitor — do NOT re-send, poll, or block waiting
  for a reply.** A `send` is one-shot + instant, but the *reply is not*: the peer
  is a Claude session that has to be woken, read your message, think, and answer —
  normally **seconds to a few minutes**, longer if it's mid-task. **Silence is not
  failure, and it is not "the message didn't arrive."** The reply files into your
  inbox and the Monitor you armed (part 2) **wakes you** when it lands — that is
  the entire point of arming it. So after sending:
  - **Do NOT re-send the same message** because no reply has come yet — the peer
    already has it; re-sending spams and can trigger duplicate work (a peer got the
    same question 3× and kept re-answering). `relayed -> X` only means the *daemon*
    accepted the frame; only a reply proves the peer received + processed it, and
    that reply comes to you via the Monitor.
  - **Do NOT stand up a blocking watch** — a long-timeout `comm-relay.sh ask`, a
    `while`-loop `tail`, or a Bash "wake poll" — to sit and wait. It ties up your
    turn and duplicates the Monitor you already have. `ask` / a short `listen` are
    for a *deliberate, brief* synchronous check (you need the answer to pick your
    very next action, for a few seconds) — never for "wait on a peer to think."
  - **Instead:** set `comm-status.sh waiting "<what you're waiting on>"` (purple)
    and either do other useful work or **end the turn**. You lose nothing — the
    Monitor surfaces the reply and wakes you whenever it comes.
  - **Only re-send** with *positive evidence* the message was lost — you learn the
    peer was deaf/restarted (its listener was down) or `/bus-sync` shows it never
    saw it — not merely because a reply is slow.
  - **Your Monitor survives a context *summary* (compaction) — you just can't see
    it.** It is a background harness task, NOT part of the summarized context, so
    after compaction it is not re-stated in what you can read — but it is still
    armed and still watching your inbox. *Not seeing it ≠ it died.* Do not arm a
    fresh ad-hoc watch "to be safe." (Only a full `--continue` RESTART actually
    kills it — and the fix there is `/sot-session-start`, whose
    `comm-listen.sh --selftest` proves the wake path in one shot, never a bespoke
    short watch.)
  - **Calibrate for peer think-time.** A substantive peer reply routinely takes
    **minutes** — the peer must be woken, read, reason, and compose. A short empty
    window (a 60–90s watch) or a peer `last_seen` that looks a few minutes stale is
    NOT evidence of a dead path or a deaf peer; it almost always means "still
    composing." Your armed Monitor catches the reply whenever it lands, so there is
    no window you need to "keep open."
- **Receive on Windows**: the native frontend writes inbound messages to
  `<state-dir>/fe-inbox.jsonl` (`%LOCALAPPDATA%\sot\`); the in-terminal FE agent
  reads that. To send from Windows, use `comm-relay.sh` with
  `SOT_RELAY_ENDPOINT=tcp:127.0.0.1:<local-forward-port>` or send the same
  `agent.send` JSON frame to that local tunnel. The port is local to the FE
  machine and terminates at the backend's Unix socket over SSH.
- Requires a daemon built with the relay (ships alongside the `workspace.changed`
  push).

`--name` is optional (defaults to `<repo>-<host>`). **Names derive from the
repo, never the task** (canonical table: `comm/PROTOCOL.md` § Naming): durable
BE peers `<repo-lowercase>-<host>` (`myrepo-myhost`, `lldevtools-myhost`), spawned
agents bare `<repo-lowercase>` (`mypackage`) for a repo checkout, FEs
`win-fe-<host>`. **Add a `-<descriptor>` suffix ONLY for a git worktree**
(`<repo-lowercase>-<worktree>`, e.g. `myrepo-fix` for worktree
`fix`) — the descriptor names the worktree, never the task; a plain repo open
takes the bare repo name. If several sessions genuinely share a repo+host,
suffix (`<repo>-<host>-2`) — never a task name, and never a handle that already
has a registry row.

## Showing your outputs in the frontend — push them, don't just describe them

Ship of Tools exists to render results at native fidelity in the FE. **Whenever your work
produces something the user should SEE — a saved plot/figure, a rendered image, an
output file, a built doc — push it to their FE preview and tell them it's up.** A
figure left on disk and merely *named* in text defeats the whole premise. This is
not optional polish; it is how a session delivers a visual result. Be aggressive:
any saved plot, generated image, rendered doc, or notable output is a candidate.

Use the `sot-fe` command (the **op::FE_COMMAND** channel — built + shipped,
ADR-0025):

```bash
~/.sot-comm/bin/sot-fe preview <workspace-slug> <path>   # render in the FE preview pane
~/.sot-comm/bin/sot-fe reveal  <workspace-slug> <path>   # cursor in the Files tree (no preview)
```

`<path>` is relative to that workspace's root (an absolute path under it is
auto-relativized); `<workspace-slug>` is normally your own (`$SOT_WORKSPACE`).

**Discovering your slug — never guess it.** Prefer `$SOT_WORKSPACE`: the backend
stamps it when it *creates* a workspace tmux session. It can be **unset**, though —
a pane that predates the stamp, was *attached* rather than created, or lost it on a
re-shell won't have it (`SOT_SESSION=1` may still be set without it). Then derive
the slug from your tmux session name by stripping the `sot-be-` prefix
(`sot-be-<slug>`). `sot-nav.sh` does this automatically —
`SLUG=${SOT_WORKSPACE:-$(tmux display-message -p '#S' | sed -n 's/^sot-be-//p')}` —
so a session never has to guess the repo name.

**It does NOT require the FE to be on that workspace** — that's the whole point of
op::FE_COMMAND over the old gated `sot-nav.sh`. The daemon broadcasts to every
connected FE. A FE already viewing that workspace shows the file immediately
(nav cursor + preview). A FE elsewhere **badges the workspace row and never
steals the user's view** — and when the user switches to that workspace, the
consume is COMPLETE: the file is cursored in the nav AND rendered in the
preview, automatically (maintainer semantics 2026-07-10: "always set the nav
and show" means selection + preview are both set when seen, "not to yank my
session over"). `--urgent --fe <handle>` is the user-requested focus-capture
variant; broadcast urgent is stripped FE-side. Do NOT use it proactively.

**Then END your reply telling them it's there**, in those words, e.g.:

> "…and that figure is now showing in your nav pane."

### The full `sot-fe` verb surface (`sot-fe --help`)

`preview`/`reveal` above are the show-a-file verbs. The rest drive the FE the same
way — broadcast by default (the badge floor), `--fe <handle>` to target one FE:

- `goto <ws>` — switch the FE to a workspace (no path). `--boot` seeds autostart so
  the FE boots `ccb` on the switch (the scriptable spawn->goto->boot primitive —
  send **directed** with `--fe`, never broadcast).
- `mode <files|modules|project|...>` — switch the FE's active mode.
- `notify <text> [--level l] [--ws w]` — surface a one-line notice.
- **`open-url <http(s)-url>`** — open a URL in the FE machine's OS browser (a PR you
  opened, a CI/release page, a dashboard, a `wglshow` figure). **Underused — reach
  for it instead of pasting a link the user has to copy.** Plain https always works;
  a loopback URL resolves only if the FE launcher forwards that port.
- **`repl run <ws> <path>` / `repl eval <ws> --code <c>`** — run a `.jl` file (or eval
  code) in a workspace's persistent REPL and get the COLLECTED output back
  (stdout/stderr/value/error + figure paths). A real request/response op
  (`repl.execute`, ADR 0033): blocks until done or `--timeout` (120s default),
  non-destructive (runs in the REPL's current project, no reset). This is how a
  session drives another workspace's REPL and reads the result — e.g. serving a
  `wglshow` figure into a peer's browser.

**Discipline: every new BE->FE command lands in THIS section, in lockstep with the
`sot-fe` verb that ships it.** A capability no session can discover is dead weight —
this list sat without `open-url` and `repl` for a while and sessions never reached
for them.

## Bootstrapping another session (first contact)

A session is only addressable by `@name` once it has *joined* — that's the consent
model. If another session has the skill installed but hasn't joined, you can
enroll it: find its tmux target and nudge it.

```bash
# discover live panes (each is a candidate session)
tmux list-panes -a -F '#{session_name}:#{window_index}.#{pane_index}  #{pane_id}'
# paste a join+reply instruction into it
~/.sot-comm/bin/comm-bootstrap.sh sot-be-lab-guide:1.1 lab-guide
```

`comm-bootstrap.sh` pastes a self-contained "run comm-join then reply to me"
message into the target's prompt (via `comm-send.sh --force-target`, the only path
that bypasses the registry). Once the target joins it appears in `comm-list.sh`
and you exchange messages normally with `@name`. Use `--force-target` directly only
for raw one-off delivery; prefer `comm-bootstrap.sh` for enrollment.

## Spawning a new agent for a task (delegation)

When you need work done in **another package** and want a dedicated agent to do
it and report back, spawn one. By default `comm-spawn.sh` creates the agent as a
**sot workspace** (via the backend `workspace.create` op), so it shows up in
the frontend **session strip** and is switchable with **Ctrl+PageDown** — and
switching also gives you that package's files / REPL / concept layer. On first
FE attach the agent launches via **`ccb`** with `SOT_COMM_NAME=<name>` (so
its `/sot-session-start` first turn joins under the handle you chose, starts
the listener, and arms its inbox Monitor), then receives a **task-only brief**
— who it is comes from the repo's CLAUDE.md, and joining is ccb's job, so the
brief carries nothing but the task and the report-back contract.

**The agent is addressable the moment the spawn returns.** comm-spawn
pre-registers the handle (status `spawning`) and creates its inbox, so
`comm-send.sh @<name> "..."` works immediately — the message queues durably,
and the agent reads the backlog (its bootstrap ends with `comm-poll.sh`) and
replies once the comm systems are up, ~1 min after first attach. Don't re-send
on silence inside that window; `comm-list.sh` shows when `spawning` flips to a
real join.

```bash
~/.sot-comm/bin/comm-spawn.sh mysim ~/projects/MySim \
  --expertise "simulation workflows" \
  --task "Add a per-emitter intensity field to the Emitter struct; branch + PR"
```

**Naming rule (enforced): sessions are named after the REPO, not the task.**
The workspace label defaults to the repo basename and drives the slug + tmux
session name (`sot-be-<slug>`) — leave `--label` alone. A task-named label
(`--label edge-classify`) hides the agent from the user next to its repo-named
siblings; `comm-spawn.sh` now rejects any label that isn't the repo name or
`<Repo>-<suffix>` (the suffix form is for a deliberate second workspace on the
same repo). Task identity goes in `--task`/`--expertise`; the conventional
*handle* is the lowercase repo name (`mypackage`), like the durable-peer
`<repo>-<host>` convention minus the host.

The spawned agent joins as `@<name>`, does the task, and reports back to **you**
(the spawner, resolved from your own registry handle) over the bus — coordinate
via `comm-poll.sh` / `comm-send.sh @<name>`. The brief reminds it that local
text is invisible to peers; everything else (identity, branch policy, repo
conventions) it gets where every session does — the repo's CLAUDE.md.

**Seeing it in the FE:** the frontend learns about new workspaces on its next
`workspace.list` poll, so **refresh the session list (enter Sessions mode)** after
spawning and the new row appears; then Ctrl+PageDown to switch to it. Tear it down
with `comm-despawn.sh <name|slug>` (removes it from sot-comm and destroys the
workspace, so the strip row goes away).

Notes: you must be joined first (the spawner handle comes from your session). The
daemon endpoint is auto-detected from explicit env, old dev `--tcp`/`--socket`
daemon flags, or `sotd session-socket-path ${SOT_BACKEND_LABEL:-sot}`; override
with `--endpoint unix:/path/to/sot.sock` or `--endpoint tcp:HOST:PORT`. Tune boot wait with
`SOT_COMM_SPAWN_WAIT` (default 6s). Use `--no-workspace` for a plain tmux
session with no daemon (headless; won't appear in the FE strip).

**Durable peer instead of a task agent?** `comm-spawn.sh` is for delegation with a
report-back-and-despawn lifecycle. For a long-lived comm-aware backend session
(survives `--continue`, re-bootstraps its own receive path every restart) there
are two paths by who is spawning:

- **You are a Claude session (or headless)**: use `comm-spawn.sh` in workspace
  mode — the daemon + FE autostart give claude a clean env and a real attach.
  Do NOT hand-roll tmux panes for claude: they inherit your
  `CLAUDECODE`/`CLAUDE_CODE_*` env and, worse, claude's TUI in a never-attached
  pane exits cleanly with no error (silent failure).
- **Human at a shell**: `tmux new-session -s <name> -c <repo>
  ~/.local/bin/ccb` (`ccbe` for a Ship of Tools backend) — **no `-d`**: create and
  attach in one step. Claude never starts in a detached pane.

See "Starting peer sessions" in the **sot-session-start** skill, including the
handle-collision warning (never take a registry handle that already exists, even
stale-looking).

## Git worktrees (`spawn worktree`)

When work wants an **isolated full-repo checkout** — a parallel branch to
build/edit without disturbing the main tree (docs, a risky refactor, a second
agent on one repo) — use a git **worktree**. Two fixed conventions:

- **Location: a `worktrees/` folder in the repo's PARENT directory** — never a
  sibling of the repo, never inside it: `<repo-parent>/worktrees/<repo>-<descriptor>`.
  For `~/dev/ship-of-tools` that is
  `~/dev/worktrees/sot-docs`. (Repos parented directly under
  `~/projects/` land in `~/projects/worktrees/` by the same rule —
  e.g. `analysis-docs` off MyAnalysis.)
- **Branch off `main`**; the worktree dir + (if you spawn into it) the comm handle
  are `<repo-lowercase>-<descriptor>` — the descriptor names the WORKTREE, never
  the task (same rule as spawn naming above).

So **"spawn worktree"** means:

```sh
git -C ~/dev/ship-of-tools \
    worktree add ~/dev/worktrees/sot-docs -b docs/site main
```

To run an agent in it, `comm-spawn.sh <repo>-<desc> <worktree-path>` (a worktree
is just a repo path). Remove a stale one with `git worktree remove <path>`.

## On receiving `[name:repo] ...`

That is an inbound sot-comm message pasted into your prompt. If you are not yet
joined, run `comm-join.sh` first, then reply with `comm-send.sh @name "..."`.
**Local text output is not seen by other sessions** — you must reply via
`comm-send.sh`.

## Conventions (from agent-comm — keep them)

- **State goals, not orders.** The other session knows its own repo better than
  you. Describe the problem/goal; suggest a fix if you have one, but let them
  decide the implementation.
- **Anti-groupthink tags:** `[design]` `[question]` `[breaks]` `[challenge]`
  `[consensus]`. No `[consensus]` without a prior `[breaks]` in the thread.

## Notes

- Delivery is durable: if the recipient isn't reachable live, the message is
  queued to their inbox and they get it on `comm-poll.sh`. A send that couldn't
  deliver live says `queued to inbox (<host>)` — that is expected, not an error.
- Liveness is heartbeat-based (`comm-list.sh` shows live/stale). `poll`, `send`,
  and `join` all refresh your heartbeat.
- To poll on a schedule, use the `loop` skill: `/loop 5m comm-poll`.
