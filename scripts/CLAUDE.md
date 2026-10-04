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
- Release to updater: a `v*` tag runs `.github/workflows/release.yml`; `rust/updater` fetches the assets and checks
  `SHA256SUMS`, and `release.sh` orders tags with the updater's own `compare_versions` (`vercmp` example).
- Updater to apply: the Rust stager writes `updates/pending-<target>.json` and a stage folder, and holds the
  `updates/.lock` folder that `sot-apply.sh` and `sot-apply.ps1` also take. Callers of apply: the unit's
  `ExecStartPre=-`, the rendered `sot-launch` wrapper, and `launch-sot.ps1` (`Invoke-PendingApply`).
- Launch to daemon: `sotd session-socket-path`, `sotd topology plan|sync|status`, the `fe.lease` and `fe.leaving`
  frames, and exit codes 0, 75 and 76 from the window.
- Launch to window: arguments (`--socket`, `--dial`, `--relaunched`), the `relaunch.request` sentinel written by
  `relaunch-sot.ps1` and watched by `rust/frontend/src/relaunch.rs`.
- Install to launch: on Unix `install.sh` renders the `sot-launch` wrapper once and the wrapper reads
  `lib/sot-daemon.sh` from `repo/current` at each start; on Windows `launch-sot.ps1` re-runs the install steps at
  every launch.
- Install to comm: `install.sh` and the launchers call `ShipTools.update_comm()`.

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
- `sot-local-daemon.ps1`: starts and stops this computer's `sotd`.
- `sot-hosts.ps1`: reads `sotd topology plan` output and runs `topology sync`.
- `sot-install-layout.ps1`: the pinned-checkout test, the shortcut target, the launcher code id and the first-launch install layout.
- `sot-freshness.ps1`: the armed update's apply, the dev pull's rebuild and the comm install, run before a window starts.
- `sot-lease.ps1`: opens a lease on this computer's daemon and hands every lease over to the window.
- `relaunch-sot.ps1`: builds the window and drops the relaunch sentinel.
- `shutdown-sot.ps1`: ordered local teardown.
### Dev and docs
- `restart-backend.sh`: loads a freshly built `sotd` into a running dev daemon.
### Folders
- `lib/`: `sot-daemon.sh`, sourced by the installer, the launcher and the rendered wrapper.
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
- The bounds the launcher and the daemon share (`LAUNCH_WAIT` 160 s, `DAEMON_LOCK_WAIT` 150 s, the lease reply wait and
  the handover bound, all in `rust/protocol/src/ops.rs`) are pinned by `launcher_bounds_match_ops` in
  `tests/installer-state.sh`; change both sides together.
- `sot_install_copy` has three byte-equal copies (`lib/sot-daemon.sh`, `install.sh`, `sot-apply.sh`), pinned by
  `one_copy_helper` in `tests/installer-apply.sh`; edit all three.
- `.sh` files are POSIX sh plus `local` (`sot-apply.sh` runs under dash, macOS ships bash 3.2); `.ps1` files are Windows
  PowerShell 5.1 with ASCII-only string literals, parsed under 5.1 by the "Parse PowerShell scripts" step of
  `.github/workflows/rust.yml`.
