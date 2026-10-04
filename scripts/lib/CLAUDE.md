# scripts/lib/: the Unix launch library (distribution)

The one place the `sotd` unit, the all-in-one `sot-launch` wrapper and the backend ensure are written down. `install.sh`,
`sot-apply.sh` and `launch-sot.sh` source it from the checkout, and the rendered wrapper sources it from
`$PREFIX/repo/current` at every start, so a rollback restores the wrapper and its library together. Part of
distribution; charter: scripts/CLAUDE.md.

## Files
- `sot-daemon.sh`: the unit and wrapper renderers, the daemon ensure, the log pruner and the copy and backup helpers.
- `sot-hosts.sh`: the `sotd topology plan` reader (`sot_topology_plan`) and the `topology sync` runner; `launch-sot.sh` sources it.

## Start here
`sot-daemon.sh`, in this order: `render_sot_launch` (the wrapper text), `sot_daemon_ensure` (what every launch does
before a window), then `sot_rerender_owned` (what an update does to the unit and the wrapper).

## Rules
- `sot-daemon.sh` is POSIX sh plus `local`: no arrays, `[[`, `declare`, `$'...'`, `function` or `==` in `[`, because
  `sot-apply.sh` runs it under dash and macOS ships bash 3.2.
- `render_sotd_unit` and `render_sot_launch` write beside the destination under a name of the shell's own pid and move
  it in, so a failed write leaves the old file whole and returns 1.
- `sot_install_copy` is byte-equal to the copies in `scripts/install.sh` and `scripts/sot-apply.sh`; edit all three
  (`one_copy_helper` in `scripts/tests/installer-apply.sh`). `sot_unit_owner_path` and `sot_wrapper_owner_prefix` are
  copies of `installer_unit_owner_path` and `installer_wrapper_owner_prefix` in `install.sh`, pinned equal by
  `owner_helpers_agree`.
- `SOT_LAUNCH_WAIT_S` equals `lease::LAUNCH_WAIT` in `rust/protocol/src/ops/lease.rs` (`launcher_bounds_match_ops`). The
  ensure's wait stays longer than the daemon's own lock wait, so `sot_daemon_ensure` never kills a daemon it started
  and never removes the socket.
- A rendered unit or wrapper is the install's own only when `sot_service_owned` or `sot_wrapper_owned` says so (the
  prefix embedded in it matches); `sot_rerender_owned` and `sot_backup_owned` touch no other file.
- `sot_prune_logs` keeps a log while the pid in its name is alive or while it is the newest, and a failed `rm` is
  logged and skipped, never fatal (`SOT_LOG_KEEP`, `SOT_LOG_CAP_BYTES`).
