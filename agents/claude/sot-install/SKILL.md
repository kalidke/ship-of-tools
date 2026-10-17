---
name: sot-install
description: Install or update Ship of Tools on this computer (Linux, macOS and Windows). Activates for "update sot", "update sot from the repo", "update Ship of Tools", "install sot", "sot install", "update sot comm". Runs the installer (the update entry) and reports its one verdict line; the agent comm and skills are refreshed as part of it.
---

# sot-install

"Update sot from the repo" means run the installer from the installed checkout and report its last line. Do not stop at
refreshing the comm scripts: that is one step the installer already takes.

## Update (Linux and macOS)

If `~/.local/share/sot/install.json` exists (use `--prefix <dir>` or `SOT_PREFIX` for another prefix):

```bash
bash ~/.local/share/sot/repo/current/scripts/install.sh
```

With no options it follows the channel the machine already runs, keeps the role the install recorded, and never moves
backwards. Read its **last line** and tell the human:

- `PASS: Ship of Tools <v> at <short> on this computer`: done.
- `ONE STEP LEFT: <step>`: the install is whole; give the human that step in those words (usually: quit the window and
  start it again).
- `FAIL: <check>: <detail>`: report the line. Do not retry blindly.
  - `daemon:` names rows with a live REPL and says when the Pluto server is running (a restart would end both), or says
    why the daemon could not be asked in full. Ask the human; run it again with `--restart-daemon` only on a yes.
  - `target:` and `direction:` are the human's to answer, never a version the agent picks. `target:` means the machine
    runs a build of the line (a `+` in its version): a pack of the line goes in with `--archive <folder>`, while a release
    named with `--version` is usually older and moves this computer back. `direction:` means the newest release is older
    than what runs here (or the direction could not be told): only a version the human names with `--version vX.Y.Z` goes
    back. Ask which they want.
  - `--archive` runs the install.sh of the checkout that made the pack (the folder `scripts/pack-build.sh` wrote there),
    not the installed one: `bash <that checkout>/scripts/install.sh --archive <folder>`.
  - `enroll:` the hub's hosts.toml omits this computer: run the command the line names on the hub, then run this again.

If a run stops half way, run it again: a repeat is a repair run. If the installed `install.sh` does not contain
`ONE STEP LEFT` it predates this entry. Read `version` in `~/.local/share/sot/install.json` first. A `+` in it is the
`target:` case above: ask the human. Otherwise follow the channel: a stable version takes the latest stable release
(`curl -fsSLI -o /dev/null -w '%{url_effective}' https://github.com/kalidke/ship-of-tools/releases/latest` ends in its
tag), a pre-release the newest published one (`curl -fsSL 'https://api.github.com/repos/kalidke/ship-of-tools/releases?per_page=1'`,
its first `tag_name`). Tell the human that version before running
`curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/<tag>/scripts/install.sh | bash -s -- --version <tag>`.
With no `install.json` this is a first install:
follow `docs/INSTALL-AGENT.md` in the checkout, or fetch it from the repo. A source checkout (no `install.json`, a `+`
in `sotd --version`) updates by `git pull` and a rebuild.

## Update (Windows)

If `%LOCALAPPDATA%\sot\install.json` exists (pass `-Prefix <dir>` for another prefix), run the installer from the
installed checkout. The versions path works from an ssh session too, where `repo\current` cannot be traversed:

```powershell
$p="$env:LOCALAPPDATA\sot"; $t=(Get-Content "$p\install.json" -Raw | ConvertFrom-Json).tag
powershell -NoProfile -ExecutionPolicy Bypass -File "$p\repo\versions\$t\scripts\install.ps1"
```

It reads the same way as the Unix one: no options follows the channel the machine already runs, never moves backwards,
and ends with exactly one line. Tell the human what it says:

- `PASS: Ship of Tools <v> at <short> on this computer`: done.
- `ONE STEP LEFT: <step>`: the install is whole, or the update is armed and waits for one thing only the human can do:
  quit Ship of Tools (choose Keep) and start it from the shortcut, start it at the desk, or install Julia or jq with the
  winget command the line names. Give the human the step in those words, then run the installer again to confirm.
- `FAIL: <check>: <detail>`: report the line. Do not retry blindly.
  - `daemon:` names rows with a live REPL and says when the Pluto server is running (a restart would end both), or says
    why the daemon could not be asked in full. Ask the human; run it again with `-RestartDaemon` only on a yes.
  - `target:` and `direction:` are the human's to answer: the agent does not choose a version. `target:` means the machine
    runs a build of the line (a `+` in its version): a pack of the line (`scripts\pack-build.ps1` in the checkout that
    built it) goes in with `-Archive <folder>`, while a release named with `-Version vX.Y.Z` is usually older and moves
    this computer back. `direction:` means the newest release is older than what runs here: only a version the human
    names goes back. Ask which they want.
  - `-Archive` runs the install.ps1 of the checkout that made the pack, not the installed one:
    `powershell -NoProfile -ExecutionPolicy Bypass -File <that checkout>\scripts\install.ps1 -Archive <folder>`.

A run that names what to install (`-Version`, `-Archive`) pins the install: automatic updates stay off until a run that
names no version. If a run stops half way, run it again: a repeat is a repair run. With no install here, a first
install is `docs/INSTALL-AGENT.md` (fetch `https://raw.githubusercontent.com/kalidke/ship-of-tools/<tag>/scripts/install.ps1`
with `curl.exe` and run it with `-Version <tag>`). A source checkout (no `install.json`, a `+` in `sotd.exe --version`)
updates by `git pull` and a rebuild.

## Only the agent comm and skills

To refresh just the agent-side resources (the comm scripts in `~/.sot-comm/bin/`, the Claude Code skills in
`~/.claude/skills/`, the Codex skills in `$CODEX_HOME/skills/`, the launcher commands `ccb` and `ccx` in
`~/.local/bin/`, the state-nav hooks), from a checkout:

```bash
julia --project=. -e 'using ShipTools; ShipTools.update_comm()'
```

Drop `--project=.` if Ship of Tools is in the global env instead of a local checkout. It is idempotent.

## After install

**Exit and restart Claude Code**: frontmatter changes need a restart; body edits hot-reload.

On a Windows frontend box whose session identity still lives in a legacy shared no-pane self-file (no recorded
`root=`), the comm scripts' identity check refuses to send until the session joins again. A plain Claude Code restart
doesn't fix this; `/sot-session-start` does: its join records the registry root and rewrites the self-file.

## Cross-machine note

One install covers every host sharing a home directory. On a separate-filesystem machine, run the update there:
`comm-join.sh`'s protocol-version check warns loudly if a machine is out of sync.
