---
name: sot-install
description: Install or update Ship of Tools agent resources (Claude Code skills, Codex skills/hooks, and sot-comm scripts) from the package into ~/.claude, $CODEX_HOME, ~/.local/bin, and ~/.sot-comm. Idempotent. Run after pulling the Ship of Tools repo on a machine to close version skew. Activates for "install sot", "update sot comm", "sync sot", "sot install", "reinstall comm".
---

# sot-install

Sync Ship of Tools' agent-side resources from the package source into your home dir.
This wraps `ShipTools.install_comm()` — copies the comm scripts to
`~/.sot-comm/bin/`, Claude Code skills to `~/.claude/skills/`, and Codex skills
to `$CODEX_HOME/skills/` (default `~/.codex/skills/`). Idempotent:
running it again updates an existing install.

## Run this

From the Ship of Tools repo checkout:

```bash
julia --project=. -e 'using ShipTools; ShipTools.update_comm()'
```

If Ship of Tools is in the global env instead of a local checkout, drop `--project=.`.

This:
1. Copies `comm/core/scripts/*` → `~/.sot-comm/bin/`
2. Copies the Claude skill adapters (`sot-comm`, `sot-install`,
   `sot-session-start`, `sot-be-session-start`, `sot-fe-session-start`,
   `project-log`, and others) → `~/.claude/skills/`, each skill's whole
   directory (not just `SKILL.md`), so a skill's own `resources/` (e.g.
   `project-log`'s vendored templates) travels with it
3. Copies Codex skills (`sot-comm`, `sot-session-start`,
   `sot-be-session-start`, `sot-fe-session-start`) → `$CODEX_HOME/skills/`
   (default `~/.codex/skills/`)
4. Installs launcher commands (`ccb`, `ccbe`, `ccx`) → `~/.local/bin/`
5. Installs state-nav hooks/plugin wiring for Claude Code and Codex
6. Stamps/checks the protocol version

## After install

**Exit and restart Claude Code** to pick up new or changed skills (frontmatter
changes need a restart; hot-reload covers body edits).

**On a Windows frontend box** — any session whose identity lives in the
shared no-pane self-file (one file shared by every no-pane shell on the
box) — re-join with an explicit name after installing:
`comm-join.sh --name <your handle>`, never a bare join. The refreshed comm
scripts' stricter identity check rejects the shared self-file until a named
join corroborates it with a matching project root recorded in the sot-comm
registry, and restarting Claude Code does not fix this. Until you re-join,
sends fail with a clear refusal while receive keeps working.

## Launchers

Both live in `~/.local/bin` (which must be on PATH) and run
`claude --permission-mode auto` with a bootstrap skill baked in as the
first-turn prompt. Use the bare form for a fresh session, `--continue` to resume
and re-arm comm (harness Monitors don't survive `claude --continue`); extra flags
are forwarded to `claude` ahead of the skill.

- **`ccb`** — *any* backend session. Bakes in `/sot-session-start` (the generic
  receive-bootstrap: listener + inbox Monitor + wake proof + catch-up). Use this
  for your non-Ship of Tools project sessions so they receive cross-session messages.
- **`ccbe`** — *Ship of Tools* backend session. Bakes in `/sot-be-session-start`, which
  runs the generic bootstrap then adds Ship of Tools checks (frontend reachability, FE
  count, `.claude-bus`). The backend analog of the FE's ADR-0017 auto-resume.

## Cross-machine note

In an optional multi-host / shared-home deployment, one install covers every host
sharing that home directory. On a separate-filesystem machine, `git pull` the
Ship of Tools repo there and run this skill to install locally — the
protocol-version check on `comm-join.sh` warns loudly if a machine is out of sync.
