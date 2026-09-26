# Agent Sessions

An agent session is a Claude Code or Codex process running in a workspace on
the **backend daemon**: you describe what you want in the agent pane, and it
reads code, writes code, and runs code in the workspace's [REPL](repl.md) to
get there. For the pane itself, see
[The agent pane](interface.md#The-Agent-Pane).

Each session runs Claude Code, Codex or a plain shell, and brings that agent's
own tools and context management. A session belongs to a workspace on the
backend, so it sits alongside project state and survives a frontend restart.
Several sessions run concurrently, one per workspace, across machines, and
message each other over the [inter-agent communication system](messaging.md).
You start and switch them from [Sessions mode](modes.md). A dedicated
**Agents mode** (tasks, timeline, step detail) is not built.

## What Ship of Tools adds to the agent

The installer gives each agent CLI a set of skills and hooks (listed under
[What the installer changes](@ref install-footprint)):

- **Skills** that call small command-line tools: `julia-repl` runs code in the
  session's REPL ([How agents use the REPL](@ref agents-repl)), `show-result`
  puts a file in your preview pane or opens a page in your browser, and
  `sot-comm` sends messages to other sessions. Claude Code gets all of them;
  Codex gets only `sot-comm` and `sot-session-start`, so Codex sessions have no
  REPL skill yet.
- **Hooks** that report the session's work state (working, blocked, done),
  which is what colours its row. See [Work-state colours](../concepts/work-state.md).

## Permission mode

Sessions created from the window start their agent with:

- **Claude Code**: `claude --permission-mode auto`.
- **Codex**: `codex --dangerously-bypass-approvals-and-sandbox
  --dangerously-bypass-hook-trust`, through the `ccx` launcher, which also
  marks the session's directory as trusted in `~/.codex/config.toml`.

Claude Code's auto mode is not a bypass. It runs without routine permission
prompts, but a separate classifier model reviews each action before it runs and
blocks anything that goes beyond your request, targets infrastructure it does
not recognise, or looks driven by hostile content; your own `ask` rules still
prompt. See Claude Code's
[permission modes](https://code.claude.com/docs/en/permission-modes).
Codex has no equivalent reviewed mode here: its flags turn approvals and the
sandbox off.

These flags are fixed: no setting or environment variable changes them. The
Claude Code flags are built into the daemon, and the Codex flags are in the
`ccx` script, which every update overwrites. Ship of Tools adds no permission
tiers or action gating of its own on top.

## See also

- [The REPL](repl.md) — the session the agent runs code in.
- [Running several agents](agents.md) — creating, switching and watching
  sessions.
