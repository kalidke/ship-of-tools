# scripts/: distribution (charter)

## Idea
A box runs exactly one tagged release. CI turns a tag into verified artifacts; install, update-apply and launch place,
swap and start that release as whole files under `$PREFIX` (`~/.local/share/sot`, or `%LOCALAPPDATA%\sot` on Windows),
and the launch path fails open: no update step can stop a window from starting.

## Owns
- The release cut and CI: `release.sh`, `tests/rc-gate.sh`, `.github/`.
- Install and the Unix update entry: `install.sh` (Linux and macOS; "update sot from the repo" runs it); on Windows `install-shortcut.ps1` then `install-manifest.ps1`, plus
  `Initialize-InstallLayout` (in `sot-install-layout.ps1`, run by `launch-sot.ps1`).
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
- A tag is cut only from `main`, `fixes/*` or `rc/*`, sorts above every tag of its line, and has green `rust.yml` and
  `CI.yml` runs at HEAD (`release.sh` preflight).
- Install checksums verify before any write, and `install.json` is written last (`install.sh` steps 2 and 9).
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
  direction` says `older` (or cannot say) and the run fails, before anything under the prefix changes. `--archive` runs
  the `install.sh` of the checkout that made the pack. With no role flag and no topology entry the role comes back from
  `install.json` (its bits, its `service`, its `hub`; a remote install's alias from its wrapper). A daemon is restarted
  only when it runs another version (`sotd same-version`) and only an active unit no window holds; the restart, and a
  fresh start of the unit, are verified: the daemon must answer as the target within `SOT_INSTALL_RESTART_WAIT_S`, and
  an active unit whose daemon is in any other state is `FAIL: daemon:`, never PASS. Every daemon on another version is
  asked `sotd update repls` (the daemon's own `workspace.list`) before anything changes. When the unit would be restarted
  and a row has a live REPL, or the daemon cannot be asked, the run stops first and asks for `--restart-daemon`, changing
  nothing under the install. A held unit and a launcher-started daemon are not restarted: the `ONE STEP LEFT` names the
  live rows and the command that fits (the launcher, `systemctl --user restart sotd.service`, or by hand), and says the
  old daemon runs beside the new `repo/current` and `bin/sot-capsule` until then. A box the topology plan names, with
  another daemon host in the plan, gets the launcher that dials the plan (`launch-sot.sh`) whether or not `--backend` is
  given again; without `--daemon` for this computer in the plan, its own rows do not show in its window. A `--backend`
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
  `http.lowSpeedTime=60`, curl with `CURL_BOUND` (connect 15 s, under 1000 B/s for 60 s).

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
`sotd topology plan|sync|status`, `sotd update direction|repls`, `sotd session-socket-path`, `launch-sot.sh`, `Get-SotTopologyPlan`,
`scripts/lib/sot-daemon.sh`, `sotd stdio-bridge`, `Leases::commit_update`, `julia::resolve_bin`, `check_remote_fs`,
`scripts/install.sh`, `REMOTE_FS_TYPES`, `docs/make.jl`, `.github/workflows/CI.yml`,
`sotd trust declare <absolute-prefix>`, `sotd topology pin`, `HeldDir`, `remove_ended`.

## Folders
- `scripts/lib/`: the shared Unix launch library.
- `scripts/tests/`: the suites and the local candidate gate.
- `deploy/`: `sotd.service`, the unit template (no page; its header is its page).
- `.github/`: workflows and the tag gate.
- `src/`, `test/`: the comm installer package.
- `rust/updater/`: update discovery, staging and the pending pointer.
- `rust/backend/src/update.rs`: the daemon's update ops.
- `rust/frontend/src/selfupdate.rs`: the window's update check and banner.

## Files
### Release
- `release.sh`: stamps the version, runs the workspace tests, commits and tags the cut, stamps the line's next -dev
  version in its own commit, and pushes both.
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
- `install-shortcut.ps1`: Windows shortcut and taskbar pin; calls the manifest writer.
- `install-manifest.ps1`: writes `install.json` for a Windows install and runs the first topology sync.
- `set-shortcut-aumid.ps1`: stamps the window's AppUserModelID on a shortcut.
### Update-apply
- `sot-apply.sh`: offline apply and rollback of an armed update (Unix).
- `sot-apply.ps1`: the same for Windows.
### Launch (Unix)
- `launch-sot.sh`: reads the topology plan, ensures the local daemon and execs the window with `--dial` arguments.
### Launch (Windows)
- `launch-sot.ps1`: the supervisor: self-update, apply, handover, local daemon, respawn loop; it dot-sources the helper files below.
- `launch-splash.ps1`: the launch progress window.
- `sot-local-daemon.ps1`: starts and stops this computer's `sotd`, and names the `sotd.exe` it runs (`-Resolve`).
- `sot-hosts.ps1`: reads `sotd topology plan` output and runs `topology sync`; `Invoke-SotWithin`
  runs a native command under a wall-clock bound (every script ssh call): at the bound it ends the command with every
  process it started, and a command that cannot start is a result (127).
- `sot-install-layout.ps1`: the pinned-checkout test, the shortcut target, the launcher code id, the first-launch install layout and offline trust declaration delegation.
- `sot-freshness.ps1`: the armed update's apply, the dev pull's rebuild and the comm install, run before a window starts.
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
launch). A new suite joins a named step of `.github/workflows/rust.yml` in the same change.

## Rules
- Entry-point paths are an interface and do not move, and neither do the option names of `install.sh` (`--version`,
  `--archive`, `--local`, `--backend`, `--be-only`, `--prefix`, `--hub`, `--no-service`, `--force-role-change`; a new
  option is added, never renamed): an agent's instructions name them. `install.sh` fetches `scripts/install.sh` of its own tag
  (the prelude before step 0), shortcuts run `repo\current\scripts\launch-sot.ps1`, and `Get-SotLauncherCodeId` hashes
  `launch-sot.ps1`, `sot-hosts.ps1`, `sot-install-layout.ps1`, `sot-freshness.ps1` and `sot-lease.ps1`
  (`tests/test-install-layout.ps1` section 3). A new launcher file joins that list.
- Every file `launch-sot.ps1` dot-sources is in `Get-SotLauncherCodeId`'s list and in the converge parse loop of
  `launch-sot.ps1`.
- A dev clone's prelude re-execs only when `launch-sot.ps1` itself changed (`Invoke-SelfUpdatePrelude`), so a pull that
  changes only a helper takes effect at the next launch, while a converge compares every listed file.
- The launch path fails open. `sot-apply.sh` and `sot-apply.ps1` exit 0 by contract, the unit's
  `ExecStartPre=-` tolerates a missing script, and `launch-sot.sh` runs without `set -e`; `installer-apply.sh`'s
  `failed_apply_keeps_old_record_and_pending` and `test-sot-apply.ps1` pin the contract.
- Files are published whole: written beside the destination under a name of their own, then renamed
  (`sot_install_copy`, `render_sotd_unit`, `render_sot_launch`). The `one_copy_helper` and
  `partial_wrapper_write_restores` cases of `installer-apply.sh` pin those distribution publishers. Folder-trust
  declaration publication belongs to agents; the agents → distribution connection defines exclusive creation,
  existing-file upgrades and the external-writer concurrency limit.
- Socket and pipe paths come from `sotd session-socket-path`; no script builds one.
- A launch script reaches a daemon's socket or pipe only through `sotd stdio-bridge --endpoint` (`sot_socket_open`,
  which `restart-backend.sh` also runs, `Test-SotPipeOpen`, `Open-SotLease`, whose lease names its bridge child), which
  connects only to an endpoint this OS account serves (ADR 0049 `## User isolation`; each path is shown by running it,
  in rust/backend/tests/shell_dial.rs and test-local-daemon-own.ps1's 4b); a script that finds this account's daemon in
  a process list lists only this account's processes and compares the daemon's `--socket` or `--label` value whole and
  literally (`sot_daemon_pid` in lib/sot-daemon.sh, `Get-LocalDaemonProcess`).
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
- `sot_install_copy` has three byte-equal copies (`lib/sot-daemon.sh`, `install.sh`, `sot-apply.sh`), pinned by
  `one_copy_helper` in `tests/installer-apply.sh`; edit all three.
- install.sh's two cleanup traps follow comm/CLAUDE.md's rule: it arms itself (`trap - HUP INT TERM`) before each
  of its two `mktemp`s, each trap's body starts `trap "" HUP INT TERM` and ends with `sot_end_by_signal`, and
  `sot_cleanup_signals` runs right after each trap is set (a copy of the comm library's two helpers, beside `die`).
  sot-apply.sh's trap does not yet: its signal handling is the 0.6.6 item SOT-APPLY-SIGNAL.
- `sot-apply.sh` is POSIX sh plus `local` (it runs under dash and macOS's `/bin/sh`); the other `.sh` files are bash and
  run under bash 3.2, macOS's `/bin/bash` (checked by `tests/under-bash.sh`); `.ps1` files are Windows
  PowerShell 5.1 with ASCII-only string literals, parsed under 5.1 by the "Parse PowerShell scripts" step of
  `.github/workflows/rust.yml`.
