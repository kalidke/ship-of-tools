# rust/backend/src/rows/store: the row toml store and its boot migrations (rows)

The row toml is a projection of a row that the daemon rewrites: one file per row under the per-host dir
`workspaces-<host>/`, with the older `sessions-<host>/` read for migration only. This folder scans, loads and saves
those files, owns the hand toml codec they use and the three migrations that run at boot. Part of the daemon's rows
subsystem; charter: `rust/backend/src/rows/CLAUDE.md`.

## Files
- `mod.rs`: `scan_disk`, `scan_dir`, `load_toml`, `save`, `declared_host` and the config-dir paths (`workspaces_dir`, `toml_path_for`, `legacy_toml_path_for`, `sessions_dir`, `app_config_dir`, `check_config_dir`)
- `codec.rs`: the hand toml reader and writer: `parse_kv`, `parse_section`, `strip_canonical_top_and_kernel`, `toml_quote`, `toml_unquote`
- `migrate.rs`: `migrate_legacy_state_dirs` with its fold, and the Windows legacy config-dir move
- `tests.rs`: load, save and scan cases, the round trips and the config-dir cases
- `support_tests.rs`: `EnvGuard` and `env_guarded`, which serialize and restore the process env for the tests

## Start here
`mod.rs::scan_disk` for how rows come back at boot, `save` for how a row is written; `codec.rs` for the file format.

## Rules
- Every save is a durable replace: `save` writes through `crate::durable::write` and keeps the sections the frontend
  owns (`[nav_state]`, `[layout]`).
- A legacy toml never overrides a canonical row of the same slug: `scan_dir` skips it when `has_slug` is true.
- The Windows legacy registry moves only with the canonical flag: `scan_disk` calls
  `migrate_legacy_windows_config_dir` only when `adopt_legacy_registry` is true.
- The reader and writer escape as a pair: `toml_quote` writes what `toml_unquote` undoes on load.
- The state dirs are per host: `workspaces_dir` and `sessions_dir` carry `declared_host()`, so one box never reads
  another's rows.
