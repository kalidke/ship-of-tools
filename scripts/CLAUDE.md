# scripts/: distribution (charter)

## Idea
A box runs exactly one tagged release. CI turns a tag into verified artifacts; install, update-apply and launch place,
swap and start that release as whole files under `$PREFIX` (`~/.local/share/sot`, or `%LOCALAPPDATA%\sot` on Windows),
and the launch path fails open: no update step can stop a window from starting.

## Owns
- The release cut and CI: `release.sh`, `tests/rc-gate.sh`, `.github/`.
- Install and the update entry: `install.sh` (Linux and macOS) and `install.ps1` (Windows); "update sot from the repo"
  runs the one for the OS. On Windows `install.ps1` runs `install-shortcut.ps1` and `install-manifest.ps1` and the
  launcher's own `Initialize-InstallLayout` (in `sot-install-layout.ps1`, also run by `launch-sot.ps1`) and
  `Invoke-FreshnessPass` from the target checkout.
- Update discovery and staging: `rust/updater`, `rust/backend/src/update.rs` (the `update.check` and `update.apply`
  ops, and `sotd update stage`), `rust/frontend/src/selfupdate.rs`.
- Apply: `sot-apply.sh`, `sot-apply.ps1`.
- Launch and supervision: `lib/sot-daemon.sh`, `launch-sot.sh`, `launch-sot.ps1`, `sot-local-daemon.ps1`,
  `sot-hosts.*`, `relaunch-sot.ps1`, `shutdown-sot.ps1`, `deploy/sotd.service`.
- The comm installer `ShipTools.update_comm()` (`src/`, `test/`), `~/.sot-comm/bin`, and the Cargo workspace
  manifest.
- On disk: `$PREFIX` with `bin/` (plus `.prev` copies), `repo/{base,versions/<tag>,current}`, `install.json`,
  `updates/`, `logs/`.

## Promises
- A tag is cut only from `main`, sorts above every tag of its line, and has green `rust.yml` and
  `CI.yml` runs at HEAD (`release.sh` preflight).
- An install never passes with a Julia below the kernel's floor: `install.sh` (`julia_floor_check`) and `install.ps1`
  (`Test-JuliaFloor`) ask, as `julia.rs` does, the Julia the daemon will run (`SOT_JULIA_BIN`, else juliaup's default channel read from
  `juliaup.json` by its Version string with no Julia started, else the PATH julia) and the PATH julia the comm refresh
  runs. Below `[compat] julia` of `julia/kernel/Project.toml` the PATH julia ends the run before any step runs on it, and
  the daemon's alone, or either unaskable, ends it ONE STEP LEFT. The line names the version and the command: `juliaup
  default <floor>` (after `juliaup add <floor>` when `juliaup.json` lists no such channel), a newer Julia ahead on PATH,
  or the `SOT_JULIA_BIN` to change.
- Install checksums verify before any write, and `install.json` is written last (`install.sh` steps 2 and 9). Every writer of it
  (`install.sh`, `install-manifest.ps1`, `install.ps1`'s pin edit, `sot-apply.sh`, `sot-apply.ps1`) writes a temp file in
  the same folder and renames it, with no BOM, so an interrupted write cannot cut it: a file that cannot be read keeps
  automatic updates off until an installer run repairs it (ADR 0030 rule 6), and `install-manifest.ps1` writes
  `"pinned": true` over a record it replaces and cannot parse. `sot-apply.ps1` and `install-manifest.ps1` share
  `Write-Utf8NoBomViaRename` in `sot-install-layout.ps1`; `install.ps1` keeps its own copy, because its first pin edit runs
  before the target's layout file loads.
- A refused install writes nothing under its prefix or `~/.local/bin`: the role and the ownership, host and record
  gates run on the unpacked `sotd` before the layout, and an unpacked `sotd` that cannot run stops the install. Without
  `--force-role-change`, a host a readable hosts.toml runs no sotd on never writes over an install at the prefix
  (`installer_host_decision`), and under a declared topology a run never records less than install.json does
  (`installer_record_decision`). The update stager (`stage_prepare_arm` in `rust/backend/src/update.rs`) arms nothing
  into an install from a host the topology names with neither `daemon` nor `frontend` (a `sotd` started by hand there
  included), and an automatic arm goes only strictly above `install.json`'s version and never into a dev build.
- Apply verifies everything before it mutates, restores on a failure after mutation and exits 0; a rollback marks the
  bad tag so it is never armed again (`sot-apply.sh`, `sot-apply.ps1`).
- On Unix a release runs its own `sot-apply` at every later update: `sot-apply.sh` is git mode 100755 (the packers copy it
  with `cp`, which keeps the mode), and the staged copy, re-executed with `SOT_APPLY_REEXECED` set (every installed apply since v0.6.5
  does), reads the install prefix from where the stage sits under `<prefix>/updates/<tag>-<target>/<top>/`: one rule for
  every installed apply. After an apply `sot-apply.sh` runs `sot_comm_refresh` on the applied checkout,
  whatever the box's role, so comm follows every apply; the refresh is fail-open. `installer-handoff.sh`, sourced by
  `installer-apply.sh`, runs a real staged copy and pins the mode in the packed archive. The unit's `ExecStartPre` runs
  `sot-apply` through the same `bash -c` form as `ExecStart` (it sees `~/.bashrc`), and the comm refresh's `julia` is
  bounded by `timeout` (`SOT_COMM_REFRESH_TIMEOUT_S`, 30 s) where one exists.
- `--drop-only` (`sot-apply.sh`) and `-DropOnly` (`sot-apply.ps1`) exit after the in-lock refusal check and never apply or
  re-execute: the wrapper's `apply_pending` and `Invoke-PendingApply` run them when their own question refused the
  pointer and left the daemon running, so a fitting pointer armed in between waits for the next start.
- A pointer is applied only onto the install it was armed over. Both apply scripts ask the installed `sotd update
  pointer` above their re-exec of the pointer's own release's script (so a refused pointer never reaches it) and drop a
  pointer it refuses (exit 3, an unreadable one included) under the lock; any other answer, an older `sotd` included,
  leaves the exact-tag check. `apply_pending` in the rendered `sot-launch` and `Invoke-PendingApply` ask first, so a
  pointer that will not apply stops and restarts no daemon, and they run the apply drop-only. A
  new install drops the leftover pointer (`install.sh` for its target, `install-manifest.ps1` when the version or commit
  changes). The rule is ADR 0030's 2026-10-17 amendment; `installer-apply.sh`'s `a_refused_pointer_is_dropped_and_nothing_is_applied`
  group and `test-sot-apply.ps1` section 9 pin it.
- On Unix the installer is the update entry (`install.sh`, run from the installed checkout or as the one-liner), and every
  exit ends in exactly one line, printed by `fail`, by the verdict or by the one EXIT trap (`on_exit`) when a command
  failed under `set -e` or a signal came: `PASS: Ship of Tools <v> at <short> on this computer`, `ONE STEP LEFT: <what a
  person does>` (exit 0, the install is whole) or `FAIL: <check>: <detail>` (exit 1, or 2 for a refusal). With no
  `--version` and no `--archive` it follows the channel `install.json` records (no install or a stable release: GitHub's
  latest stable; a pre-release: the newest published release of any kind; a `+` build: stop and send the person to
  `--archive <folder>` of a pack of the line) and never moves an install backwards: the target's own `sotd update
  direction` says `older` (or cannot say while the installed version is readable) and the run fails, before anything under the prefix changes. The installed version is the record's, else the installed `sotd --version`'s (it also picks the channel, and `direction` takes it as `--installed` where the record names none); only where neither says does the run go on and rewrite the record. `--archive` runs
  the `install.sh` of the checkout that made the pack. With no role flag and no topology entry the role comes back from
  `install.json` (its bits, its `service`, its `hub`; a remote install's alias from its wrapper). A daemon is restarted
  only when it runs another version (`sotd same-version`) and only an active unit no window holds; the restart, and a
  fresh start of the unit, are verified: the daemon must answer as the target within `SOT_INSTALL_RESTART_WAIT_S` seconds by the clock (a daemon whose socket accepts
  and never answers costs the bound plus one ask), and an active unit whose daemon is in any other state is `FAIL: daemon:`,
  never PASS; a unit that is not running because its host pin excludes this computer says the pin is the cause. Every daemon on another version is
  asked `sotd update repls` (the daemon's own `workspace.list`: the rows with a live REPL and whether its Pluto server is
  running, which ends every notebook open in it) before anything changes. When the unit would be restarted and a REPL is
  live or the Pluto server is running, or the daemon cannot be asked in full (a daemon older than the Pluto field cannot
  say), the run stops first and asks for `--restart-daemon`, changing nothing under the install; a stop for a daemon that
  could not be asked carries `sotd update repls`' one stderr line as its reason and names the REPL rows it could read. A
  daemon runs when its unit is active or a process of this account runs `<prefix>/bin/sotd` (never a socket file alone);
  one that runs and `sotd same-version` cannot tell is `unknown`: the run stops (exit 2) with sotd's line before anything
  changes (`--restart-daemon` answers it for the unit), and the end check beside it is a FAIL, as on Windows. A
  held unit and a launcher-started daemon are not restarted: the
  `ONE STEP LEFT` names the live REPLs, the running Pluto server and the command that fits (the launcher, `systemctl --user restart sotd.service`, or by hand), and says the
  old daemon runs beside the new `repo/current` and `bin/sot-capsule` until then. A box the topology plan names, with
  another daemon host in the plan, gets the launcher that dials the plan (`launch-sot.sh`) whether or not `--backend` is
  given again. That launcher exports `SOT_OWN_DAEMON=1` (a `--backend <alias>` install runs no daemon and does not), and `launch-sot.sh`
  then ensures this computer's own daemon (`sot_daemon_ensure` on `sotd session-socket-path`, as `launch-sot.ps1` does) and starts the
  frontend with `--socket <it>` beside the plan's `--dial`s, dropping the plan's dial line for this host: a window shows this computer's
  own rows whether or not the plan lists it as a daemon host, and never connects twice. A `--backend`
  install whose hub's hosts.toml omits this computer ends `FAIL: enroll:` naming the `sotd topology set add` command to
  run on the hub. The rollback survives a repeat: a run whose `repo/current` is already at the target prunes no
  `repo/versions/<tag>` folder and leaves a `bin/*.prev` equal to the installed binary alone; any prune keeps, names and
  never deletes a folder with local changes. `installer-entry.sh` (sourced by `installer-state.sh`) runs the installer
  end to end and pins each of these.
- The launch path never stops on an update: every update or freshness step logs and the window still starts.
- One launcher per user, and no window spawns without a lease held on the local daemon across the gap (Windows:
  `launch-sot.ps1`, `Open-SotLease`).
- The comm installer publishes by copy then rename, prunes only names it recorded and writes `VERSION` last
  (`install_comm` in `src/comm.jl`).

- Every network call a product script makes carries a bound on its whole wait: ssh runs under `sot_bounded` (bash:
  comm-lib's, of which `install.sh` carries the one copy) or `Invoke-SotWithin` (PowerShell, `sot-hosts.ps1`) with the
  tree's `SSH_OPTS`, git with `http.lowSpeedLimit=1000` and
  `http.lowSpeedTime=60` (install.sh's one `git` function and install.ps1's one `Invoke-Git`, never an environment variable the
  daemon and rows would inherit), curl with `CURL_BOUND` (connect 15 s, under 1000 B/s for 60 s).

- Both installer paths delegate folder-trust declaration to the agents-owned offline command; a failure is reported
  and never described as a successful declaration.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `sot_updater::Spawner`, `--socket`, `--dial`,
`--relaunched`, `relaunch.request`, `spawn_watcher`, `rust/frontend/src/relaunch.rs`, `deploy/sotd.service`,
`sot-apply.sh`, `install_comm`, `update_comm`, `comm/bin-folders.txt`, `src/sources.jl`, `~/.sot-comm/bin`,
`scripts/install.sh`, `docs/INSTALL-AGENT.md`, `v*`, `.github/workflows/release.yml`, `rust/updater`, `SHA256SUMS`,
`scripts/release.sh`, `compare_versions`, `updates/pending-<target>.json`, `updates/.lock`, `sot-apply.ps1`,
`ExecStartPre=-`, `sot-launch`, `Invoke-PendingApply`, `render_sot_launch`, `launch-sot.ps1`. Uses: `UpdaterSpawner`, `WindowSpawner`, `Signal::spawn`, `fe.lease`,
`fe.leaving`, `scripts/sot-lease.ps1`, `launcher_bounds_match_ops`, `scripts/tests/installer-state.sh`,
`is_release_build`, `rust/backend/src/update.rs`, `rust/frontend/src/selfupdate.rs`, `version_line`, `--version`,
`sotd topology plan|sync|status`, `sotd update direction|repls`, `pluto_server_up`, `sotd session-socket-path`, `launch-sot.sh`, `Get-SotTopologyPlan`,
`scripts/lib/sot-daemon.sh`, `sotd stdio-bridge`, `Leases::commit_update`, `julia::resolve_bin`, `check_remote_fs`,
`scripts/install.sh`, `REMOTE_FS_TYPES`, `docs/make.jl`, `.github/workflows/CI.yml`,
`sotd trust declare <absolute-prefix>`, `sotd topology pin`, `HeldDir`, `remove_ended`.

## Folders
- `scripts/lib/`: the shared Unix launch library.
- `scripts/tests/`: the suites, the local candidate gate, and the install smokes release.yml runs on a release's artifacts.
- `deploy/`: `sotd.service`, the unit template (no page; its header is its page).
- `.github/`: workflows and the tag gate.
- `src/`, `test/`: the comm installer package.
- `rust/updater/`: update discovery, staging and the pending pointer.
- `rust/backend/src/update.rs`: the daemon's update ops.
- `rust/frontend/src/selfupdate.rs`: the window's update check and banner.

## Files
### Release
- `release.sh`: stamps the version, runs the workspace tests, commits and tags the cut, stamps the line's next -dev
  version in its own commit, and pushes both. Before the stamp it refuses, naming the fix, when the MathJax modules or a
  reachable user manager (a test container started with `--user-manager`) are missing; a failure after the stamp
  restores the stamped files.
### Install
- `install.sh`: the Unix installer and update entry; the one-liner fetches its own tag's copy (the channel's, with no
  `--version`); `--archive <folder>` installs a packed folder in place of a release download; ends with one verdict line.
- `pack-build.sh`: builds this checkout's Linux release archive, COMMIT, LIBC and SHA256SUMS into a folder, as
  release.yml's Linux job does. sotd and sot-capsule are musl static when a musl compiler is present; with none it
  builds them for the gnu target and only on a host whose glibc is at most `FLEET_GLIBC_FLOOR` (the fleet's oldest
  glibc host, so the binaries run everywhere), else it refuses. `install.sh --archive` prints the LIBC line.
- `pack-build.ps1`: the same for Windows, as release.yml's Windows job does; `sotd update stage <folder> --prefix <prefix>`
  arms the folder for the supervisor's converge.
- `sot.svg`: the desktop-entry icon the sot-setup skill names.
- `install.ps1`: the Windows installer and update entry (Windows PowerShell 5.1); the prelude fetches its own tag's copy;
  `-Archive <folder>` installs a packed folder; ends with one verdict line (see the rule below).
- `install-shortcut.ps1`: Windows shortcut and taskbar pin; calls the manifest writer. A step of `install.ps1`
  (`-Prefix` passes its prefix); ends with an explicit `exit 0`.
- `install-manifest.ps1`: writes `install.json` for a Windows install (`daemon` and `frontend` true, since every Windows
  box runs its local daemon; keeping a recorded `pinned`, or setting it over a record it cannot parse) and runs the first topology sync, whose refusal it reports
  as a warning (the entry reads it). A `+` build gets a record only when the caller hands `-Commit` (`install.ps1` does,
  for a fresh install of a pack of the line, running the target checkout's own copy, never one beside the installer); a source
  checkout gets none. Called later without the commit (the shortcut step), it says it keeps the record that names the
  build; with no record it keeps the "not a release build - no manifest written" line. The record decision and the sync are separate: a `+` build with no record still syncs its hub.
- `set-shortcut-aumid.ps1`: stamps the window's AppUserModelID on a shortcut.
### Update-apply
- `sot-apply.sh`: offline apply and rollback of an armed update (Unix).
- `sot-apply.ps1`: the same for Windows (it loads `sot-install-layout.ps1` under a try for `Get-SotComparablePath`, which its flip check uses, and `Write-Utf8NoBomViaRename`, which writes `install.json`).
### Launch (Unix)
- `launch-sot.sh`: reads the topology plan, ensures the local daemon and execs the window with `--socket` for this computer's own daemon when the install runs one, plus one `--dial` per plan host other than this one (a dev checkout runs no daemon of its own and dials this host's plan line too).
### Launch (Windows)
- `launch-sot.ps1`: the supervisor: self-update, apply, handover, local daemon, respawn loop; it dot-sources the helper files below.
- `launch-splash.ps1`: the launch progress window.
- `sot-local-daemon.ps1`: starts and stops this computer's `sotd`, and names the `sotd.exe` it runs (`-Resolve`).
- `sot-hosts.ps1`: reads `sotd topology plan` output and runs `topology sync`; `Invoke-SotWithin`
  runs a native command under a wall-clock bound (every script ssh call): at the bound it ends the command with every
  process it started, and a command that cannot start is a result (127).
- `sot-install-layout.ps1`: `Invoke-SotNative`, how a script under `$ErrorActionPreference = 'Stop'` calls a native command, streaming a build's lines with `-Echo` (a native call in a local `Continue` scope does the same; Windows PowerShell 5.1 ends such a script on any stderr line when the caller redirects; the exit code is the answer), the pinned-checkout test, the shortcut target, the launcher code id, `Write-Utf8NoBomViaRename` (the rename write of `install.json` for `sot-apply.ps1` and `install-manifest.ps1`), `Get-SotComparablePath` (the one comparison of a path the OS reports with one this code built: a junction's target, a script's own path against a prefix that may be 8.3-spelled; `sot-apply.ps1` and `install.ps1` load the copy in the target checkout), the first-launch install layout and offline trust declaration delegation.
- `sot-freshness.ps1`: the armed update's apply, the dev pull's rebuild and the comm install (`Invoke-CommFreshness`: one stamp rule for a pinned install and a dev checkout, so Julia starts only when comm is missing or its stamp names another commit), run before a window starts.
- `sot-lease.ps1`: opens a lease on this computer's daemon, hands every lease over to the window, and starts the bridge (`Start-SotBridge`) for the lease and `sot-local-daemon.ps1`'s probe; `Start-SotRedirected` starts every child a launch script writes to (the bridge and every bounded run) and `Stop-SotTree` ends one with every process it started. A request the daemon has not answered within `$LeaseReplyWaitMs` is kept open, its bridge process held for the supervisor's life (`$global:SotLeasesUnanswered`), with its handover written behind it, since closing it could end the computer's sessions.
- `relaunch-sot.ps1`: builds the window and drops the relaunch sentinel.
- `shutdown-sot.ps1`: ordered local teardown; its two remote journal reads run under `Invoke-SotWithin`.
### Dev and docs
- `restart-backend.sh`: loads a freshly built `sotd` into a running dev daemon.
### Folders
- `lib/`: `sot-daemon.sh`, sourced by the installer, the launcher, `restart-backend.sh` and the rendered wrapper.
- `tests/`: shell and PowerShell suites and `rc-gate.sh`.

## Start here
Read `lib/sot-daemon.sh` for anything on the Unix launch path, `sot-apply.sh` for what an update does to `$PREFIX`,
`install.sh` for a first install, and `launch-sot.ps1` for Windows launch (its top-level order is the sequence of a
launch). A new suite joins a named step of `.github/workflows/rust.yml` in the same change (the install smokes join
`release.yml`).

## Rules
- On Windows the installer is the update entry too (`install.ps1`, run from the installed checkout's `repo\versions\<tag>`
  path or fetched as the one-liner). Its options are `-Version`, `-Archive`, `-Hub`, `-Prefix` and `-RestartDaemon` (parsed
  by hand from `$args`, so an option error ends in the same one verdict line), and every exit ends in exactly one `PASS`,
  `ONE STEP LEFT` or `FAIL` line, then an explicit `exit`: the one `try`/`finally` in `Invoke-InstallEntry` prints it,
  Ctrl+C included. The channel, the never-backwards direction (`sotd update direction`, asked of the target's own sotd
  before anything under the prefix changes), the `+` stop and the live-work ask (`sotd update repls`: a live REPL or the
  Pluto server stops the run with sotd's own reason, `-RestartDaemon` answers it) are install.sh's, in PowerShell. A
  daemon runs when its pipe exists or a process runs from `bin\sotd.exe`; one `same-version` cannot tell is `unknown`, so
  the ask fails closed and the end check is a FAIL, and a daemon of any version, the target's included, is asked before
  a path stops or restarts it. It reads `repo\current` by its target and works on `repo\versions\<tag>`, because an ssh
  session on a box with redirection trust cannot traverse the junction; the shortcut probe (`Invoke-Shortcut`, which runs
  `install-shortcut.ps1` through the junction when this session can) is its one deliberate traversal. A fresh install lays down `bin`, `repo\base`,
  `repo\versions\v<v>` and the junction, then the launcher's `Initialize-InstallLayout` and comm and the shortcut. An
  update stages with the target's own `sotd update stage` and applies by what runs here: a supervisor whose start line
  carries `code=` gets the converge sentinel (written by the installed checkout's `relaunch-sot.ps1 -Converge`, the one
  writer) and the entry waits for `sot-apply: APPLIED` and `status: DONE`; an older supervisor, or a window with none,
  leaves the update armed and the step is to quit and start from the shortcut (never a sentinel without a window); with
  nothing running a session at the desk (a non-zero session id; an ssh logon runs in session 0) applies it here, and
  another leaves it armed with the step to start at the desk. The end check compares the supervisor's `code=` with the
  installed launcher's code id: a window on older launcher code is ONE STEP LEFT. A box with no Julia is a supported
  state: the stage skips the Julia steps (`rust/backend/src/update.rs` `prepare_julia`; only `JuliaError::NotFound` is skipped, a Julia the resolver refused stays the stage's error) and the run ends ONE STEP LEFT
  naming `winget install --id Julialang.Juliaup -e`. The stage prints one "staging and preparing" line first and a failed
  one reports sotd's last line only. `SOT_INSTALL_REPO` names the release repo (default `kalidke/ship-of-tools`). A daemon the converge restarted must answer as the target within
  `SOT_INSTALL_DAEMON_WAIT_S` by the clock. A run that names what to install (`-Version`, `-Archive`; `install.sh`'s
  `--version`, `--archive`) pins the install: `install.json` records `"pinned": true`, the updaters' arm guards
  (`InstalledBuild::admits`) never arm over it ("pinned at <v>: automatic updates stay off for this install until a run
  that names no version", at the end of the `PASS` and `ONE STEP LEFT` lines of both entries), and a run that names none
  removes the pin (a record with no pin field is unpinned). A named run of the version already installed, with the daemon
  answering as it, is a repair run on both entries: it writes the pin, drops any update pointer armed over the install
  and stages, applies and restarts nothing (on Windows it fetches no release asset; the prelude still fetches the tag's
  `install.ps1` unless `SOT_INSTALL_TAG` is set). A `FAIL` ends with the pin once the run wrote it (both entries) and, on
  Windows, with "the update to <tag> stays armed: the next start applies it" when the run staged one that is still
  armed (`install.sh` arms none itself). `tests/test-install-entry.ps1` pins the URLs and the prelude, the verdicts, the
  daemon states (a renamed `sotd-stale-*.exe` image included), the pointer drop, the window check, the enroll refusal,
  the Julia outcomes (a refused Julia is a stage error, none found is a skip), the converge to its end, the stop of a
  running daemon before an apply, the untraversable junction, and the launcher's log lines it reads.
- Entry-point paths are an interface and do not move, and neither do the option names of `install.sh` (`--version`,
  `--archive`, `--local`, `--backend`, `--be-only`, `--prefix`, `--hub`, `--no-service`, `--force-role-change`) or of
  `install.ps1` (`-Version`, `-Archive`, `-Hub`, `-Prefix`, `-RestartDaemon`); a new option is added, never renamed: an agent's instructions name
  them. `install.sh` fetches `scripts/install.sh` of its own tag and `install.ps1` its tag's `scripts/install.ps1` (the preludes), shortcuts run `repo\current\scripts\launch-sot.ps1`, and `Get-SotLauncherCodeId` hashes
  `launch-sot.ps1`, `sot-hosts.ps1`, `sot-install-layout.ps1`, `sot-freshness.ps1` and `sot-lease.ps1`
  (`tests/test-install-layout.ps1` section 3). A new launcher file joins that list.
- A release is installed before it is published. `release.yml`'s `publish` needs the install smokes (`install-linux`,
  `install-macos`, `install-windows`), which run the entry of the run's own checkout against the artifacts the run built:
  a fresh install and an update from the newest published release older than the run's version (`release-before`, by
  version order, never GitHub's "latest"), each ending in the entry's PASS line, then a repair run
  (`tests/install-smoke.sh`, `tests/install-smoke.ps1`). The `update-apply` legs (on Windows the `update-entry` leg too,
  in a runner session 0) run the release before's installed apply over a release staged by the new `sotd`, and require
  that the staged release's own apply ran. The `update-implicit` legs install the release before by `--version`, then run
  the entry with no arguments against the real repository, as a lab box does: the channel resolves to the release before
  (this run's is still a draft), so the run is a repair run there that clears the pin, and the new `sotd` staging the run's
  release must say `newer` and arm. Every other leg ends in one state check: `install.json`, the checkout, the binaries,
  the installed `sot-apply`, the launcher (the wrapper `render_sot_launch` writes; `Get-SotLauncherCodeId` on Windows)
  and comm are the new release's, so a release that changes nothing in the launcher still passes. The archive's `sot-apply` must be executable (a mode lost in packing ends the hand-off), the Linux legs take
  `--no-service` (a container has no user manager), and a dispatch on a branch runs them too (its artifacts are named
  `0.0.0-ci.<sha>` and carry the binaries' `+` version; the smokes repack under it).
- Every file `launch-sot.ps1` dot-sources is in `Get-SotLauncherCodeId`'s list and in the converge parse loop of
  `launch-sot.ps1`.
- A dev clone's prelude re-execs only when `launch-sot.ps1` itself changed (`Invoke-SelfUpdatePrelude`), so a pull that
  changes only a helper takes effect at the next launch, while a converge compares every listed file.
- The launch path fails open. `sot-apply.sh` and `sot-apply.ps1` exit 0 by contract, the unit's
  `ExecStartPre=-` tolerates a missing script, and `launch-sot.sh` runs without `set -e`; `installer-apply.sh`'s
  `failed_apply_keeps_old_record_and_pending` and `test-sot-apply.ps1` pin the contract.
- Files are published whole: written beside the destination under a name of their own, then renamed
  (`sot_install_copy`, `render_sotd_unit`, `render_sot_launch`, `sot_boot_wants`). The `one_copy_helper` and
  `partial_wrapper_write_restores` cases of `installer-apply.sh` pin those distribution publishers. Folder-trust
  declaration publication belongs to agents; the agents → distribution connection defines exclusive creation,
  existing-file upgrades and the external-writer concurrency limit.
- Socket and pipe paths come from `sotd session-socket-path`; no script builds one.
- A launch script reaches a daemon's socket or pipe only through `sotd stdio-bridge --endpoint` (`sot_socket_open`,
  which `restart-backend.sh` also runs, `Test-SotPipeOpen`, `Open-SotLease`, whose lease names its bridge child), which
  connects only to an endpoint this OS account serves (ADR 0049 `## User isolation`; each path is shown by running it,
  in rust/backend/tests/shell_dial.rs and test-local-daemon-own.ps1's 4b); a script that finds this account's daemon in
  a process list lists only this account's processes, knows a program by the kernel's name for it (`ps -o ucomm`, never
  `comm`, which macOS fills from argv[0] and cuts to 16 characters in any column but the last), and compares the
  daemon's `--socket` or `--label` value whole and literally (`sot_daemon_pid` in lib/sot-daemon.sh; install.sh's
  `installer_process_at` knows a window or daemon by the same name and its argv0); on Windows the daemon is the pid the
  pipe's owner reports (`Get-SotPipeOwnerPid`, `GetNamedPipeServerProcessId`) when this account owns it, and no command
  line or name selects it (`Get-LocalDaemonProcess`; shutdown-sot.ps1's supervisor is the pid `launcher.pid` records).
  The frontend, tunnel and capsule counts in shutdown-sot.ps1 still match by image path and command line.
- A launch keeps this computer's daemon only when it runs the version of the binary the launch would start: the ensure
  (`sot_daemon_ensure`, `sot-local-daemon.ps1`) asks `sotd same-version` once the socket or pipe answers and replaces a
  daemon of another version (an apply swaps the binary, not the running process), the way a stop ends one, naming the
  daemon before it asks so a launch ends only the one it asked; while a window holds a lease (`sotd same-version` exit
  4, from the daemon's `held.json`) it keeps the daemon and the window's startup notice says `daemon out of date,
  restart when ready` (`SOT_ENSURE_NOTICE` to `SOT_LAUNCH_NOTICE` on Unix, a `NOTICE:` log line to the launcher's
  notices on Windows), and a replace's notice says the rows keep running and their REPLs restart; a daemon that cannot
  say is kept. The dev launcher (`launch-sot.sh` from a built checkout, no owned unit) probes with the checkout's own
  `sotd`, so it replaces an installed nohup daemon of another commit when no window holds a lease. The comm install follows the checkout the same way, by its stamp
  (`~/.sot-comm/VERSION`) against the checkout's commit (`Invoke-FreshnessPass`), not by whether this launch applied.
- This computer's local daemon has one binary on Windows: `sot-local-daemon.ps1`'s resolver chooses it (the complete
  dev pair, else the complete install pair), and the daemon's start, probes and pipe-name query and the launcher's
  own query and lease (`-Resolve`) all run that `sotd.exe`. `-Stop` makes the same choice when a complete pair exists;
  if neither pair is complete, it can use a lone `sotd.exe`, dev first, to query and probe without starting a capsule.
- On Windows every child a launch script writes to (the bridge, `Start-SotBridge`; every bounded run,
  `Invoke-SotWithin`) is started only by `Start-SotRedirected` (sot-lease.ps1), so its input carries exactly the bytes
  its caller writes: while it starts the process, the console's input encoding is UTF-8 without a preamble whenever
  the caller's has one, which Windows PowerShell 5.1 would otherwise write first.
- The bounds the launcher and the daemon share (`LAUNCH_WAIT` 160 s, `DAEMON_LOCK_WAIT` 150 s, the lease reply wait and
  the handover bound, all in `rust/protocol/src/ops/lease.rs`) are pinned by `launcher_bounds_match_ops` in
  `tests/installer-state.sh`; change both sides together.
- `sot_path_refused` (the one list of characters a prefix or home may not hold) has three byte-equal copies (`lib/sot-daemon.sh`,
  `install.sh`, `sot-apply.sh`), pinned by `one_path_predicate` in `tests/installer-unit-limits.sh`; edit all three.
- `sot_install_copy` has three byte-equal copies (`lib/sot-daemon.sh`, `install.sh`, `sot-apply.sh`), pinned by
  `one_copy_helper` in `tests/installer-apply.sh`; edit all three.
- install.sh's two cleanup traps follow comm/CLAUDE.md's rule: it arms itself (`trap - HUP INT TERM`) before each
  of its two `mktemp`s, each trap's body starts `trap "" HUP INT TERM` and ends with `sot_end_by_signal`, and
  `sot_cleanup_signals` runs right after each trap is set (a copy of the comm library's two helpers, beside `die`).
  sot-apply.sh's trap does not yet: its signal handling is the 0.6.6 item SOT-APPLY-SIGNAL.
- A script makes a temporary file or folder from a template in `${TMPDIR:-/tmp}` (`mktemp "${TMPDIR:-/tmp}/x.XXXXXX"`),
  never with a bare `mktemp`: macOS's, given no template, makes it in the account's own temporary folder (`getconf
  DARWIN_USER_TEMP_DIR`) whatever `TMPDIR` names, where a run that gives the script a `TMPDIR` of its own (the
  temp-files rows) does not look.
- install.sh's Julia steps (`julia_run`: the comm refresh and the three instantiates) run with `TMPDIR` set to its
  `WORK`, so what Julia leaves in its temporary folder when a person interrupts the run (Pkg's package download, which
  Pkg removes only at the end of the call that made it) goes with `WORK` at the exit trap. A SIGKILL of the run leaves
  `WORK`, as it leaves every scratch file of a killed script. Pkg's own half stays: a download it leaves in a folder
  nothing of ours made (the Windows first launch's, a person's own Pkg call) is Pkg's to remove.
- `sot-apply.sh` is POSIX sh plus `local` (it runs under dash and macOS's `/bin/sh`); the other `.sh` files are bash and
  run under bash 3.2, macOS's `/bin/bash` (checked by `tests/under-bash.sh`), where a utility run by the `command`
  builtin ends a `set -e` shell when it fails, even inside the `if`, `!` or `||` that handles it (install.sh's `git`
  function runs git through `env`); `.ps1` files are Windows
  PowerShell 5.1 with ASCII-only string literals, parsed under 5.1 by the "Parse PowerShell scripts" step of
  `.github/workflows/rust.yml`.
