# Update Ship of Tools: the runbook for a coding agent

You are a coding agent (Claude Code, Codex or another) with a shell on one computer, asked to update Ship of Tools on
that computer. If the tool that fetched this page gave you a summary, fetch it again with
`curl -fsSL https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/UPDATE-AGENT.md` and read it whole: the
commands below must be used exactly as written. If you have a skill named `sot-install`, or any other procedure for
updating Ship of Tools, do not use it for this task: this page replaces it.

Follow these steps in order. Where a step says to ask the person, ask and wait for the answer. Four rules hold
throughout:

- Never pick a version yourself.
- Never answer a question the installer asks on its own. The commands in step 3 keep it from asking; if a run still
  shows "Restart the daemon? ... [y/N]", answer N and ask the person (step 4 says how a yes is given).
- Never quit, end or kill a Ship of Tools window, its daemon or a row yourself. Ask the person. (Your own step 3 run is
  yours to stop, as step 3 says.)
- Change only this computer. Step 2 reads what the daemons of other computers run, and that is all you do on them: an
  agent on each computer updates it, with that person's yes.

An agent running these steps inside a Ship of Tools row was run only on Linux, with stand-ins (step 2 says what).

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

From Git Bash on Windows, `%LOCALAPPDATA%\sot` is `$LOCALAPPDATA/sot`, here and in every later step: for example
`cat "$LOCALAPPDATA/sot/install.json"` and `"$LOCALAPPDATA/sot/bin/sotd.exe" --version`.

Tell the person the `version` it records, then:

- **It records `"pinned": true`.** Someone kept this computer on that version on purpose. Ask: "This computer is pinned
  at <version>. Updating removes the pin and turns its automatic updates back on. Go on?" Go on only on a yes.
- **The version is below 0.6.6-rc1** (0.6.5 or older), **or, on Windows, below 0.6.6-rc9.14.** This runbook has not been
  tried from it. Tell the person so, and go on only if they say to.
- **The install lives under another folder** (the person set `SOT_PREFIX` or `--prefix`): use that folder here, and add
  `--prefix <folder>` (Windows: `-Prefix <folder>`) as step 3 says.

If the file does not exist, this computer has no release record. Ask the person how Ship of Tools runs here:

- **They build it from a source checkout, and the computer has a Rust toolchain** (`cargo --version` answers): it
  updates by `git pull` and a rebuild in that checkout, which is theirs to run. This runbook ends here.
- **Windows, `%LOCALAPPDATA%\sot\bin\sotd.exe` exists and the window starts:** the record alone is missing. Use the
  version `sotd.exe --version` prints in place of the record's, and go on at step 2; the update writes the record again.
- **Windows, and the Ship of Tools window will not start** (its launcher says "no sotd binary found"; often there is no
  `cargo`, and only an old `sot.exe` in `%LOCALAPPDATA%\sot\bin`): a release install over it is the update, and this
  is the one procedure for that case. Tell the person it replaces the binaries in `%LOCALAPPDATA%\sot\bin` and the
  Desktop shortcut with the release's, and leaves the source checkout's folder as it is; go on only on a yes. If that
  window used to show rows of other computers, this computer is in a group and is not its hub: ask for the hub's ssh
  alias (the name `ssh <alias>` reaches the hub by). `-Hub` needs ssh to the hub that asks for no password: check with
  `ssh -o BatchMode=yes <alias> true`, and when that succeeds add `-Hub <alias>` as step 3 says. When the person does
  not know the alias, or the check fails, go on without it: the run passes, and its output says how to add the hub
  later.
- **Linux or macOS with no working build, or nothing installed at all:** this runbook does not cover it. A first install
  is `docs/INSTALL-AGENT.md`; follow it with the person.

## 2. Daemons before windows, then ask before you run it

A Ship of Tools window works only with a daemon that speaks its protocol, and it dials the daemon on this computer and
the daemon on every other computer its group lists. Inside one release line the protocol holds (0.7.0 and the 0.7.1
candidates speak the same one); between lines it does not. A 0.6 daemon and a 0.7 window refuse each other, in either
direction, and the window then shows that computer as refused ("the backend there predates the link; update it", for a
0.6 daemon reached over ssh) with none of its rows. (A 0.6.6 candidate before rc9.13 speaks a third protocol.) So
daemons move before windows: a computer's window moves to a release only when every daemon it dials already runs that
release's line, and the computer with the window goes last. Do this part first, before the question further down and
before step 3 changes anything.

### Daemons before windows

Read the newest release, the one step 3 installs. Its line is its first two numbers: `v0.7.0` is line 0.7.

```bash
curl -fsSLI -o /dev/null -w '%{url_effective}\n' https://github.com/kalidke/ship-of-tools/releases/latest
```

On Windows, from PowerShell (Git Bash takes the line above as written):

```powershell
curl.exe -fsSLI -o NUL -w '%{url_effective}' https://github.com/kalidke/ship-of-tools/releases/latest
```

It prints a URL ending in the tag, such as `.../releases/tag/v0.7.0`. If it prints anything else, tell the person you
cannot reach GitHub and change nothing.

Then read which daemons the group has and what each one runs. Both commands only read; `status` opens one short
connection to each daemon the plan lists:

```bash
~/.local/share/sot/bin/sotd topology plan
~/.local/share/sot/bin/sotd status
```

On Windows, from PowerShell:

```powershell
& "$env:LOCALAPPDATA\sot\bin\sotd.exe" topology plan
& "$env:LOCALAPPDATA\sot\bin\sotd.exe" status
```

- `plan` says `no hosts.toml at`: there is no group file here, but the window can still dial one daemon on another
  computer, the one an install made with `--backend <alias>` names. Look for that alias: the line `export SOT_HOST="..."`
  in `~/.local/bin/sot-launch` (Linux and macOS) and the `"hub"` value of the record (any system, when it is not
  empty).

  ```bash
  grep '^export SOT_HOST=' ~/.local/bin/sot-launch; grep '"hub"' ~/.local/share/sot/install.json
  ```

  - **There is an alias.** This computer and the one the alias names are a pair with no group file, and the pair cannot
    move to 0.7 as it is: a 0.7 window dials only the hosts of a group file kept on its hub, and the installer on this
    computer ends `FAIL: enroll`. Updating either computer, or both, leaves the window with no host or with a daemon it
    cannot use. Stop and change nothing; tell the person this, with the names filled in:

    ```text
    Not updated, on purpose. <laptop> and <server> are a pair with no group file: the window on <laptop> reaches the
    daemon on <server> by --backend, and Ship of Tools 0.7 needs <server> to declare a group first. Changing either
    computer now would leave the window with no host. Nothing was changed. To move the pair: on <server> write
    ~/.config/sot/hosts.toml holding the three blocks below (each name is what `hostname` prints, first part, lower
    case; <server>'s must also be the name <laptop> reaches it by in ssh), on <laptop> run
    `~/.local/share/sot/bin/sotd topology sync --hub <server>`, then send the same message again on <server> and on
    <laptop>, <server> first. Until then do not restart the window or the daemon on either computer: a restart may
    apply an update that is already staged.

    hub = "<server>"
    [host.<server>]
    daemon = true
    [host.<laptop>]
    frontend = true
    ```

  - **No alias, and a window here.** The only daemon this window uses is this computer's own, which the update moves:
    go on to "The one question". When the record says `"daemon": false`, ask the person which computer holds the daemon,
    and stop.
  - **No alias, and no window here** (the record says `"frontend": false`). Ask: "Does a window on another computer
    reach this computer's daemon by `--backend`?" On a yes, the same stop, with this computer as the server. On a no, go
    on to "The one question".
- `plan` fails any other way (a bad `hosts.toml`, no `self` line): show the person its words and wait.
- `plan` prints `self`, `hub` and `dial` lines: the group's daemons are the `status` table's rows whose DECLARED column
  holds `daemon`, except the row named on the `self` line, whose daemon this update moves. If `status` printed an error
  in place of a table, those daemons are unknown. For each of those rows read the DAEMON column:

| DAEMON column of the row | What that daemon runs |
|---|---|
| `<version> up (host: ...)` | that version |
| `unreachable: ... protocol mismatch: backend <version> (protocol <n>) vs frontend ...` | that version: it answered and refused this computer's program, so it is on another line |
| `unreachable: ... Usage: sotd stdio-bridge --label ...` | 0.6.6-rc1 to rc9.8: on a lower line than any release |
| any other `unreachable: ...`, or `not dialable here` | unknown: it is down, or ssh to the hub fails, or this install is older than rc9.13, whose plan names `tcp:` addresses nothing listens on |

A version is on the release's line when its first two numbers equal the release's (a pre-release of the line counts).
Does this computer run a window? Look at the row named on the `self` line: when its DECLARED column holds `frontend`,
it does; when it does not, this computer is a daemon only, with no window here. (The record is one per home, and
several computers can share a home, so it cannot say.)

**A window here.** Decide by the rows:

- **Every one is on the release's line.** The daemons are ready. Go on to "The group".
- **One is on a lower line, or unknown.** Stop: do not run step 3, and nothing has changed on this computer. Tell the
  person this, with the names and versions filled in, in these words:

  ```text
  Not updated. This computer's window uses the daemons on <A> (<version>) and <B> (<version>), which are older than
  <release>, and a window on <release> cannot use them. Update <A> first, then <B>, then this computer: on each, tell an
  agent "Update Ship of Tools: fetch https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/UPDATE-AGENT.md
  and follow it". Until then do not restart the window or the daemon on this computer: either may apply an update that
  is already staged, and move ahead of the others. Nothing was changed here.
  ```

  The order is the hub first (when it is not this computer), then the others in the table's order, then this computer.
  For an unknown daemon ask instead: "I could not read the version of the daemon on <C> (<the reason>). Does it
  already run <release>?" Go on only if the person says it does; if they say no or do not know, give the message above
  naming it.
- **One is on a higher line than the release.** The release is the older side. Tell the person, and stop.

When a computer the message names also shows `frontend` in its DECLARED column, and the `self` row's DECLARED column
holds `daemon`, that computer's window dials this computer's daemon in turn, and its agent will stop naming this
computer: nobody can go first. Ask the person to keep every window on this computer and on those computers closed until
all of them are updated, and to tell the agent on each of those computers that they agreed. On that yes, run `status`
again and go on only when every named computer whose DECLARED column lacks `frontend` is on the release's line;
otherwise give the message above naming those computers only. At step 5, do not ask for a window to start before every
one of them is done.

**No window here.** Nothing stops this update on account of a window: go on to "The group".

### The group

If `plan` printed a `self` line and a `hub` line, this computer belongs to a group that shares rows and mail through that
hub, and when the two names are the same, this computer is the hub. A computer on 0.6 and one on 0.7 carry no mail
between them and do not show each other's rows, so the group moves in one sitting. On a home shared by several
computers, an update on one replaces the binaries for all of them, but each computer's daemon keeps the old one until it
restarts: run the update on each computer anyway, one computer at a time. If another update is running on this home
(its `~/sot-update.log` is still growing), wait for it to end.

### The one question

If the version from step 1 starts with `0.6`, ask it. Otherwise, on Linux or macOS, run:

```bash
~/.local/share/sot/bin/sotd same-version --endpoint "unix:$(~/.local/share/sot/bin/sotd session-socket-path sot)"
```

If it prints a version that starts with `0.6`, this computer's daemon still runs 0.6 (on a home several computers share,
another computer's update already moved the record): ask the question. If it prints why it could not tell, and the
reason is not that no socket exists ("No such file or directory"), ask it too. If it prints nothing, another version, or
that no socket exists, do not ask. Ask it in your own words, and go on only on a yes:

> Updating from 0.6 to 0.7 ends every agent running in Ship of Tools on this computer once, and everything those agents
> started in their rows (a build, a test, a long job). On Windows the open Ship of Tools window closes and opens again
> by itself. It happens during the update, unless a window holds the daemon on 0.6.6-rc9.12 or later, and then when
> the window is next started. Each row then starts again with a new conversation, and `/resume` in a row brings an
> earlier one back. Is now a good time?

If you run inside a row (step 1), add that this ends your own session too, most likely before you can read the result,
and may cut the update short with it: when the row shows a new conversation (it begins by running `/sot-session-start`
on its own), the person types `/resume` in it and picks this conversation; you then read `~/sot-update.log` and run step
3 again, which finishes a cut-short update and ends `PASS`. That path was run once, on Linux, in a test container with a
stand-in for the service manager and a stand-in agent in a real 0.6.6 row: the agent was ended mid-run, the row came
back with a new conversation, the repeat of step 3 ended `PASS`, and a message sent to the row afterwards was filed. A
real unit also ends the row's scope, which that test did not have, so there the installer may be cut short sooner; the
repeat should cover it, since a repeat is a repair run. A row on Windows or macOS has not been run. In a Codex row the
update brings no way back to the earlier conversation: ask the person whether they have one.

## 3. Run the update

Linux and macOS: run this in one shell call.

```bash
( cd ~ || exit 1
  tag=$(curl -fsSLI -o /dev/null -w '%{url_effective}' https://github.com/kalidke/ship-of-tools/releases/latest); tag=${tag##*/}
  case "$tag" in v[0-9]*) echo "newest release: $tag" ;; *) echo "STOP: cannot reach GitHub"; exit 1 ;; esac
  f=$(mktemp) || exit 1
  if curl -fsSL -o "$f" "https://raw.githubusercontent.com/kalidke/ship-of-tools/$tag/scripts/install.sh"; then
    SOT_INSTALL_TTY=/dev/null SOT_INSTALL_TAG="$tag" bash "$f"; rc=$?
  else
    echo "STOP: could not fetch the installer of $tag"; rc=1
  fi
  rm -f "$f"; exit "$rc"
) 2>&1 | tee ~/sot-update.log
```

An option from step 1 or step 4 goes right after `bash "$f"`, for example `bash "$f" --restart-daemon`.

Windows: save these lines, exactly, as `sot-update.ps1` in your home folder (from Git Bash, the folder `echo $HOME`
prints) with your file-writing tool. An option from step 1 or step 4 goes into the `$opts` line, for example
`$opts = @('-Hub', 'myhub')`.

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
$null | & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $f @opts 2>&1 | ForEach-Object { Say $_ }
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

Use exactly these. Never add `--version` (Windows: `-Version`) unless the person named that version: naming one pins
the install and turns its automatic updates off.

Both write everything they print to `sot-update.log` in your home folder. A run takes from under a minute to many
minutes (the first run on a computer prepares Julia), longer than a tool call's default timeout: run it in the
background, or with the longest timeout your tool allows, and read the last line when it ends. If `sot-update.log` gets
no new line for 10 minutes, stop your run and run step 3 once more; if it stalls again, show the person the log's last
lines and wait for them. If your session ended during the run and you are back in this conversation, read
`sot-update.log`: when it ends with one of the lines of step 4, act on that line; when it stops mid-run (your session
was ended before the installer printed its last line), run step 3 again: on an updated computer it ends `PASS`.

## 4. Act on the last line

It ends with exactly one line.

| Last line | What you do |
|-----------|-------------|
| `PASS: Ship of Tools <version> at <commit> on this computer` | Go to step 5. What follows the first `;` is a checklist, and the line is a pass whatever it says; "pushes refused until a valid publish-list.txt exists" there concerns only the install's own copy of the repository. |
| `ONE STEP LEFT: <step>` | Give the person the step in its own words, with any choice it names (for example "choose Keep"). A window to quit and start again is theirs to do. Anything to install (Julia, jq, Claude Code) or any command it names: ask before you run it. When step 1's version starts with `0.6` and you are in a row, add that restarting the window ends your session, and that `/resume` in this row brings it back. When it is done, go to step 5. |
| `FAIL: daemon: ...`, with `--restart-daemon` (Windows: `-RestartDaemon`) in the line above it | A restart would end a live Julia REPL or Pluto notebook (their variables are lost). Nothing was changed. Ask the person, naming the rows the line names. From a 0.6 daemon it names none ("could not ask the daemon ... in full: it runs another version; ... protocol mismatch"), because a 0.7 installer cannot ask a 0.6 daemon which rows hold a REPL: it is still the question, so ask whether a REPL or notebook may end. Only on a yes, run step 3 again with `--restart-daemon` (Windows: `-RestartDaemon` in `$opts`). |
| `FAIL: daemon: ...`, with "Find why it does not answer, or stop it" in the line above it | A daemon runs that the installer cannot ask. Nothing was changed. Report both lines to the person; do not run step 3 again until they have dealt with it. |
| any other `FAIL: daemon: ...` (for example "no daemon answers", or "did not answer as <version>") | The update was written, and the daemon did not come up as it should. Report the line to the person and run step 3 once more. |
| `FAIL: target: ...` | This computer runs a build of the line (a `+` in its version), not a release. Nothing was changed. Ask the person what they want: a pack of the line, or the release the line names. Change nothing yourself. |
| `FAIL: direction: ...` | This computer runs a newer pre-release than the newest release (for example 0.7.1-rc.3 against 0.7.0). Nothing was changed, and there is nothing to update. Tell the person. Only if they name a version to go back to, run step 3 with `--version <that tag>` (Windows: `-Version`), which pins the install. |
| a `FAIL:` line ending `no such choice: ''` | The installer wanted to ask which role this computer has (nothing here records one), and had no one to ask. Ask the person: everything on this computer (`--local`), the window here and the daemon on a server they reach by ssh (`--backend <alias>`), or the daemon only (`--be-only`). Run step 3 again with that option. |
| `FAIL: enroll: ...` | The hub's `hosts.toml` does not list this computer, or the hub has none ("could not read the hosts.toml on ..."; a `--backend` install from before 0.6.6-rc9.13 never had one). Tell the person the line: the hub's owner declares this computer on the hub, as the line says or by `docs/ENROLLING-A-HOST.md`, then you run step 3 again. Until then this computer's window opens with no hosts, whatever the versions; create nothing on the hub yourself. |
| `FAIL: <anything else>` | Report the line to the person. If the run was cut short (a lost connection, an interrupt), run step 3 once more: a repeat repairs. |
| `STOP: ...` or a `curl:` error | This computer could not reach GitHub, or could not fetch the installer. Nothing was changed. Tell the person, and run step 3 again once it can. |

## 5. Prove it worked

Run step 3 once more, exactly as before. On an updated computer it is a repair run: it changes nothing and ends
`PASS: Ship of Tools <the new version> at <commit> on this computer`. If it ends any other way, act on its last line by
step 4. Then:

- `~/.local/share/sot/bin/sotd --version` (Windows: `& "$env:LOCALAPPDATA\sot\bin\sotd.exe" --version`) names the same
  version.
- After an update from 0.6, every row was started again with a new conversation, which begins by running
  `/sot-session-start`. Tell the person that `/resume` in a Claude row lists that row's earlier conversations and brings
  the one they pick back.
- Check that the daemons this computer's window dials all answer on the new version (skip this when step 2 found no
  window here, or no group). If step 4 gave you a `ONE STEP LEFT` for a daemon restart, wait until the person has done
  it. Run `status` again, with the program step 3 installed:

  ```bash
  ~/.local/share/sot/bin/sotd status
  ```

  Windows:

  ```powershell
  & "$env:LOCALAPPDATA\sot\bin\sotd.exe" status
  ```

  Every row whose DECLARED column holds `daemon`, except this computer's own, must read `<version> up` with a version of
  the new release's line. A row that reads unreachable may be a slow login: run `status` once more before you decide.
  Anything else is a daemon the window cannot use: tell the person which computer and what its
  row says, and that this computer's window should not be started until that computer is updated.
- Ask the person to quit and start again every open Ship of Tools window, on every computer of the group, once the
  daemons all answer. Never quit or end a window yourself.
