# Running several agents

Ship of Tools is built for more than one agent at a time. Every session is a
**row** in Sessions mode: a project directory with its own Julia REPL and an
agent pane running **Claude Code**, **Codex**, or a bare shell (see
[Hosts, sessions, REPLs](@ref session-model)). You move between rows
from the keyboard, and each row's colour tells you whether it needs you.

```@raw html
<DemoLoop name="sessions" caption="Demo session rows changing colour as a script sets their work states: green working, red waiting for your answer, purple waiting on a job, blue done, gray idle." />
```

## Start a session

Press **`s`** for Sessions mode and pick **`[+ create new]`**; the steps are in
[Start an agent session](@ref start-session).

The daemon starts the agent in its own supervised **capsule**, so it keeps
running when you switch rows, close the frontend, or lose the connection (see
[Sessions and persistence](../concepts/sessions.md)).

## Move between sessions

| Key | Action |
|-----|--------|
| `s` | Sessions mode — every row on every connected host, grouped by host |
| `Shift+ArrowRight` / `Shift+ArrowLeft` | cycle the active session from anywhere |
| `Ctrl+Arrow` | move focus between panes inside the session |

Switching rows is a frontend state change: it never restarts a session's
REPL, so the REPL holding `x = 5` still holds it when you come back.

## Read the colours

Each row is coloured by the agent's current work state — green while it works,
red when it is blocked waiting for your answer, purple while it waits on a peer or a long
job, blue when it has finished a turn you have not looked at, gray when idle.
Scan the list instead of tabbing through each pane. See
[Work-state colours](../concepts/work-state.md).

## One worktree per agent

Parallel agents on one repository should not share a checkout. The bundled
`worktree` skill (`/worktree` in a Claude Code session, backed by
`comm-worktree-new.sh <short>`) creates a
git worktree next to the repo and spawns a new session bound to it:

- directory `<repo-parent>/worktrees/<repo>-wt-<short>` (never inside the repo),
- branch `wt/<short>` off the current `HEAD`,
- a session labelled `<repo>-wt-<short>`, listed next to the parent row.

`/worktree status` shows the family and whether each worktree is ready to clean
up, `/worktree sync` reminds the parent and worktree sessions to sync, and
`/worktree clean` tears a finished one down. The same scripts
(`comm-worktree-new.sh`, `-status.sh`, `-sync.sh`, `-clean.sh`) live in
`~/.sot-comm/bin` for use from any shell.

## Hand work to a new agent in another repo

`comm-spawn.sh <repo-path> --task "..."` starts a new agent session on another
package and has it report back to you over the comm relay. Combined with
[agent messaging](messaging.md), one session can fan work out to several
others and collect their answers.

## A separate subscription per session

A session normally runs under the agent's default login. To spend a different
Claude subscription — a team account, say — create an account folder once
outside Ship of Tools:

```sh
mkdir -p ~/.claude-auth/team
CLAUDE_CONFIG_DIR=~/.claude-auth/team claude
# then, inside that claude session: /login
```

It then appears as a choice in the session picker (`Tab` cycles it), and rows
running under it show a `· team` suffix. Ship of Tools discovers accounts; it
never creates them or stores a credential. Details in [Modes](modes.md#Accounts).
