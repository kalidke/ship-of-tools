# Update Ship of Tools: the runbook for a coding agent

You are a coding agent (Claude Code, Codex or another) with a shell on one computer, asked to update Ship of Tools on
that computer. Follow these steps in order. Where a step says to ask the person, ask and wait for the answer. Three
rules hold throughout:

- Never pick a version yourself.
- Never answer a question the installer asks on its own. If a run shows "Restart the daemon? ... [y/N]", answer N and
  ask the person (step 4 says how a yes is given).
- Never quit, end or kill a Ship of Tools window or any of its processes yourself. Ask the person.

The steps hold for every install from 0.6.6-rc1 to 0.7.0.

## 1. Which computer, and what is installed

If the environment variable `SOT_WORKSPACE_ID` is set, you are running inside a Ship of Tools row, and a row can run on
another computer than the one the person sits at. Tell the person the name `hostname` prints and ask whether that is
the computer they want updated. If it is not, stop: an agent on that computer has to do it.

Then read the install record. Linux and macOS:

```bash
cat ~/.local/share/sot/install.json
```

Windows (PowerShell):

```powershell
Get-Content "$env:LOCALAPPDATA\sot\install.json"
```

Tell the person the `version` it records, then:

- **It records `"pinned": true`.** Someone kept this computer on that version on purpose. Ask: "This computer is pinned
  at <version>. Updating removes the pin and turns its automatic updates back on. Go on?" Go on only on a yes.
- **The version is below 0.6.6-rc1** (0.6.5 or older), **or, on Windows, below 0.6.6-rc9.14.** This runbook has not been
  tried from it. Tell the person so, and go on only if they say to.
- **The install lives under another folder** (the person set `SOT_PREFIX` or `--prefix`): use that folder here, and add
  `--prefix <folder>` (Windows: `-Prefix <folder>`) as step 3 says.

If the file does not exist, this computer has no release install. Ask the person how Ship of Tools runs here:

- **They build it from a source checkout, and the computer has a Rust toolchain** (`cargo --version` answers): it
  updates by `git pull` and a rebuild in that checkout, which is theirs to run. This runbook ends here.
- **Windows, and the Ship of Tools window will not start** (its launcher says "no sotd binary found"; often there is no
  `cargo`, and only an old `sot.exe` in `%LOCALAPPDATA%\sot\bin`): a release install over it is the update, and this
  is the one procedure for that case. Tell the person it replaces the binaries in `%LOCALAPPDATA%\sot\bin` and the
  Desktop shortcut with the release's, and leaves the source checkout's folder as it is; go on only on a yes. If that
  window used to show rows of other computers, ask for the hub's ssh alias (the name `ssh <alias>` reaches the hub by)
  and add `-Hub <alias>` as step 3 says. If they do not know it, go on without it: the run passes, and its output says
  how to add the hub later.
- **Linux or macOS with no working build, or nothing installed at all:** this runbook does not cover it. A first install
  is `docs/INSTALL-AGENT.md`; follow it with the person.

## 2. Ask before you run it

First see whether this computer is one of a group. Run `~/.local/share/sot/bin/sotd topology plan` (Windows:
`& "$env:LOCALAPPDATA\sot\bin\sotd.exe" topology plan`). If it prints a `self <name>` line and a `hub <name>` line,
this computer belongs to a group that shares rows and mail through that hub, and when the two names are the same, this
computer is the hub. If it prints an error or no `hub` line, there is no group. A computer on 0.6 and one on 0.7 carry
no mail between them and do not show each other's rows, so the hub moves first and the others in the same sitting:

- On the hub: tell the person the other computers of the group need the same update right after.
- On another computer of the group: ask whether the hub is updated already. If the person does not know, they may still
  go on: this computer then shows no rows or mail of the group until the hub moves too.
- On a home shared by several computers, an update on one replaces the binaries for all of them, but each computer's
  daemon keeps the old one until it restarts: run the update on each computer anyway.

Then, when the version from step 1 starts with `0.6`, ask the person this one question, in your own words, and go on
only on a yes:

> Updating from 0.6 to 0.7 ends every agent running in Ship of Tools on this computer once, and everything those agents
> started in their rows (a build, a test, a long job). On Windows the open Ship of Tools window closes and opens again
> by itself. It happens during the update, unless a window holds the daemon on 0.6.6-rc9.12 or later, and then when
> the window is next started. Each row then starts again with a new conversation, and `/resume` in a row brings an
> earlier one back. Is now a good time?

If you run inside a row (step 1), add that this ends your own session too, most likely before you can read the
result, while the update itself carries on to its end: when the row shows a new conversation (it begins by running
`/sot-session-start` on its own), the person types `/resume` in it and picks this conversation, and you then run step 3
once more to see the result. That is what a run showed: from 0.6.6 under its unit, the agent in the row was ended
mid-run, the update completed, and its repeat of step 3 ended `PASS`. In a Codex row there is no such way back: say so,
and that its new conversation starts afresh.

## 3. Run the update

Linux and macOS: run this in one shell call.

```bash
( cd ~ || exit 1
  tag=$(curl -fsSLI -o /dev/null -w '%{url_effective}' https://github.com/kalidke/ship-of-tools/releases/latest); tag=${tag##*/}
  case "$tag" in v[0-9]*) echo "newest release: $tag" ;; *) echo "STOP: cannot reach GitHub"; exit 1 ;; esac
  f=$(mktemp) || exit 1
  if curl -fsSL -o "$f" "https://raw.githubusercontent.com/kalidke/ship-of-tools/$tag/scripts/install.sh"; then
    SOT_INSTALL_TAG="$tag" bash "$f"; rc=$?
  else
    echo "STOP: could not fetch the installer of $tag"; rc=1
  fi
  rm -f "$f"; exit "$rc"
) 2>&1 | tee ~/sot-update.log
```

An option from step 1 or step 4 goes right after `bash "$f"`, for example `bash "$f" --restart-daemon`.

Windows: save these lines, exactly, as `sot-update.ps1` in your home folder with your file-writing tool. An option from
step 1 or step 4 goes into the `$opts` line, for example `$opts = @('-Hub', 'myhub')`.

```powershell
# sot-update.ps1: the update of docs/UPDATE-AGENT.md step 3. Options go in $opts, for example @('-Hub', 'myhub').
$opts = @()
$log = Join-Path $HOME 'sot-update.log'
[IO.File]::WriteAllText($log, '')
function Say($line) { "$line"; [IO.File]::AppendAllText($log, "$line`r`n") }
Set-Location $HOME
$tag = "$(curl.exe -fsSLI -o NUL -w '%{url_effective}' https://github.com/kalidke/ship-of-tools/releases/latest)".Split('/')[-1]
if ($tag -notmatch '^v[0-9]') { Say 'STOP: cannot reach GitHub'; exit 1 }
Say "newest release: $tag"
$f = Join-Path $env:TEMP "sot-install-$tag.ps1"
curl.exe -fsSL -o $f "https://raw.githubusercontent.com/kalidke/ship-of-tools/$tag/scripts/install.ps1"
if ($LASTEXITCODE -ne 0) { Say "STOP: could not fetch the installer of $tag"; exit 1 }
$env:SOT_INSTALL_TAG = $tag
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $f @opts 2>&1 | ForEach-Object { Say $_ }
exit $LASTEXITCODE
```

Then run it. From PowerShell:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File "$HOME\sot-update.ps1"
```

From Git Bash (the shell Claude Code uses on Windows):

```bash
powershell -NoProfile -ExecutionPolicy Bypass -File ~/sot-update.ps1
```

Use exactly these. Each moves to your home folder, so the installer takes no git checkout of yours for its own (run
from inside a Ship of Tools checkout, it would add its push guard to that checkout too). Each asks GitHub for its newest
release (0.7.0 when this was written), prints it, fetches that release's own installer and runs it; `SOT_INSTALL_TAG`
tells the installer which release it is and does not pin the install. Nothing either sets stays in your shell: the Linux
lines run in a subshell, the Windows ones in a PowerShell process of their own. Never add `--version` (Windows:
`-Version`) unless the person named that version: naming one pins the install and turns its automatic updates off.

Both write everything they print to `sot-update.log` in your home folder. A run takes from under a minute to many
minutes (the first run on a computer prepares Julia), longer than a tool call's default timeout: run it in the
background, or with the longest timeout your tool allows, and read the last line when it ends. If your session ended
during the run and you are back in this conversation, read `sot-update.log`: when it ends with one of the lines of step
4, act on that line; when it stops mid-run (your session was ended before the installer printed its last line), run step
3 again: on an updated computer it ends `PASS`.

## 4. Act on the last line

It ends with exactly one line.

| Last line | What you do |
|-----------|-------------|
| `PASS: Ship of Tools <version> at <commit> on this computer` | Go to step 5. What follows the first `;` is a checklist, and the line is a pass whatever it says; "pushes refused until a valid publish-list.txt exists" there concerns only the install's own copy of the repository. |
| `ONE STEP LEFT: <step>` | Tell the person the step in its own words (usually: quit Ship of Tools and start it again, since the window still runs the old version). Do not do it yourself. If you are in a row, add that doing it ends your session, and that `/resume` in this row brings it back. When it is done, go to step 5. |
| `FAIL: daemon: ...`, with the line above it ending "Ask whoever uses this computer, then re-run with --restart-daemon" (Windows: "then run this again with -RestartDaemon") | A restart would end a live Julia REPL or Pluto notebook (their variables are lost). Nothing was changed. Ask the person, naming the rows the line names, if it names any. Only on a yes, run step 3 again with `--restart-daemon` (Windows: `-RestartDaemon` in `$opts`). |
| any other `FAIL: daemon: ...` (for example "no daemon answers", or "did not answer as <version>") | The update was written, and the daemon did not come up as it should. Report the line to the person and run step 3 once more. |
| `FAIL: target: ...` | This computer runs a build of the line (a `+` in its version), not a release. Nothing was changed. Ask the person what they want: a pack of the line, or the release the line names. Change nothing yourself. |
| `FAIL: direction: ...` | This computer runs a newer pre-release than the newest release (for example 0.7.1-rc.3 against 0.7.0). Nothing was changed, and there is nothing to update. Tell the person. Only if they name a version to go back to, run step 3 with `--version <that tag>` (Windows: `-Version`), which pins the install. |
| `FAIL: enroll: ...` | The hub's `hosts.toml` does not list this computer. Tell the person the line: the hub's owner runs the command it names on the hub; then run step 3 again. |
| `FAIL: <anything else>` | Report the line to the person. If the run was cut short (a lost connection, an interrupt), run step 3 once more: a repeat repairs. |
| `STOP: ...` or a `curl:` error | This computer could not reach GitHub, or could not fetch the installer. Nothing was changed. Tell the person, and run step 3 again once it can. |
| none of these | If your session ended during the run, read `sot-update.log`: act on its last line by this table, or, when it stops mid-run, run step 3 again. |

## 5. Prove it worked

Run step 3 once more, exactly as before. On an updated computer it is a repair run: it changes nothing and ends
`PASS: Ship of Tools <the new version> at <commit> on this computer`. If it ends any other way, act on its last line by
step 4. Then:

- `~/.local/share/sot/bin/sotd --version` (Windows: `& "$env:LOCALAPPDATA\sot\bin\sotd.exe" --version`) names the same
  version.
- After an update from 0.6, every row was started again with a new conversation, which begins by running
  `/sot-session-start`. Tell the person that `/resume` in a Claude row lists that row's earlier conversations and brings
  the one they pick back.
- Ask the person to quit and start again every open Ship of Tools window, on every computer of the group. Never quit or
  end a window yourself.

Tell the person the version the last line names, and anything from steps 2 and 4 that is still theirs to do.
