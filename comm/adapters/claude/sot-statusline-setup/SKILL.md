---
name: sot-statusline-setup
description: Installs the Claude Code statusline (Windows, Linux, or macOS) by copying this skill's maintained scripts into ~/.claude, never regenerating them. Use for "/sot-statusline-setup", "set up the statusline", "status line setup", "fix my statusline". Directly invoked, one-shot.
---

# sot-statusline-setup

Copies the maintained statusline scripts into `~/.claude` (or
`%USERPROFILE%\.claude`) and points `settings.json` at them. The scripts in
this skill's own directory are the single source of truth — copy them as
files, never retype or regenerate them. A regenerated Windows script is what
breaks the statusline each time the generic Claude Code statusline setup
runs instead of this one.

> **Canonical copy:** this file and its payload scripts (repo
> `.claude/skills/`). The install payload
> `comm/adapters/claude/sot-statusline-setup/` is a byte-for-byte copy —
> edit HERE, then sync the payload and re-run `/sot-install` to close skew.

## 1. Detect OS
`win32` / `linux` / `darwin`. Determines which payload file you copy and how
you edit `settings.json`.

## 2. Locate the payload
The harness reports "Base directory for this skill: `<path>`" when this
skill is invoked — that directory holds `statusline.bat`, `statusline.ps1`,
and `statusline.sh` next to this file (deployed copy:
`~/.claude/skills/sot-statusline-setup/`). Copy from there; do not
transcribe the content.

## 3. Shared-home check — before writing anything
If `~/.claude/settings.json` already sets `statusLine.command` to a path
containing `statusline-session-model.sh`, a shared-home deployment already
has a maintained statusline. Leave it alone, tell the user, and stop.

## 4. Windows
Copy `statusline.bat` and `statusline.ps1` to `%USERPROFILE%\.claude\`.
`statusline.ps1` is ASCII-only literals — Windows PowerShell 5.1 reads
`.ps1` as CP1252, so a non-ASCII literal breaks the parse. Keep it
ASCII-only if you ever touch it.

Merge into `%USERPROFILE%\.claude\settings.json`, preserving other keys —
a short PowerShell `ConvertFrom-Json` / `ConvertTo-Json -Depth 10` merge,
not a hand-edit:
```json
{ "statusLine": { "type": "command", "command": "C:/Users/<you>/.claude/statusline.bat" } }
```
**Forward slashes, always.** Claude Code runs the statusLine command through
`SHELL -c` (the Ship of Tools Terminal drawer inherits `SHELL=/bin/bash.exe`
from its MINGW64 launch chain), and a backslash path gets mangled by bash
(`C:\Users\...` → `C:Users...`, exit 127) — the statusline then silently
never renders, no error, no output. Forward slashes survive both `bash -c`
and `cmd /c`.

## 5. Linux / macOS
Copy `statusline.sh` to `~/.claude/statusline.sh`, `chmod +x` it. Requires
`jq` + `awk` (both ubiquitous).

Merge into `~/.claude/settings.json` with `jq`, preserving other keys:
```json
{ "statusLine": { "type": "command", "command": "/home/<you>/.claude/statusline.sh" } }
```
Use the **absolute path**. The forward-slash gotcha is Windows-only —
POSIX paths are native here, no `bash -c` mangling.

## 6. Verify
Pipe sample JSON through the exact command now in `settings.json` and
confirm it prints two colored lines and exits 0:
```sh
echo '{"model":{"display_name":"Opus 4.8"},"session_id":"abc","version":"x","workspace":{"current_dir":"'$PWD'"},"context_window":{"total_input_tokens":120000,"total_output_tokens":34000},"cost":{"total_cost_usd":0.42}}' \
  | bash -c "<the exact command from settings.json>"
```
Claude Code hot-reloads `statusLine` from `settings.json` — no restart
needed.
