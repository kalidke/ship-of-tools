# [Updating and rollback](@id updating)

Updating a release install on Linux, macOS or Windows is one command. Tell a coding agent running on the machine

```text
update sot from the repo
```

or run it yourself:

```bash
bash ~/.local/share/sot/repo/current/scripts/install.sh
```

On Windows the same entry is `install.ps1` in the installed checkout (its versions path also works over ssh, where
`repo\current` cannot be traversed):

```powershell
$p="$env:LOCALAPPDATA\sot"; $t=(Get-Content "$p\install.json" -Raw | ConvertFrom-Json).tag
powershell -NoProfile -ExecutionPolicy Bypass -File "$p\repo\versions\$t\scripts\install.ps1"
```

Its options are `-Version`, `-Archive`, `-Hub`, `-Prefix` and `-RestartDaemon`, and it ends in the same one line. A
`ONE STEP LEFT` there is usually to quit Ship of Tools (choose Keep) and start it from the shortcut, when the window's
launcher is older than release candidate 9.13 or no window is up to be told, or to start it at the desk when the run was
over ssh; with a current launcher running, the entry asks it to converge and waits for it, and your sessions keep running.

With no options it moves to the newest release on the channel the machine already runs (a stable release follows the
stable releases; a release candidate follows the candidates), and never moves the install backwards. The role is decided
in this order: `hosts.toml`'s entry for this computer first (it wins over a role flag); then the role flag, used only when
the topology names no entry; then the role the install recorded, used only when there is no flag either. An update
therefore needs no role flag. On Windows there is no role flag, and every box runs its local daemon (`install.json`
records `daemon` and `frontend` as true). Where no Julia is found the update's stage skips the Julia environments instead
of failing: the launcher prepares them at the new version's first start, and the run ends with a step naming
`winget install --id Julialang.Juliaup -e`. A run can take many minutes (the stage prepares Julia, and a converge waits up
to 30 minutes for the window), so a coding agent runs it in the background or with the longest timeout and reads the last
line of its output. It ends with exactly one line:

| Last line | Meaning |
|-----------|---------|
| `PASS: Ship of Tools <v> at <commit> on this computer` | Done: the binaries, the checkout, the Julia environments and the agent comm are on `<v>`. |
| `ONE STEP LEFT: <step>` | The install is whole and one thing remains that only you can do, usually to quit the window and start it again, since it still runs the old version. |
| `FAIL: <check>: <detail>` | Something did not hold. Run it again after fixing what the line names; a half-finished run is safe to repeat. A `FAIL` after the run wrote the pin ends with the pin text. On Windows, one after the run staged an update that is still armed also says `; the update to <tag> stays armed: the next start applies it` (`install.sh` arms no update itself). |

Three `FAIL` lines are questions rather than faults:

- `daemon:` names the rows whose REPL is live and says when the Pluto server is running (restarting the daemon ends every
  notebook open in it, however it was opened), or says why the daemon could not be asked in full, naming the REPL rows it
  could read (a v0.6.5 daemon refuses the question, and a daemon older than the Pluto field cannot say whether its Pluto
  server is running, so the first update from either pays one flag). Updating restarts the backend, which ends those REPLs'
  and notebooks' variables (the rows and their agents keep running). Nothing under the install was changed. Run it again
  with `--restart-daemon` (Windows: `-RestartDaemon`) when that is fine. A daemon that is running but cannot be told its
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

## What updates a machine

Two mechanisms complement rather than compete (release installs only; **a source build, with a `+` in its version, never
self-updates** and updates by `git pull` and a rebuild):

| Mechanism | What it does | What you do |
|-----------|--------------|-------------|
| **Built-in update check** | The backend checks GitHub for new releases daily, **notifies the frontend**, and prepares the whole new version in the background: binaries, a tag-pinned checkout and instantiated Julia environments, verified against the release's `SHA256SUMS`. The staged version is applied at the next launch (or daemon restart) as an offline switch, with the previous version kept for rollback. | Nothing: you will see the notice; relaunch when convenient. |
| **Running the installer** | The **complete** update in one command, above: new binaries *and* the repo checkout moved to the new tag *and* Julia envs re-instantiated, with one verdict line. It is also what migrates a pre-0.6 install onto the auto-update layout. | `bash ~/.local/share/sot/repo/current/scripts/install.sh`, or say "update sot from the repo". |

`SOT_UPDATE_MODE` controls the built-in check: `notify` (the default) stages and applies at the next launch, `off`
disables it, and `auto` additionally restarts the backend to apply, if no frontend is attached when the daily check arms
an update (otherwise it applies at the next launch or restart). That restarts REPLs and kernels, so their in-memory state
is lost, which is why it is opt-in; agent sessions survive it.

The daemon of a machine runs the check; a window runs it itself only where the daemon does not own updates (a host the
topology declares `daemon = false`, or a `--backend` frontend-only install, on any OS). A pre-release install follows
pre-releases and a plain release follows plain releases; the check never moves an install backwards or into a dev
build, and an armed update records the install it was armed over, so `sot-apply` drops one that no longer fits (the
installer moved on, or the version is not above the install) without stopping the daemon. A host the hub's
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
  armed over the install, and restarts nothing (on Windows a named run of the installed version fetches no release asset; the prelude still fetches the tag's `install.ps1` unless `SOT_INSTALL_TAG` is set).
  A build with a `+` never updates automatically, so a pin changes nothing there.
- **An `install.json` that cannot be read.** A record cut short (an interrupted write, a bad hand edit) may hold a pin,
  so automatic updates stay off for that install, and the update status says so in one sentence: "install.json cannot be
  read at <path> (<cause>); it may hold a pin, so automatic updates stay off until it is repaired by running
  `install.sh --version <version>`" (Windows: `install.ps1 -Version <version>`). That run rewrites the record whole and
  pins the install at the version; a later run that names no version removes the pin. A leading byte-order mark alone
  (a record saved by Notepad) is not unreadable. On Windows a record that is missing while `bin\sotd.exe` exists is
  treated the same way, and the status says "install.json is missing at <path> ..." with the same run to write it again.
- **Automatic rollback.** Applying a staged update is an offline pointer flip done by `sot-apply`, which records the
  previous install as *last good*. The `--local` launcher runs `sot-apply --rollback` if the frontend exits with an error
  within 10 seconds twice in the 30 minutes after an update. Windows rolls back after a single such exit within two hours
  of the update, through `launch-sot.ps1`. A `--backend` (frontend-only) install on Linux or macOS does not roll back on
  its own: run the installer with `--version` to go back.
