---
name: sot-install
description: Install/update Ship of Tools agent resources (Claude/Codex skills, sot-comm scripts) into ~/.claude, $CODEX_HOME, ~/.local/bin, ~/.sot-comm. Idempotent — run after pulling the repo. Activates for "install sot", "update sot comm", "sot install".
---

# sot-install

Sync Ship of Tools' agent-side resources from the package source into your
home dir. Wraps `ShipTools.install_comm()` — copies the comm scripts to
`~/.sot-comm/bin/`, Claude Code skills to `~/.claude/skills/`, and Codex
skills to `$CODEX_HOME/skills/` (default `~/.codex/skills/`). Idempotent:
running it again updates an existing install.

## Run this

```bash
julia --project=. -e 'using ShipTools; ShipTools.update_comm()'
```

Drop `--project=.` if Ship of Tools is in the global env instead of a local
checkout. This copies `comm/core/scripts/*` → `~/.sot-comm/bin/`, each
Claude/Codex skill's whole directory (so a skill's own `resources/`/
`references/` travels with it) into the respective skills dir, installs
launcher commands (`ccb`, `ccbe`, `ccx`) into `~/.local/bin/`, installs the
state-nav hooks, and stamps/checks the protocol version.

## After install

**Exit and restart Claude Code** — frontmatter changes need a restart; body
edits hot-reload.

On a Windows frontend box whose session identity still lives in a legacy
shared no-pane self-file (no recorded `root=`), the comm scripts' identity
check refuses to send until you re-join with an explicit name:
`comm-join.sh --name <your handle>` (a named join records the registry root
and rewrites the self-file). A plain Claude Code restart doesn't fix this;
the normal `/sot-session-start` bootstrap does, because it performs the
named join.

## Cross-machine note

One install covers every host sharing a home directory. On a
separate-filesystem machine, `git pull` the repo there and run this skill —
`comm-join.sh`'s protocol-version check warns loudly if a machine is out of
sync.
