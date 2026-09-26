# Modes

A **mode** is a switchable root for the navigation tree. Every mode has the same
shape — a parent → current → children hierarchy shown as a collapsible outline —
and a hotkey swaps which tree fills it. **Cursor position is preserved per mode**
across switches, so jumping from Files to Modules and back lands you exactly where
you left each one.

```@raw html
<DemoLoop name="modules" caption="Modules mode: the cursor moves through a module's definitions, with docstring and source in the preview." />
```

Modes are a **planned** plugin surface: the design is a [`Mode`](@ref) subtype
with `tree_root` / `tree_children` / `preview_for` methods adding a new root, and
the core modes shipping as methods on that same type with no privileged path.
Today the nav roots are fixed in the frontend (Files, Modules, Sessions, Hosts):
Files, Sessions and Hosts are built in Rust, and only the Modules tree comes
from the Julia kernel. The mode-plugin seam is not yet wired.
See [The Dispatch ABI](../extend/abi.md) and [Writing a Mode Plugin](../extend/mode.md).

## The same shape everywhere

The window's navigation pane is a single collapsible tree: a parent context, the
current level, and its children, shown through nesting. What changes between modes
is *what* the tree enumerates and what the preview pane shows for the cursored node.

| Mode | Level 1 → Level 2 → Level 3 | Preview |
|------|-----------------------|---------|
| Project | Sections → contents → subitems | Rendered markdown / task detail |
| Files | Parent dir → current dir → contents | File at appropriate fidelity |
| Modules | Modules → types, functions, macros, submodules → constructors | Definition source + concept artifact |
| Types | Types → facets (fields/methods/sub) → members | Type def + meaning + data shape |
| Math | Concept areas → concepts → derivations/impls | LaTeX + implementing functions |
| Outputs | Recent runs → contents → artifacts | PNG / plot / JSON / MP4 |
| Agents | Tasks → timeline → step detail | Diff / live tail / message |

This table is the **conceptual mode set** — the design target. Not all of it is
built yet; see the status breakdown below.

## Built today vs planned

| Mode | Status |
|------|--------|
| Files | **Built.** Filesystem navigation with previews. |
| Modules | **Built, read-only.** Structural view derived from `JuliaSyntax.jl`. |
| Types | Planned (after phase 1). |
| Math | Planned (after phase 1). |
| Outputs | Planned (after phase 1). |
| Agents | **Partly here.** Multi-agent is real *today* — concurrent Claude Code sessions coordinated over the comm bus and surfaced in the Sessions view. What is pinned for later is *this dedicated mode*: an in-UI tasks → timeline → step-detail view for orchestrating them. |

The conceptual set above describes where modes are going. What you can switch
between *today* is the set of operational nav modes bound in the keymap, below.

## The nav modes bound today

The frontend binds four navigation modes. Each is a single-character chord that is
active **only in navigation focus** — in the pty panes, the editor, or a prompt,
those characters stay literal text.

| Key | Mode | What it roots the tree at |
|-----|------|---------------------------|
| `f` | Files | the project filesystem |
| `m` | Modules | modules and their definitions (types, functions, macros and submodules), read-only, from `JuliaSyntax.jl` |
| `s` | Sessions | workspaces — the projects this backend is hosting |
| `h` | Hosts | the remote hosts you can target |

Files and Modules are the two conceptual modes wired up so far. Sessions and Hosts
are **operational** modes — they navigate the deployment surface (which project,
which machine) rather than the concept hierarchy of a single project.

### Sessions mode (`s`)

Sessions mode lists the workspaces the backend knows about and lets you commit a
new one. The session picker uses three chords on the cursored directory:

| Chord | Action |
|-------|--------|
| `Enter` | Create a workspace with a Claude Code agent. |
| `Ctrl+Enter` | Create a workspace with a Codex agent. |
| `Shift+Enter` | Create a workspace with a plain shell and no agent. |

One backend daemon hosts one Julia kernel per workspace, routed by `workspace_id`,
so switching workspaces is fast and does not tear down the kernel — switching is
"like tmux windows in the same session." You can also cycle the active workspace
directly with `Shift+ArrowRight` / `Shift+ArrowLeft`.

#### Accounts

A session normally runs under the agent's default login. To spend a *different*
subscription — a team account, say, kept separate from your personal one — add
an account outside Ship of Tools first:

```sh
mkdir -p ~/.claude-auth/team
CLAUDE_CONFIG_DIR=~/.claude-auth/team claude
# then, inside that claude session: /login
```

Once that subdirectory exists it appears as a choice in the session picker
(`Tab` cycles it) — Ship of Tools discovers accounts, it never declares or
creates them, and it never stores a credential of any kind. A subdirectory
you haven't logged into yet still shows up and is still selectable — the
row's own pane runs the login on its first start. Sessions running under a
non-default account show a short `· <name>` suffix in the Sessions list so
you can tell at a glance which subscription each one spends.

An account folder holds only its own login and Claude's own per-account
runtime state (sessions, caches, and the like). Everything else — `CLAUDE.md`,
`settings.json` (hooks, permissions, model, status line), `agents`, `commands`,
`skills`, `plugins`, `output-styles`, per-project memory (`projects`), and
prompt history (`history.jsonl`) — is **shared with the default folder**: on
the first session in a new account, the daemon links each of those entries in
from `~/.claude`, so the account starts with the same instructions, hooks,
and history as the default login. An entry already present in the account
folder — created by you, or from an earlier session — is kept as is and
overrides the shared one; to use the shared one instead, remove the
account's own copy. User-scope MCP servers live with the per-account
state, so add them separately for each account you want to use them from.

An account name is lowercase letters, digits, `-`, and `_` only, and must
start with a letter or digit — `team`, `team-2`, and `a_b` all work,
`Team` and `../etc` do not.

Claude only, this release — Codex accounts are deferred, so a codex row
always runs the default login.

### [Hosts mode (`h`)](@id hosts-mode)

Hosts mode lists every daemon host the frontend is connected to. The frontend
dials every daemon host declared in `hosts.toml` at once, so each row shows
whether that host is connected or unreachable, tagged `[current]` for the host
you are working on and `[default]` for the first connection. `Enter` on a host
moves the Sessions view to that host's group; there is nothing to relaunch.

The launcher's primary host is the hub unless `SOT_HOST_NAME` (or `SOT_HOST`)
names another.

## Switching, focus, and layout keys

Mode switches share the keymap with pane focus and layout. The relevant chords:

| Chord | Action |
|-------|--------|
| `f` / `m` / `s` / `h` | switch nav mode (nav focus only) |
| `Shift+ArrowLeft` / `Shift+ArrowRight` | cycle the active workspace |
| `Ctrl+ArrowLeft/Right/Up/Down` | move pane focus (4-way, spatial) |
| `Alt+=` | maximize the focused pane; `Escape` restores |
| `Ctrl+j` / `Ctrl+t` / `Ctrl+m` | toggle the REPL / Terminal / Monitor drawers |
| `Ctrl+?` | show pane actions briefly; press again for the Help drawer |
| `F1` | open or close the Help drawer |

Because the mode keys are plain single characters, they are deliberately scoped to
navigation focus — the keymap matches them only when a nav pane is focused, so
typing `f` into the REPL or the editor inserts an `f`. The authoritative chord list
lives in [Keybindings](../ref/keybindings.md).
