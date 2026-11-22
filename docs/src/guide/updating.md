# [Updating and rollback](@id updating)

Updating a release install on Linux, macOS or Windows is one command. To have a coding agent on the machine do it, tell
it

```text
Fetch https://raw.githubusercontent.com/kalidke/ship-of-tools/main/docs/UPDATE-AGENT.md and follow it.
```

[`docs/UPDATE-AGENT.md`](https://github.com/kalidke/ship-of-tools/blob/main/docs/UPDATE-AGENT.md) is the one update
procedure, for an agent or for a person running it by hand: its step 3 is the command for each system, the same from
every install since 0.6.6-rc1, and its other steps say what to ask the person who uses the computer first. That command
reads the newest release from GitHub's latest-release page and runs that release's own installer, so it lists no
releases through GitHub's API, needs no token, and moves a pre-release install to the latest stable release.

For a person running the installer by hand, the installed checkout's own entry is the other way in:
`bash ~/.local/share/sot/repo/current/scripts/install.sh`, or on Windows `install.ps1` from the versions path (which
also works over ssh, where `repo\current` cannot be traversed):

```powershell
$p="$env:LOCALAPPDATA\sot"; $t=(Get-Content "$p\install.json" -Raw | ConvertFrom-Json).tag
powershell -NoProfile -ExecutionPolicy Bypass -File "$p\repo\versions\$t\scripts\install.ps1"
```

Run with no options, the installed entry follows the channel the computer runs (a stable release takes the latest
stable release; a pre-release takes the newest published release of any kind) and never moves the install backwards,
within two limits. From 0.6.6-rc9.14 on, a pre-release install's entry lists the releases through GitHub's API, which
answers a shared network address past its hourly limit with `FAIL: target: could not list the releases ... (HTTP 403,
its rate limit ...)`. And from 0.6.6-rc9.15 on it sends `GITHUB_TOKEN` or `GH_TOKEN` when one is set, so a token GitHub
refuses ends `FAIL: target: could not list the releases`, and one with push access lists an unpublished draft first,
whose files cannot be fetched.

The Windows entry's options are `-Version`, `-Archive`, `-Hub`, `-Prefix` and `-RestartDaemon`, and it ends in the
same one line. A
`ONE STEP LEFT` there is usually to quit Ship of Tools (choose Keep) and start it from the shortcut, when the window's
launcher is older than release candidate 9.13 or no window is up to be told, or to start it at the desk when the run was
over ssh; with a current launcher running, the entry asks it to converge and waits for it, and your sessions keep running
between 0.7 releases. The update from 0.6 to 0.7 ends every row's agent once; use `/resume` in each row to bring its
conversation back.

However the installer is reached, the role is decided in this order: `hosts.toml`'s entry for this computer first (it
wins over a role flag); then the role flag, used only when the topology names no entry; then the role the install
recorded, used only when there is no flag either. An update therefore needs no role flag. On Windows there is no role
flag, and every box runs its local daemon (`install.json` records `daemon` and `frontend` as true). Where no Julia is
found the update's stage skips the Julia environments instead of failing: the launcher prepares them at the new
version's first start, and the run ends with a step naming `winget install --id Julialang.Juliaup -e`. A run can take
many minutes (the stage prepares Julia, and a converge waits up to 30 minutes for the window), so a coding agent runs it
in the background or with the longest timeout and reads the last line of its output. It ends with exactly one line:

| Last line | Meaning |
|-----------|---------|
| `PASS: Ship of Tools <v> at <commit> on this computer` | Done: the binaries, the checkout, the Julia environments and the agent comm are on `<v>`. |
| `ONE STEP LEFT: <step>` | The install is whole and one thing remains that only you can do, usually to quit the window and start it again, since it still runs the old version. |
| `FAIL: <check>: <detail>` | Something did not hold. Run it again after fixing what the line names; a half-finished run is safe to repeat. A `FAIL` after the run wrote the pin ends with the pin text. On Windows, one after the run staged an update that is still armed also says `; the update to <tag> stays armed: the next start applies it` (`install.sh` arms no update itself). |

Three `FAIL` lines are questions rather than faults:

- `daemon:` names the rows whose REPL is live and says when the Pluto server is running (restarting the daemon ends every
  notebook open in it, however it was opened), or says why the daemon could not be asked in full, naming the REPL rows it
  could read (a v0.6.5 daemon refuses the question, and a daemon older than the Pluto field cannot say whether its Pluto
  server is running, so the installer judges it from the daemon's own REPL and Pluto child processes instead: with none, the
  update goes on with no flag). Updating restarts the backend, which ends those REPLs' and notebooks' variables (the rows
  and their agents keep running between 0.7 releases). The update from 0.6 to 0.7 ends every row's agent once; use
  `/resume` in each row to bring its conversation back. When something is live and you are at the console, the installer asks "Restart the
  daemon? This ends ... [y/N]" in place and goes on at a y. With no console (an agent's run) it stops and changes nothing
  under the install: run it again with `--restart-daemon` (Windows: `-RestartDaemon`) when that is fine. A daemon that is running but cannot be told its
  version (it does not answer, or refuses the question) is treated the same way: the run stops with `sotd`'s own line and
  changes nothing, and the end check beside it is a `FAIL`, never a `PASS`. A daemon runs when `sotd.service` is active
  (Linux) or a process of this account runs `<prefix>/bin/sotd` (Windows: its pipe is listed or a process runs from
  `bin\sotd.exe`); a socket file alone, which a crash leaves behind, is no daemon. On Linux `--restart-daemon` answers it
  for the unit and a daemon the run cannot restart is stopped by hand; on Windows a daemon that already runs the target
  version is asked too before anything stops it.
- `target:` says the machine runs a build of the line (a `+` in its version). Install a pack of the line with
  `--archive <folder>`, a folder `scripts/pack-build.sh` packed; that runs the `install.sh` of the checkout that made the
  pack, not the installed one. A release named with `--version` is usually older than a build of the line and moves the
  machine back, so a person chooses it.
- `direction:` says the newest release is older than what runs here. Add `--version vX.Y.Z` to go back on purpose.

A daemon that a window holds, or that the launcher started, is not restarted by the update (the built-in `auto` mode
keeps a daemon under an attached window too). It is asked which REPLs are live and whether the Pluto server is running all the same, and the `ONE STEP
LEFT` line names them and the command that fits the computer (the launcher, `systemctl --user restart sotd.service`, or
starting `sotd` by hand). Until it is taken the old daemon runs beside the new `repo/current` and `bin/sot-capsule`.

Without a network, `--archive <folder>` installs from a folder carried to the machine (the base clone still needs the
repo's origin unless it already holds the commit). On Windows the frontend stages the update and `launch-sot.ps1` applies
it at the next launch, below.

## Moving from 0.6 to 0.7

- Every 0.6.6 install moves to 0.7.0 with the runbook's command: a 0.6.6 release candidate, 0.6.6 itself, a pinned
  install (the run removes the pin), and the installed entry of an install too old to print the one line, whose run
  hands over to the new release's own installer. On 0.7.0 the same command is a repair run that changes nothing.
- A Windows computer that ran from a source checkout whose window will not start (no `install.json`, an old `sot.exe`
  alone in `%LOCALAPPDATA%\sot\bin`, a launcher that says "no sotd binary found") takes a release install over it:
  the runbook's command lays down the binaries, the checkout under the prefix, `install.json` and a Desktop shortcut to
  the release's launcher, and leaves the source checkout as it was.
- The move ends every row's agent once, and everything that agent started in its row. The 0.7 daemon ends each 0.6 row
  and starts it again with a new conversation, which begins by running `/sot-session-start`; `/resume` in a Claude row
  lists its earlier conversations and brings the chosen one back. An agent that runs the update from inside a row is
  ended the same way, most likely before it reads the last line, and the update may be cut short with it: brought back
  with `/resume`, it runs the same command again, which finishes the update and ends `PASS`. That was run once on Linux,
  in a test container with a stand-in for the service manager and a stand-in agent in a real 0.6.6 row, which has no row
  scope for the retire to end; a row on Windows or macOS has not been run.
- Daemons move before windows. A window works only with a daemon of its own release line: a 0.7 window and a 0.6 daemon
  refuse each other, in either direction, and the window shows that computer as refused (for a 0.6 daemon reached over
  ssh, "the backend there predates the link; update it") with none of its rows; 0.7.0 and 0.7.1 work
  together. So on a computer with a window (the `self` row of `sotd status` declares `frontend`; the record is one per
  home, so it cannot say) the runbook reads the newest release and every daemon of the group (`sotd topology plan` and
  `sotd status`) before it changes anything. When one is on another line, or cannot be read, it stops with nothing
  changed and names the computers to update first (the hub, then the others, then this one); it never updates another
  computer itself, because an agent on each does that with that person's yes. It also tells the person not to restart
  the window or the daemon until then, since either may apply an update the daemon already staged (the daemon checks
  daily and applies at its next start). After the update it reads the daemons again, and asks for the windows to be
  started only when every one answers on the new line. A computer with no window is not held by this. Computers that
  each run a window and a daemon that the others' windows dial cannot all go first: the person keeps those windows
  closed until all are updated, and the runbook goes on once every named computer without a window is on the new line.
- A `--backend` laptop and its server with no `hosts.toml` on either computer (an install from before 0.6.6-rc9.13) is
  not moved by the update, from either computer: a 0.7 window dials only the hosts of a group file kept on its hub, so
  moving either computer leaves the window with no host (`FAIL: enroll` on the laptop). The runbook first fetches the
  hub's group file when the record or the launcher names a hub (a group member whose copy is missing needs no file
  written, only the order check); if the hub holds a file that does not list the laptop, the hub's owner adds it with `sotd topology set add`; if
  the server holds no file at all, it stops with nothing changed and tells the person to write a three-block
  `hosts.toml` on the server and sync it on the laptop, after which the same message moves the pair, the server first.
  That works only when the server's host name is also the name the laptop reaches it by; otherwise the pair stays as
  it is (BACKEND-PAIR-NEEDS-HUB-HOSTS-TOML). Nothing in the update writes or replaces a `hosts.toml` on another computer.
- The same message can be sent again from any state an earlier run left: a window computer already on 0.7 whose daemon
  still runs 0.6 stops and names the daemon's computer (a repair there changes nothing); the daemon's computer updates
  and ends `PASS`; a 0.7 daemon with a 0.6 window lets the window's computer update; both on 0.7 is a repair that changes
  nothing. A pair with no group file is the exception above.
- A hub and the computers linked to it move in one sitting. A computer on 0.6 and one on 0.7 carry no mail between them.
  Computers that share one comm folder keep their mail across the line.
- The move is one way. A rollback to 0.6 restores the binaries, links, unit and record, not the agent comm and not the
  rows 0.7 already started again.

## What updates a machine

Two mechanisms complement rather than compete (release installs only; **a source build, with a `+` in its version, never
self-updates** and updates by `git pull` and a rebuild):

| Mechanism | What it does | What you do |
|-----------|--------------|-------------|
| **Built-in update check** | The backend checks GitHub for new releases daily, **notifies the frontend**, and prepares the whole new version in the background: binaries, a tag-pinned checkout and instantiated Julia environments, verified against the release's `SHA256SUMS`. The staged version is applied at the next launch (or daemon restart) as an offline switch, with the previous version kept for rollback. | Nothing: you will see the notice, and the mark on the window's bottom border while the update is found, being prepared or ready; start Ship of Tools again when convenient, quitting as the mark says so the sessions keep running. |
| **Running the installer** | The **complete** update in one command, above: new binaries *and* the repo checkout moved to the new tag *and* Julia envs re-instantiated, with one verdict line. It is also what migrates a pre-0.6 install onto the auto-update layout. | Follow `docs/UPDATE-AGENT.md` (or tell an agent to fetch it and follow it). |

`SOT_UPDATE_MODE` controls the built-in check: `notify` (the default) stages and applies at the next launch, `off`
disables it, and `auto` additionally restarts the backend to apply, if no frontend is attached when the daily check arms
an update (otherwise it applies at the next launch or restart). That restarts REPLs and kernels, so their in-memory state
is lost, which is why it is opt-in; agent sessions survive it between 0.7 releases.

While this computer's daemon has a newer release in hand, the window says so on its bottom border, right after the
version stamp (`fe ... · be ...`), for as long as it is true:

| The mark | What it means |
|----------|---------------|
| `update 0.7.4 found` (gray) | A newer release was found and nothing has begun to prepare it yet, or nothing can here. |
| `update 0.7.4 preparing` (gray) | The release is being downloaded and its checkout and Julia environments built, which can take many minutes. |
| `update 0.7.4 ready: quit keeping sessions, then start again` (green) | The release is prepared, and the next start of Ship of Tools on this computer applies it. Do not use the close button to get there: it ends this computer's sessions. Quit with `Ctrl+q` and answer Yes, then start Ship of Tools again; "Applying a ready update", below, says what that start does, and what is different on a Mac. |
| `update 0.7.4 not applied` (yellow) | The release was found and could not be made ready, or a prepared update no longer fits what is installed. The reason is in this computer's row of `sotd status`, below. |
| `updates off: install record unreadable` (yellow) | Automatic updates are off because the install's record cannot be read; `sotd status` names the installer run that repairs it. |

Where the border has no room for those words the mark shortens (`0.7.4 ready`, `0.7.4 not applied`, `updates off`), and
where even that does not fit it is not drawn. With nothing prepared there is no mark when the install is up to date,
before the first check, when updates are off by choice (a source build, update mode off, a pinned install) or when a
check could not run. A prepared update that fits the install is ready in every one of those states and shows green: an
update an operator staged onto a source build, or one prepared before the daemon last restarted and not yet checked
again. Two states have a line in `sotd status` and no mark, because nothing in them is ready: a prepared update whose
file cannot be read, and a refused one whose version is not known. When one release could not be made ready while
another is prepared, the mark is the prepared one's (`update 0.7.4 ready: quit keeping sessions, then start again`),
because that is what the next start applies; `sotd status` says both. A newer release that is found or being prepared
while an older one is ready shows as the newer one's gray mark; a start meanwhile applies the older one.

**Applying a ready update.** The next start of Ship of Tools on that computer applies it: the launcher applies the
update before the window opens, and restarts that computer's daemon on the new version.

- The daemon's restart ends that computer's REPLs and its Pluto server, so what a REPL held in memory is lost.
  Sessions and their agents keep running through it between 0.7 releases: a session's terminal runs outside the daemon,
  and the restarted daemon finds it again.
- Getting to that start is where sessions are lost. A plain close of the last window on a computer (the close button,
  the system's close, `Cmd+q` on macOS) ends every session on that computer and their agents, as it always does (see
  "What keeps running when the window closes"); the next start still applies the update, to a computer with no
  sessions. To keep them, quit with `Ctrl+q` and answer Yes to **Keep the daemon and sessions running?** (No is the
  default), then start Ship of Tools again. That is what the mark's words say.
- On a Mac that runs its own sessions, no way to follow those words is known. The quit key there is `Cmd+q`, which
  macOS's own Quit takes, and that Quit is a plain close: it ends the sessions without asking. Whether the question
  can be reached on a Mac has not been tried on one. So on such a Mac the sessions end at the quit, and the next
  start applies the update. A Mac whose sessions all run on another computer loses nothing by quitting.
- A computer whose plan lists another computer's daemon (a hub with servers behind it) has a different launcher: it
  applies the update itself while the daemon runs, and restarts the daemon afterward only when no window holds it.
  Quitting with Yes and starting again ends the same way there. With a second window still open, the install is
  switched and the daemon stays on the old version, with the launch notice `daemon out of date, restart when ready`,
  until a start that finds no window holding it. (Read from the launch scripts; that case has not been run.)
- On a Linux standard install the daemon is a user service: the launcher restarts the service, and the service's own
  start applies the update. Restarting the service by hand applies it too, with no window start (`systemctl --user
  restart sotd.service`); a window that stays open through that still runs the old version beside the new daemon, and
  its stamp shows the two versions in yellow until that window is started again.
- Where opening the window starts the daemon (macOS, Windows, a Linux install without the service), the launcher stops
  the daemon, applies the update and starts the daemon again. On Windows that start is the shortcut.
- On Linux and macOS, opening a second window is a start too: its launcher applies the update while the first window
  is open.
- The start checks the prepared files once more. If it finds them damaged, or finds that version already installed, it
  applies nothing and removes the prepared update; the mark goes, and a damaged update is prepared again by the next
  check.

The mark is this computer's own: it is what the daemon on the computer the window runs on says, which is the update
this window's next start applies. It says nothing of another computer the window is connected to (a laptop's window
shows nothing of its hub's update); those are in `sotd status`. The version stamp beside it can speak of another
computer: its `be` half is the daemon of the selected session's computer, while the mark is always this computer's.
So a laptop whose sessions run on a hub can read `fe 0.7.3 · be 0.7.3` and then `update 0.7.4 ready: ...`; after its
restart it reads `fe 0.7.4 · be 0.7.3` in yellow, and stays so until the hub is updated, because the hub's update is
not the laptop's to apply. The mark goes away while the window has no connection to
its own computer's daemon and comes back when the daemon says its state again. The notice on the status line when a
release is found still appears, for ten seconds.

On a computer that runs no daemon of its own, where the window checks for updates itself when it starts, the mark is
that check's: the same words, for the update the window's next start applies. Its sessions run on another computer
and no close of this window ends them, so to apply a ready update there, close the window any way and start it
again: the launcher applies the update before the window opens, and nothing restarts on the other computer. That
check runs once, at the window's start, and nothing reads the disk again while the window runs, so the mark is what
the check came to then:

- A release published later is found by the next start.
- A yellow `not applied` stays until the window closes, also where nothing is wrong: when the update was refused
  because the install had moved to that version meanwhile, is pinned or is a source build. On a computer with a
  daemon the next daily check corrects such a mark.
- When a second window's start applies the update, the first window still shows it green.
- A green mark there is not checked against what is installed, as a daemon's is. If an installer run moved the
  install after the update was prepared, the mark still says ready; the next start removes the prepared update
  instead of applying it, that window's check prepares it again if it is still newer, and the start after that
  applies it.

There `sot --update-status` says how far the update has got, and the cause of a step that failed is in the window's
log.

To see where a computer's update stands, run `sotd status`. Its last column, `UPDATE`, is what each computer's daemon
says of its own update:

| `UPDATE` reads | What it means |
|----------------|---------------|
| `not checked yet` | The daemon started less than about two minutes ago and has not run its first check. |
| `up to date (checked 3 h ago)` | The last check found nothing newer than what is installed. |
| `0.7.4 found` | A newer release was found. A reason after it says why nothing is being prepared; `it was armed here and the armed update is no longer on disk` means that computer had prepared it and another program has since removed or applied the prepared update, and the next check looks again. |
| `0.7.4 preparing (can take many minutes)` | The release is being downloaded and its checkout and Julia environments built. |
| `0.7.4 ready: ...` | The release is prepared. The words after it say what applies it, on the computer of that row: `applies when that computer's sotd.service next starts (a start of Ship of Tools there, or a restart of the service)`, or `applies when Ship of Tools is next started on that computer`. How to get to that start without ending that computer's sessions is under "Applying a ready update", above: a plain close of its last window ends them. Starting Ship of Tools on another computer does not restart that computer's daemon. Where two computers share one install (a shared home folder), a start on either applies it for both, and the other's row reads found, as above, until its next check; its daemon runs the new version after its own restart. The start checks the prepared files once more; if it finds them damaged, or finds that version already installed, it applies nothing and removes the prepared update, and a damaged one is prepared again by the next check. |
| `0.7.5 preparing; 0.7.4 is ready meanwhile` | A newer release is being prepared while an earlier one is already ready. |
| `0.7.4 not applied: ...` | The reason follows. Either the release was found and could not be made ready, and the next daily check tries again; or a prepared update no longer fits what is installed and would be refused, and it stays so until the next start on that computer removes it. |
| `an armed update cannot be read; the next start drops it` | The file that names a prepared update is damaged; the next start removes it and the next check prepares the update again. |
| `off: ...` | Automatic updates are off on that computer, with the reason: a source build, update mode off, a pinned install, an install record that cannot be read (the reason names the installer run that repairs it), or a computer that `hosts.toml` gives neither `daemon` nor `frontend`. |
| `check could not run 2 h ago: ...` | The last check could not reach or read the release; the next one tries again. |
| `not reported` | That computer's daemon is older than 0.7.4 and does not say. |
| `-` | No daemon was asked there, or it did not answer. |

A cell that ends `; as last read, the disk did not answer in time` is what that daemon last read: its disk was too slow
to be read again for this answer. A long reason keeps its beginning and its end, with ` … ` where the middle was left
out.

Applying a ready update restarts that computer's daemon, which ends its REPLs and its Pluto server; sessions keep
running between 0.7 releases. `sotd status` needs the computer's `hosts.toml`; on a computer without one,
`sot-fe version` prints the same words for the one daemon it asks, on a line that begins `update` (`update  0.7.4
ready: ...`, `update  off: dev build`). A daemon reads the prepared update from disk again each time it is asked, so
both commands say what is on disk at that moment. The window's mark is told by its daemon when the state changes and
when the window connects; an update prepared by another program (an installer run, an operator's stage) reaches it
when the window next connects or the daemon next reads the disk.

The daemon of a machine runs the check; a window runs it itself only where the daemon does not own updates (a host the
topology declares `daemon = false`, a `frontend` entry or a `--backend` frontend-only install whose install runs no
daemon, on any OS). A pre-release install follows
pre-releases and a plain release follows plain releases; the check never moves an install backwards or into a dev
build, and an armed update records the install it was armed over, so `sot-apply` drops one that no longer fits (the
installer moved on, the version is not above the install, the stage has no record of its prepare, or a host that runs a
backend meets a prepare that did not instantiate the Julia environments) without stopping the daemon. A host the hub's
`hosts.toml` names with neither `daemon` nor `frontend` runs no `sotd` and never arms into an install it shares.
`sotd update stage <folder> --prefix <prefix>`, the operator's command for a packed build, is the one way to arm the
same or an older build.

On **Linux and macOS** each release's own `sot-apply` applies the next update, and brings the agent comm (`~/.sot-comm`)
to the applied commit on every role. On **Windows** `launch-sot.ps1` applies the staged update at the next launch
through `scripts\sot-apply.ps1`: the same verification and rollback.

## What the installer does to the checkout

The installer fetches tags into `$PREFIX/repo/base`, adds a worktree for the release at `$PREFIX/repo/versions/<tag>`,
checks that its `HEAD` is the tag's recorded commit (a moved tag or a half checkout fails at update time, not at first
use), and points `$PREFIX/repo/current` at it. It refuses to go on only if that new tag's own folder has local changes.
An update keeps the previous version's folder for rollback, and the previous binaries as `bin/*.prev`, and removes older
folders, except a folder with local changes: the installer names it, keeps it, and removes it on a later update once it
is clean. A repair run (the same version again) prunes nothing and keeps both. Do not edit the checkout; it is the manual
and the resource tree, and changes in it are not carried to the next version.

## Pinning and going back

- **Pin a release.** Add `--version vX.Y.Z` (Windows: `-Version`) to move to (or stay on) a specific tag; the same flag
  moves you *back* to an older tag. A run that names what to install (`--version`, `--archive`) pins the install:
  `install.json` records `"pinned": true` and the daily check and the window's own check leave it alone, so a downgrade is
  not undone the next morning. The update status says "pinned at <version>: automatic updates stay off for this install
  until a run that names no version", and the `PASS` or `ONE STEP LEFT` line ends with it. A run that names no version removes the pin, and an `install.json` with no pin field (every install made before the
  field existed) is unpinned and follows its channel. Running the version a machine already runs, by name, is how a machine
  is pinned in place: when its daemon already answers as that version the run writes the pin, drops any update pointer
  armed over the install, and stops, restarts, stages and applies nothing (it still downloads and checks the release, whose own `sotd install` makes that decision).
  A build with a `+` never updates automatically, so a pin changes nothing there.
- **An `install.json` that cannot be read.** A record cut short (an interrupted write, a bad hand edit) may hold a pin,
  so automatic updates stay off for that install, and the update status says so in one sentence: "install.json cannot be
  read at <path> (<cause>); it may hold a pin, so automatic updates stay off until it is repaired by running
  `install.sh --version <version>`" (Windows: `install.ps1 -Version <version>`). That run rewrites the record whole and
  pins the install at the version; a later run that names no version removes the pin. A leading byte-order mark alone
  (a record saved by Notepad) is not unreadable. On Windows a record that is missing while `bin\sotd.exe` exists is
  treated the same way, and the status says "install.json is missing at <path> ..." with the same run to write it again (it
  works while Ship of Tools runs: the installer never takes an install with a `bin\sotd.exe` for a new one). "Update sot
  from the repo", which names no version, repairs a record it cannot read too, and does not pin.
- **Automatic rollback.** Applying a staged update is an offline pointer flip done by `sot-apply`, which records the
  previous install as *last good*. The `--local` launcher runs `sot-apply --rollback` if the frontend exits with an error
  within 10 seconds twice in the 30 minutes after an update. Windows rolls back after a single such exit within two hours
  of the update, through `launch-sot.ps1`. A `--backend` (frontend-only) install on Linux or macOS does not roll back on
  its own: run the installer with `--version` to go back.
