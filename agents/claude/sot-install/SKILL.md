---
name: sot-install
description: Install or update Ship of Tools on this computer (Linux, macOS and Windows). Activates for "update sot", "update sot from the repo", "update Ship of Tools", "install sot", "sot install", "update sot comm". An update follows the one update runbook, docs/UPDATE-AGENT.md; a first install follows docs/INSTALL-AGENT.md.
---

# sot-install

This skill keeps no update procedure of its own. Ship of Tools has one, a runbook written for a coding agent, and it
decides what this skill cannot: which computers to update first (every daemon a window dials moves before that
window), what to ask the person, and what each last line of the installer means.

## Update

For "update sot", "update sot from the repo", "update Ship of Tools" or "update sot comm", on any system, fetch the
runbook, read it whole, and follow it from its first line. Run nothing else, and no installer command from memory or
from an installed checkout: the runbook's commands are to be used exactly as written.

```bash
curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/UPDATE-AGENT.md
```

On Windows from PowerShell, `curl.exe -fsSL` fetches the same address. If the tool that fetched it gave you a summary,
fetch it again: a summary drops the exact commands.

## First install

With no install on this computer, follow `docs/INSTALL-AGENT.md` in the checkout, or fetch it from
`https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/INSTALL-AGENT.md`.

## Only the agent comm and skills

To refresh just the agent-side resources (the comm scripts in `~/.sot-comm/bin/`, the Claude Code skills in
`~/.sot-comm/claude-layer/`, which a row or `ccb` loads and a plain `claude` does not, the Codex skills in
`$CODEX_HOME/skills/`, the launcher commands `ccb` and `ccx` in `~/.local/bin/`), from a checkout:

```bash
julia --project=. -e 'using ShipTools; ShipTools.update_comm()'
```

Drop `--project=.` if Ship of Tools is in the global env instead of a local checkout. It is idempotent.

## After an update

**Exit and restart Claude Code**: frontmatter changes need a restart; body edits hot-reload.

On a Windows frontend box whose session identity still lives in a legacy shared no-pane self-file (no recorded
`root=`), the comm scripts' identity check refuses to send until the session joins again. A plain Claude Code restart
doesn't fix this; `/sot-session-start` does: its join records the registry root and rewrites the self-file.
