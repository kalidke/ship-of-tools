# scripts/: distribution (charter)

## Idea
A box runs exactly one tagged release. CI turns a tag into verified artifacts; install, update-apply and launch place,
swap and start that release as whole files under `$PREFIX` (`~/.local/share/sot`, or `%LOCALAPPDATA%\sot` on Windows),
and the launch path fails open: no update step can stop a window from starting.

## Owns
- The release cut and CI: `release.sh`, `tests/rc-gate.sh`, `.github/`.
- Install: `install.sh` (Linux and macOS); on Windows `install-shortcut.ps1` then `install-manifest.ps1`, plus
  `Initialize-InstallLayout` (in `sot-install-layout.ps1`, run by `launch-sot.ps1`).
- Update discovery and staging: `rust/updater`, `rust/backend/src/update.rs` (the `update.check` and `update.apply`
  ops), `rust/frontend/src/selfupdate.rs`.
- Apply: `sot-apply.sh`, `sot-apply.ps1`.
- Launch and supervision: `lib/sot-daemon.sh`, `launch-sot.sh`, `launch-sot.ps1`, `sot-local-daemon.ps1`,
  `sot-hosts.*`, `relaunch-sot.ps1`, `shutdown-sot.ps1`, `deploy/sotd.service`.
- The comm installer `ShipTools.update_comm()` (`src/`, `test/`), `~/.sot-comm/bin`, and the Cargo workspace
  manifest.
- On disk: `$PREFIX` with `bin/` (plus `.prev` copies), `repo/{base,versions/<tag>,current}`, `install.json`,
  `updates/`, `logs/`.

## Promises
- A tag is cut only from `main`, `fixes/*` or `rc/*`, sorts above every tag of its line, and has green `rust.yml` and
  `CI.yml` runs in HEAD's history (`release.sh` preflight).
- Install checksums verify before any write, and `install.json` is written last (`install.sh` steps 2 and 9).
- Apply verifies everything before it mutates, restores on a failure after mutation and exits 0; a rollback marks the
  bad tag so it is never armed again (`sot-apply.sh`, `sot-apply.ps1`).
- The launch path never stops on an update: every update or freshness step logs and the window still starts.
- One launcher per user, and no window spawns without a lease held on the local daemon across the gap (Windows:
  `launch-sot.ps1`, `Open-SotLease`).
- The comm installer publishes by copy then rename, prunes only names it recorded and writes `VERSION` last
  (`install_comm` in `src/comm.jl`).

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `--socket`, `--dial`,
`--relaunched`, `relaunch.request`, `spawn_watcher`, `rust/frontend/src/relaunch.rs`, `deploy/sotd.service`,
`sot-apply.sh`, `install_comm`, `update_comm`, `comm/bin-folders.txt`, `src/sources.jl`, `~/.sot-comm/bin`,
`scripts/install.sh`, `docs/INSTALL-AGENT.md`, `v*`, `.github/workflows/release.yml`, `rust/updater`, `SHA256SUMS`,
`scripts/release.sh`, `compare_versions`, `updates/pending-<target>.json`, `updates/.lock`, `sot-apply.ps1`,
`ExecStartPre=-`, `sot-launch`, `Invoke-PendingApply`, `render_sot_launch`, `launch-sot.ps1`. Uses: `fe.lease`,
`fe.leaving`, `scripts/sot-lease.ps1`, `launcher_bounds_match_ops`, `scripts/tests/installer-state.sh`,
`is_release_build`, `rust/backend/src/update.rs`, `rust/frontend/src/selfupdate.rs`, `version_line`, `--version`,
`sotd topology plan|sync|status`, `sotd session-socket-path`, `launch-sot.sh`, `Get-SotTopologyPlan`,
`scripts/lib/sot-daemon.sh`, `sotd stdio-bridge`, `Leases::while_open`, `julia::resolve_bin`, `check_remote_fs`,
`scripts/install.sh`, `REMOTE_FS_TYPES`, `docs/make.jl`, `.github/workflows/CI.yml`.

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
- `release.sh`: stamps the version, runs the workspace tests, commits, tags and pushes.
### Install
- `install.sh`: the Unix installer; the one-liner fetches its own tag's copy.
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
- `sot-hosts.ps1`: reads `sotd topology plan` output and runs `topology sync`.
- `sot-install-layout.ps1`: the pinned-checkout test, the shortcut target, the launcher code id and the first-launch install layout.
- `sot-freshness.ps1`: the armed update's apply, the dev pull's rebuild and the comm install, run before a window starts.
- `sot-lease.ps1`: opens a lease on this computer's daemon, hands every lease over to the window, and starts the bridge (`Start-SotBridge`) for the lease and `sot-local-daemon.ps1`'s probe.
- `relaunch-sot.ps1`: builds the window and drops the relaunch sentinel.
- `shutdown-sot.ps1`: ordered local teardown.
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
- Entry-point paths are an interface and do not move: `install.sh` fetches `scripts/install.sh` of its own tag
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
  (`sot_install_copy`, `render_sotd_unit`, `render_sot_launch`; `Set-SotFolderTrust` on Windows). The
  `one_copy_helper` and `partial_wrapper_write_restores` cases of `installer-apply.sh` pin it.
- Socket and pipe paths come from `sotd session-socket-path`; no script builds one.
- A launch script reaches a daemon's socket or pipe only through `sotd stdio-bridge --endpoint` (`sot_socket_open`,
  which `restart-backend.sh` also runs, `Test-SotPipeOpen`, `Open-SotLease`, whose lease names its bridge child), which
  connects only to an endpoint this OS account serves (ADR 0049 `## User isolation`; each path is shown by running it,
  in rust/backend/tests/shell_dial.rs and test-local-daemon-own.ps1's 4b); a script that finds this account's daemon in
  a process list lists only this account's processes and compares the daemon's `--socket` or `--label` value whole and
  literally (`restart-backend.sh`, `Get-LocalDaemonProcess`).
- This computer's local daemon has one binary on Windows: `sot-local-daemon.ps1`'s resolver chooses it (the complete
  dev pair, else the complete install pair), and the daemon's start, probes and pipe-name query and the launcher's
  own query and lease (`-Resolve`) all run that `sotd.exe`. `-Stop` makes the same choice when a complete pair exists;
  if neither pair is complete, it can use a lone `sotd.exe`, dev first, to query and probe without starting a capsule.
- On Windows the bridge is started only by `Start-SotBridge` (sot-lease.ps1), so its input carries exactly the bytes
  its caller writes: while it starts the process, the console's input encoding is UTF-8 without a preamble whenever
  the caller's has one, which Windows PowerShell 5.1 would otherwise write first.
- The bounds the launcher and the daemon share (`LAUNCH_WAIT` 160 s, `DAEMON_LOCK_WAIT` 150 s, the lease reply wait and
  the handover bound, all in `rust/protocol/src/ops/lease.rs`) are pinned by `launcher_bounds_match_ops` in
  `tests/installer-state.sh`; change both sides together.
- `sot_install_copy` has three byte-equal copies (`lib/sot-daemon.sh`, `install.sh`, `sot-apply.sh`), pinned by
  `one_copy_helper` in `tests/installer-apply.sh`; edit all three.
- `.sh` files are POSIX sh plus `local` (`sot-apply.sh` runs under dash, macOS ships bash 3.2); `.ps1` files are Windows
  PowerShell 5.1 with ASCII-only string literals, parsed under 5.1 by the "Parse PowerShell scripts" step of
  `.github/workflows/rust.yml`.
