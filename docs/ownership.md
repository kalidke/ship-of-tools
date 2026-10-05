# Who owns what: one owner per concept

Every concept has exactly one owning subsystem. The subsystems are [wire](../rust/protocol/CLAUDE.md),
[topology](../rust/protocol/src/topology/CLAUDE.md), [server](../rust/backend/src/server/CLAUDE.md),
[lifecycle](../rust/backend/src/lifecycle/CLAUDE.md), [rows](../rust/backend/src/rows/CLAUDE.md),
[agents](../agents/CLAUDE.md), [capsule](../rust/log/CLAUDE.md), [platform](../rust/log/src/host/CLAUDE.md),
[messaging](../comm/CLAUDE.md), [files](../rust/backend/src/files/CLAUDE.md),
[sidecars](../rust/backend/src/sidecars/CLAUDE.md), [pages](../rust/backend/src/pages/CLAUDE.md),
[fe-ui](../rust/frontend/src/ui/CLAUDE.md), [fe-net](../rust/frontend/src/net/CLAUDE.md),
[distribution](../scripts/CLAUDE.md) and [records](CLAUDE.md); each link is the subsystem's charter page.
"Lives in" names repo paths, each followed by the item that holds the concept (a function, struct, constant, shell
function or file name). Runtime paths keep placeholders: `<state>` is `sot_state_dir()` (`$XDG_STATE_HOME/sot`, else
`<home>/.local/state/sot`; on Windows `%LOCALAPPDATA%\sot`), `<config>` is `sot_config_dir()` (`<home>/.config/sot`; on
Windows `%LOCALAPPDATA%\sot\config`), `<runtime>` is the private runtime directory, `<sd>` is a row's capsule state
directory and `<home>` is the user's home. Where a concept has a second copy today, the row names it and the section
"Two owners today" lists the pair.

Rule for reading the table: the owner decides the concept's meaning and is the only writer, or holds the one function
every writer calls. A bus, file or op belongs to the subsystem whose invariant it serves, not to the code that happens
to route it. A shell, PowerShell or Julia twin of a Rust rule is owned by the rule's owner.

## Owners, by concept

| Concept | Kind | Owner | Lives in |
|---|---|---|---|
| state-dir rule `<state>` | disk rule | platform | `rust/log/src/host/state_dir.rs` `sot_state_dir` (second copy `rust/backend/src/paths.rs` `state_dir`) |
| config-dir rule `<config>` | disk rule | platform | `rust/log/src/host/state_dir.rs` `sot_config_dir` (delegated `rust/backend/src/rows/store/mod.rs` `app_config_dir`; third rule `rust/frontend/src/ui/persist/resume.rs` `config_dir`; fourth `rust/frontend/src/ui/persist/discover.rs` `find_config_file`) |
| runtime dir `<runtime>`, `SOT_RUNTIME_DIR` | disk rule | platform | `rust/log/src/host/state_dir.rs` `runtime_dir` |
| host name `host_name()`, `SOT_SELF_HOST` | machine fact | platform | `rust/log/src/host/state_dir.rs` `host_name` (copies: `rust/backend/src/comm/mail/filer.rs` `comm_self_host`, `rust/frontend/src/ui/persist/resume.rs` `state_path`, `comm/lib/comm-lib-base.sh` `sot_host`, `comm/registry/comm-context.sh`, `agents/spawn/comm-despawn.sh`) |
| `state_dir_hash` (lane socket names) | machine fact | platform | `rust/log/src/host/state_dir.rs` `state_dir_hash` |
| durable write (fsync, no-clobber rename, publish) | primitive | platform | `rust/log/src/host/durable.rs` `publish_noreplace`, `fsync_dir`, `ensure_container`; `rust/backend/src/durable.rs` `write` |
| bounded file locks | primitive | platform | `rust/log/src/host/lock.rs` `lock_writer`, `lock_supervisor`, `try_lock_daemon` |
| volume preflight | primitive | platform | `rust/log/src/host/volume.rs` `preflight_volume` |
| Windows SDDL/SID helpers | primitive | platform | `rust/log/src/host/winsec.rs` `owner_protected_descriptor`, `sid_string_from_process` |
| peer challenge (OS peer, then wire identity) | primitive | platform | `rust/log/src/identity/challenge.rs` `exchange_identity`; `rust/log/src/identity/exchange.rs`; `rust/log/src/identity/deadline.rs` `run_with_deadline`; per-OS `rust/log/src/identity/challenge_unix.rs`, `rust/log/src/identity/challenge_win.rs`, `rust/log/src/identity/challenge_macos.rs` |
| private-dir and socket-dir checks | primitive | platform | `rust/backend/src/paths.rs` `ensure_private_dir`, `secure_private_dir`, `secure_socket_dir` |
| resource dir, `SOT_RESOURCE_ROOT` | machine fact | platform | `rust/backend/src/paths.rs` `resource_dir` |
| `\\?\` verbatim strip | primitive | platform | `rust/backend/src/paths.rs` `simplify_verbatim` |
| `<state>/daemon.lock` | disk, lock | server | `rust/backend/src/server/listen.rs` `take_daemon_lock`, `lock_daemon`; primitive `rust/log/src/host/lock.rs` `try_lock_daemon`|
| `<state>/sotd.log` (Windows `<state>\state\sotd.log`) | disk | server | `rust/backend/src/main.rs` `TeeWriter`; path `rust/backend/src/paths.rs` `state_dir` |
| umask 077, boot refusals | process rule | server | `rust/backend/src/main.rs` `apply_umask`, `parse_args` |
| session socket/pipe path rule `<runtime>/sessions/<label>.sock`, `\\.\pipe\sot-<USER>-<label>` | disk rule | topology | `rust/protocol/src/topology/endpoint.rs` `session_socket_path` |
| local daemon label (`sot`, `local`) | setting | topology | `rust/protocol/src/topology/endpoint.rs` `local_daemon_label`; spelled at several script sites (see two owners) |
| the bound session socket/pipe, DACL, live-socket refusal | disk, endpoint | server | `rust/backend/src/server/listen.rs` `run_local`, `refuse_live_socket`, `session_pipe_security_descriptor` |
| `<state>/held.json` | disk | lifecycle | `rust/backend/src/lifecycle/lease.rs` `HeldRecord`, `persist`; name `rust/protocol/src/ops/lease.rs` `HELD_RECORD_FILE` |
| `<state>/relaunch.request` | disk | distribution | `rust/frontend/src/relaunch.rs` `relaunch_sentinel_path`, `spawn_watcher`; writer `scripts/relaunch-sot.ps1` |
| `<state>/fe-commands/` | disk | fe-ui | `rust/frontend/src/ui/control/file_channel.rs`; `rust/frontend/src/ui/control/command.rs` |
| `<state>/fe-state.json` | disk | fe-ui | `rust/frontend/src/ui/control/file_channel.rs` `fe_state_path` |
| `<state>/session-<host>.json` | disk | fe-net | `rust/frontend/src/net/state.rs` `state_path`, `SessionMemory` |
| `<config>/state-<host>.toml` (window resume) | disk | fe-ui | `rust/frontend/src/ui/persist/resume.rs` `state_path`, `GlobalState` |
| `<config>/workspaces-<host>/<slug>.toml` | disk | rows | `rust/backend/src/rows/store/mod.rs` `workspaces_dir`, `toml_path_for`, `save` |
| `<config>/sessions-<host>/<label>.toml` (the older row-file shape) | disk | rows | `rust/backend/src/rows/store/mod.rs` `sessions_dir`, `scan_disk` |
| boot migrations of the row store (Windows config dir, unsuffixed fold, sessions read) | disk | rows | `rust/backend/src/rows/store/migrate.rs` `migrate_legacy_state_dirs`; `rust/backend/src/rows/store/mod.rs` `scan_disk` |
| `<config>/settings.toml`: the file and every section but `[trust]` | disk, setting | fe-ui | `rust/frontend/src/ui/persist/settings.rs` `Settings`, `load_layered`; `rust/frontend/src/ui/persist/discover.rs` `find_config_file` |
| `[trust] root_prefix` in settings.toml | setting | agents | `rust/backend/src/agents/folder_trust.rs` `declared_root_prefix`, `TRUSTED_ROOT_PREFIX_SECTION`; writers `scripts/install.sh`, `scripts/sot-install-layout.ps1` |
| `<config>/keybindings.toml` | disk, setting | fe-ui | `rust/frontend/src/ui/input/keybindings.rs` `KeyBindings`, `load_layered` |
| `<config>/hosts.toml`, `SOT_HOSTS` | disk, setting | topology | `rust/protocol/src/topology/mod.rs` `locate`, `load`, `parse` |
| `[monitor]` roster in hosts.toml (the grammar is topology's; the sidecars read it) | setting | topology | `rust/protocol/src/topology/mod.rs` `monitor_targets`; `rust/backend/src/sidecars/monitor.rs` `load_hosts`, `sampling_roster` |
| `<config>/data-roots` | disk | pages | `rust/backend/src/pages/site/links.rs` `read_data_roots`, `DataRoots` |
| a row's capsule state dir `<state>/workspaces/<id>` (name) | disk rule | rows | `rust/backend/src/rows/spawn/state_root.rs` `state_dir_for` |
| `<sd>/supervisor.lock` | disk, lock | capsule | `rust/log/src/supervisor/journal/fence.rs` `lock_supervisor`; `rust/log/src/host/lock.rs` `lock_supervisor` |
| `<sd>/drawer.voyage` (pointer) | disk | capsule | `rust/log/src/supervisor/journal/pointer.rs` |
| `<sd>/supervisor-journal/` | disk | capsule | `rust/log/src/supervisor/journal/mod.rs` `begin`, `finish` |
| `<sd>/voyages/<id>/{writer.lock, seg/, blobs/sha256/}` | disk, lock | capsule | `rust/log/src/store/voyage.rs`; names repeated `rust/backend/src/rows/run/end_run.rs` `absence_proof` |
| `<sd>/row-scopes` and the row's systemd scope `sot-row-<hash>-<uuid>.scope` | disk, process | rows | `rust/backend/src/rows/spawn/row_scope.rs` `capture`, `listed`, `end`, `unit_name`; `rust/backend/src/rows/spawn/row_scope_aim.rs` `aim` |
| supervisor lane socket/pipe `supervisor-<hash>` | endpoint | capsule | `rust/log/src/lane/socket_unix/mod.rs`; `rust/log/src/lane/pipe_win/mod.rs` |
| voyage lane socket/pipe `voyage-<id>` | endpoint | capsule | `rust/log/src/lane/socket_unix/mod.rs`; `rust/log/src/lane/pipe_win/mod.rs` |
| hub relay sockets `sot-host-<h>.sock`, relay units and the `zz-sot-relay-command.conf` drop-in | disk, endpoint | topology | `rust/protocol/src/topology/mod.rs` `relay_socket_path`, `runtime_relay_dir`; `rust/protocol/src/topology/relay_units.rs` `relay_service_unit`, `RELAY_COMMAND_DROPIN`; `rust/backend/src/topology/relay_units.rs` `apply`, `refresh` |
| reverse-tunnel template `sot-relay-tunnel@.service` | disk | topology | `rust/backend/src/topology/relay_units.rs` `TUNNEL_TEMPLATE`; `rust/protocol/src/topology/relay_units.rs` `tunnel_unit` |
| `<home>/.config/systemd/user/sotd.service` | disk | distribution | `deploy/sotd.service`; renderer `scripts/lib/sot-daemon.sh` `render_sotd_unit` |
| `<home>/.local/bin/sot-launch` wrapper | disk | distribution | `scripts/lib/sot-daemon.sh` `render_sot_launch` |
| `$PREFIX/{bin/*(.prev), repo/{base,versions/<tag>,current}, julia/current}` | disk | distribution | `scripts/install.sh`; `scripts/sot-apply.sh` |
| `$PREFIX/install.json` | disk | distribution | `scripts/install.sh`; `scripts/install-manifest.ps1`; `rust/updater/src/manifest.rs` `InstallManifest` |
| `$PREFIX/updates/` (stages, `pending-<t>.json`, `last-good-<t>.json`, `just-applied-<t>`, `bad-<tag>-<t>`) | disk | distribution | `rust/updater/src/lib.rs` `stage`; `rust/updater/src/pending.rs`; `scripts/sot-apply.sh`; `scripts/sot-apply.ps1` |
| `updates/.lock` (mkdir mutex) | lock | distribution | `rust/updater/src/lock.rs`; `scripts/sot-apply.sh` |
| `$PREFIX/logs/`; Windows `logs\supervisor.log`, `launcher.pid`, `launch-status.txt` | disk, lock | distribution | `scripts/lib/sot-daemon.sh` `sot_prune_logs`; `scripts/launch-sot.ps1` |
| `<home>/.sot-comm/bin/`, `.sot-comm-installed`, `VERSION` | disk | distribution | `src/comm_bin.jl` `_publish_comm_bin!`; `src/install.jl` `install_comm` |
| skills, launchers and Codex plugin installed into `<home>/.claude`, `<home>/.local/bin`, `<home>/.agents/plugins`, `$CODEX_HOME/AGENTS.md` | disk | distribution | `src/skills.jl`; `src/launchers.jl`; `src/codex.jl`; `src/adapters.jl` `_install_adapter` |
| hook entries merged into every Claude `settings.json` | disk | distribution | `src/claude_hooks.jl` |
| the comm folder rule `<home>/.sot-comm`, `SOT_COMM_HOME` | disk rule | messaging | `comm/lib/comm-lib-base.sh`; `rust/backend/src/comm/mod.rs` `sot_comm_home` |
| the comm folder's modes (0700 folders, 0600 files; `bin/`, `VERSION` excepted) | disk rule | messaging | `comm/lib/comm-lib-base.sh` (umask); `comm/lib/comm-lib-registry.sh` `ensure_home` (tightening); twins `rust/backend/src/main.rs` `apply_umask`, `src/install.jl` `install_comm`, the top-of-script `umask 077` of `comm/work_state/hooks/comm-status-heartbeat.sh`, `comm/work_state/hooks/comm-status-idle.sh` and `comm/work_state/comm-turn-auditor.sh`, and the subshell of `agents/sot-gh-auth.sh`; exception `agents/worktree/comm-worktree-new.sh` (the caller's mask) |
| `inbox/<h>.jsonl` | disk | messaging | `comm/lib/comm-lib-inbox.sh` `sot_inbox_append`; `rust/backend/src/comm/mail/inbox.rs` `append_line` |
| `inbox/<h>.lock` | lock | messaging | `comm/lib/comm-lib-inbox.sh` `sot_inbox_lock_identity`; `rust/backend/src/comm/mail/inbox.rs` `take_lock`, `INBOX_LOCK_WAIT_ENV` |
| `inbox-lock-manager` | disk | messaging | `rust/backend/src/comm/mail/inbox.rs` `record_at_start`; written at start `rust/backend/src/comm/mail/mod.rs` `record_at_boot` |
| `read/<h>.cursor` | disk | messaging | `comm/mail/comm-poll.sh`; `comm/lib/comm-lib-inbox.sh` `sot_cursor_write`; Rust reader `rust/backend/src/comm/wake/unread.rs` `cursor_offset` |
| `registry.json` | disk | messaging | `comm/lib/comm-lib-registry.sh` `registry_replace`; `rust/backend/src/comm/registry/registry.rs` `replace_registry`, `write_synced`, `stamp_last_seen` |
| `.registry.lock` and reclaim markers | lock | messaging | `comm/lib/comm-lib-registry-lock.sh` `with_lock`; `rust/backend/src/comm/registry/lock.rs` `acquire` |
| `self/<host>__<ws>.txt` | disk | messaging | `comm/lib/comm-lib-identity.sh` `sot_write_self_file`; `comm/registry/comm-context.sh`; `comm/registry/comm-join.sh` |
| derived handle `<repo>-<host>` (`sot_derive_handle`: pieces through `sot_sanitize_component`, the host clamped with a digest, tiers on collision; host text from raw `hostname -s`, case kept) | rule | messaging | `comm/lib/comm-lib-identity.sh` `sot_derive_handle`, `sot_sanitize_component`, `sot_raw_host`; the Codex launcher builds its own default in `agents/codex/bin/ccx` (see two owners) |
| `state/*.tick`, `stop-feedback-*`, `askq-*` | disk | messaging | `comm/work_state/hooks/comm-status-heartbeat.sh`; `comm/work_state/hooks/comm-status-idle.sh`; `comm/work_state/hooks/comm-status-blocked.sh` |
| `<root>/.concept/` | disk | files | `rust/backend/src/files/concept.rs` `ConceptStore`, `target_to_path` |
| annotation header grammar (the `---` fences, `synced_against`) | rule | files | `rust/backend/src/files/concept.rs` `read_synced_against`; second parser `rust/frontend/src/ui/preview/concept.rs` `split_frontmatter` (see two owners) |
| `<root>/.sot-trash/` | disk | files | `rust/backend/src/files/io.rs` `trash_file`, `trash_file_fallback` |
| `<image>.scale.json` sidecars | disk | files | `rust/backend/src/files/preview/scale.rs` `merge_scale_sidecar` |
| physical-scale schema and validity (`{axes:[{name, nm_per_px > 0}], unit}`) | rule | files | `rust/backend/src/files/preview/scale.rs`; `rust/backend/src/files/preview/mod.rs`; second check `rust/frontend/src/ui/preview/image/mod.rs` (see two owners) |
| `<ws>/.sot/captures/` | disk | files | `rust/backend/src/files/preview/crop.rs` `handle_image_crop` |
| `<ws>/.sot/runs/<run_id>/` | disk | sidecars | `rust/backend/src/sidecars/repl/execute.rs` |
| `<home>/.claude-auth/<name>/` and its allowlisted links | disk | agents | `rust/backend/src/agents/accounts.rs` `ensure_account_links`, `SHARED_ENTRIES`, `CLAUDE_ACCOUNTS_DIR` |
| `.claude.json` folder-trust entries | disk | agents | `rust/backend/src/agents/folder_trust.rs` `ensure_folder_trusted`, `publish_trust_file` |
| folder-trust entries `[projects."<root>"]` in `$CODEX_HOME/config.toml` | disk | agents | `agents/codex/bin/ccx` |
| an agent's default home: Codex `$CODEX_HOME`, else `<home>/.codex`; Claude `<home>/.claude` | rule | agents | `rust/backend/src/agents/accounts.rs` `account_home`, `claude_config_dir`; twins `agents/codex/bin/ccx`, `src/homes.jl` `codex_home` (see two owners) |
| `.sot/worktree.toml` | disk, setting | agents | `agents/worktree/comm-worktree-new.sh`; `agents/worktree/comm-worktree-clean.sh` |
| `<home>/.sot-comm/gh-device-auth.json` (a pending GitHub device code: `request` writes it 0600, `poll` removes it on every exit) | disk | agents | `agents/sot-gh-auth.sh` |
| `.sot/hosts.toml.example` | disk | topology | `.sot/hosts.toml.example` |
| `.sot/{settings,keybindings}.toml.example` | disk | fe-ui | `.sot/settings.toml.example`; `.sot/keybindings.toml.example` |
| process `sotd` | process | server | `rust/backend/src/main.rs` `main` |
| process `sot` (the window) | process | fe-ui | `rust/frontend/src/main.rs` `main` |
| starting this computer's daemon (`sot_daemon_ensure`, `sot-local-daemon.ps1`) | process | distribution | `scripts/lib/sot-daemon.sh` `sot_daemon_ensure`; `scripts/sot-local-daemon.ps1` |
| window supervisor (respawn on 75/76, crash-loop rollback) | process | distribution | `scripts/launch-sot.ps1`; `scripts/lib/sot-daemon.sh` `render_sot_launch` |
| process `sot-capsule supervise` (spawned by rows) | process | capsule | `rust/log/src/bin/sot-capsule.rs`; `rust/log/src/supervisor/mod.rs` `supervise`; spawn `rust/backend/src/rows/spawn/detach.rs` `spawn_detached_supervisor` |
| process `sot-capsule run` (leg) | process | capsule | `rust/log/src/supervisor/leg.rs` `build_run_command`; `rust/log/src/capsule/writer_loop/mod.rs` `run` |
| the agent program (claude, codex): the launch recipe (the leg runs it) | process | agents | `rust/backend/src/agents/argv.rs` `agent_argv`; `rust/log/src/capsule/producer/pty/mod.rs`; `rust/log/src/capsule/producer/conpty/producer.rs` |
| Julia kernel per workspace | process | sidecars | `rust/backend/src/sidecars/kernel.rs` `Kernel`, `run_one_generation` |
| Julia REPL per workspace | process | sidecars | `rust/backend/src/sidecars/repl/supervisor.rs` `spawn_supervisor`, `supervisor_task` |
| Pluto per daemon | process | sidecars | `rust/backend/src/sidecars/pluto.rs` `Pluto`, `spawn_supervisor` |
| MathJax (node) per daemon | process | sidecars | `rust/backend/src/sidecars/mathjax.rs` `MathJax`, `spawn_supervisor` |
| monitor sampler (`bash -s`, `ssh <alias> bash -s`) | process | sidecars | `rust/backend/src/sidecars/monitor.rs` `spawn_source`, `SAMPLER_SH` |
| quarto render child | process | pages | `rust/backend/src/pages/ops.rs` `run_quarto` |
| `git` child of a site open | process | pages | `rust/backend/src/pages/site/links.rs` `run_git` |
| `gio trash` | process | files | `rust/backend/src/files/io.rs` `trash_file` |
| hub-link ssh | process | messaging | `rust/backend/src/comm/mail/hub_link.rs` `recipe_for`, `link_once` |
| comm forward ssh (guest to hub) | process | messaging | `rust/backend/src/comm/mail/forward.rs` `forward_comm_file` |
| `sotd stdio-bridge` | process | topology | `rust/backend/src/topology/stdio_bridge.rs` `run`, `connect` |
| relay-unit ssh per relayed connection | process | topology | `rust/protocol/src/topology/relay_units.rs` `relay_command_line`; `rust/protocol/src/topology/ssh_bridge.rs` `SshRecipe` |
| window control ssh per host | process | fe-net | `rust/frontend/src/net/transport/mod.rs` `connect_and_run` |
| lane-dial ssh | process | topology | `rust/protocol/src/topology/lane_client.rs` `DaemonLaneEndpoint`, `dial` |
| page-proxy ssh per browser connection | process | pages | `rust/frontend/src/pages.rs` `pipe_one`, `dial` |
| browser opener | process | pages | `rust/frontend/src/pages.rs` `open_url_in_browser`, `open_html_in_browser` |
| drawer shell (portable-pty) | process | fe-ui | `rust/frontend/src/ui/drawer/terminal/pty.rs` `LocalTerminal`, `spawn` |
| updater children (curl, gh, tar, julia instantiate, npm) | process | distribution | `rust/updater/src/fetch/mod.rs`; `rust/updater/src/fetch/archive.rs`; `rust/updater/src/prepare.rs` |
| `systemctl` calls of the relay refresh | process | topology | `rust/backend/src/topology/relay_units.rs` `run_systemctl` |
| turn auditor (`claude -p`) | process | messaging | `comm/work_state/comm-turn-auditor.sh` |
| sotd tokio runtime and accept loop | thread | server | `rust/backend/src/main.rs` `main`; `rust/backend/src/server/listen.rs` `run_local` |
| per-connection task and writer | thread | server | `rust/backend/src/server/conn.rs` `handle_connection`, `serve_control`; `rust/backend/src/server/reply.rs` `write_frame_within` |
| off-loop job pool (4 per connection) | thread, lock | server | `rust/backend/src/server/reply.rs` `spawn_job`, `OFFLOOP_CONCURRENCY` |
| lease ticker (1 s) | thread | lifecycle | `rust/backend/src/lifecycle/lease.rs` `ticker`, `tick` |
| shutdown backstop thread | thread | lifecycle | `rust/backend/src/lifecycle/shutdown.rs` `run`, `shutdown_bound` |
| hub link task | thread | messaging | `rust/backend/src/comm/mail/hub_link.rs` `spawn_link` |
| registry poll task (workspace.list refresh) | thread | messaging | `rust/backend/src/comm/registry/poll.rs` `spawn_registry_poll` |
| row liveness stamp task (each running row's `last_seen`, every minute) | thread | messaging | `rust/backend/src/comm/registry/liveness.rs` `run` |
| comm wake tick (2 s) | thread | messaging | `rust/backend/src/comm/wake/mod.rs` `run`, `TICK` |
| comm filing blocking task, flock wait threads | thread | messaging | `rust/backend/src/comm/mail/filer.rs` `handle_comm_file`; `rust/backend/src/comm/mail/inbox.rs` `take_lock` |
| update check task (2 min, then daily) | thread | distribution | `rust/backend/src/update.rs` `spawn_periodic`, `FIRST_CHECK_DELAY`, `CHECK_INTERVAL` |
| monitor hub | thread | sidecars | `rust/backend/src/sidecars/monitor.rs` `MonitorHub`, `start` |
| sidecar supervisor tasks | thread | sidecars | `rust/backend/src/sidecars/kernel.rs` `supervisor_loop`; `rust/backend/src/sidecars/repl/supervisor.rs` `supervisor_task`; `rust/backend/src/sidecars/pluto.rs` `supervisor_task`; `rust/backend/src/sidecars/mathjax.rs` `supervisor_task` |
| `relay-refresh` thread | thread | topology | `rust/backend/src/topology/relay_units.rs` `spawn_refresh_at_start` |
| `watch-manage` threads | thread | files | `rust/backend/src/files/watcher.rs` `run_watch_manager` |
| per-row watchdog and observer | thread | rows | `rust/backend/src/rows/run/watchdog.rs` `install_watchdog`; `rust/backend/src/rows/run/observer.rs` `observe` |
| window UI thread | thread | fe-ui | `rust/frontend/src/main.rs` `main`; `rust/frontend/src/ui/app/mod.rs` |
| `sot-transport` runtime (one worker) | thread | fe-net | `rust/frontend/src/main.rs` `main`; `rust/frontend/src/net/hosts.rs` `spawn_transports` |
| `sot-relaunch-watch` | thread | distribution | `rust/frontend/src/relaunch.rs` `spawn_watcher` |
| `sot-fe-command-watch` | thread | fe-ui | `rust/frontend/src/ui/control/file_channel.rs` |
| `sot-selfupdate` | thread | distribution | `rust/frontend/src/selfupdate.rs` `spawn_startup_selfcheck` |
| `sot-term-reader` | thread | fe-ui | `rust/frontend/src/ui/drawer/terminal/pty.rs` `run_reader` |
| proxy manager task | thread | pages | `rust/frontend/src/pages.rs` `spawn_proxy_manager`, `serve_browser` |
| lease holder and reader tasks | thread | lifecycle | `rust/frontend/src/lease.rs` `spawn_holder`, `Leases` |
| attach worker, reader, supervisor probe threads | thread | capsule | `rust/log/src/attach_client/worker/mod.rs`; `rust/log/src/attach_client/worker/steady.rs`; `rust/log/src/supervisor/probe/mod.rs` |
| supervisor worker per operation | thread | capsule | `rust/log/src/supervisor/lifecycle.rs` `Lifecycle`; `rust/log/src/supervisor/oneshot.rs` `endrun_inner`, `reset_inner` |
| lane accept and reaper threads | thread | capsule | `rust/log/src/lane/socket_unix/accept.rs`; `rust/log/src/lane/pipe_win/accept.rs` |
| frame format, codec, 1 MiB cap | wire | wire | `rust/protocol/src/lib.rs` `Frame`; `rust/protocol/src/codec.rs` `read_frame`, `write_frame`, `MAX_ENVELOPE_BYTES` |
| `PROTOCOL_VERSION` | wire | wire | `rust/protocol/src/lib.rs` `PROTOCOL_VERSION` (shell literal `comm/lib/comm-lib-client.sh` `sot_hello_frame`) |
| product version, `is_release_build` | wire | wire | `rust/protocol/src/version.rs` `app_version`, `is_release_build`; `rust/protocol/build.rs` |
| IR `TreeNode`, `PreviewPayload`, `BlobDescriptor` | wire | wire | `rust/protocol/src/ir.rs` |
| `hello` | op | server | `rust/backend/src/server/hello.rs` `handle_hello`, `protocol_gate`, `admit_hello` |
| `ping` | op | server | `rust/backend/src/server/conn.rs` `handle_ping` |
| `version.query` | op | server | `rust/backend/src/clients.rs` `handle_version_query` |
| `fe.presence`; the active frontend | op, state | server | `rust/backend/src/clients.rs` `handle_fe_presence`, `ActiveFrontend`, `resolve_active` |
| `fe.command.send` and the `fe.command` bus | op, bus | server | `rust/backend/src/clients.rs` `handle_fe_command_send`; `rust/backend/src/server/events.rs` `write_fe_command` |
| `FeCommand` meaning and its handling in the window | wire payload | fe-ui | `rust/frontend/src/ui/control/command.rs`; `rust/frontend/src/ui/control/dispatch.rs` |
| `fe.sessions` | op | server | `rust/backend/src/clients.rs` `handle_fe_sessions`, `declare_sessions` (see two owners) |
| client roster `by_conn`, `disconnected` | state | server | `rust/backend/src/clients.rs` `Clients`, `ClientGuard`, `disconnected_since` |
| revision ring, `session_id`, replay at hello | state | server | `rust/backend/src/session.rs` `Session`, `bump`, `replay_after`; `rust/backend/src/server/hello.rs` `admit_hello` |
| revision-ring event names `tree.invalidate`, `preview.served`, `preview.scale_set`, `image.cropped`, `concept.written`, `file.written`, `file.deleted`, `dir.created`, `workspace.created`, `workspace.destroyed`, and their `Session::bump` calls | wire event (replay only) | server | `rust/backend/src/session.rs` `bump`; bumps `rust/backend/src/files/tree_ops.rs`, `rust/backend/src/files/io_ops.rs`, `rust/backend/src/files/concept_ops.rs`, `rust/backend/src/files/preview/mod.rs`, `rust/backend/src/files/preview/scale.rs`, `rust/backend/src/files/preview/crop.rs`, `rust/backend/src/rows/ops/create.rs`, `rust/backend/src/rows/ops/destroy.rs` |
| `fe.lease`, `fe.leaving`, `fe.notice_seen` | op | lifecycle | `rust/backend/src/lifecycle/lease.rs` `Leases`, `hold`, `depart`, `notice_seen`; `rust/frontend/src/lease.rs` `Leases`, `Leaving` |
| lease timings `SHUTDOWN_BOUND`, `LAUNCH_WAIT`, `DAEMON_LOCK_WAIT`, ack waits | setting | lifecycle | `rust/protocol/src/ops/lease.rs` `SHUTDOWN_BOUND`, `LAUNCH_WAIT`, `DAEMON_LOCK_WAIT`, `CLOSE_ACK_WAIT` |
| `tree.root`, `tree.children`, `nav.toggle_hidden` | op | files | `rust/backend/src/files/tree_ops.rs` `handle_tree_root`, `handle_tree_children`, `handle_nav_toggle_hidden` |
| `preview.get`, `preview.set_scale`, `image.crop` | op | files | `rust/backend/src/files/preview/mod.rs` `handle_preview_get`; `rust/backend/src/files/preview/scale.rs` `handle_preview_set_scale`; `rust/backend/src/files/preview/crop.rs` `handle_image_crop` |
| `preview.changed` | event, bus | files | `rust/backend/src/files/watcher.rs` `PreviewChanged`; `rust/backend/src/server/events.rs` `write_preview_changed` |
| watch bus on `Workspaces` | bus | files | `rust/backend/src/rows/registry.rs` `set_watch_bus`, `spawn_workspace_watcher` |
| `concept.read`, `concept.write` | op | files | `rust/backend/src/files/concept_ops.rs` `handle_concept_read`, `handle_concept_write` |
| `concept.list` | op | files | `rust/backend/src/files/concept_ops.rs` `handle_concept_list` |
| `file.read`, `file.write`, `file.delete`, `dir.create` | op | files | `rust/backend/src/files/io_ops.rs` `handle_file_read`, `handle_file_write`, `handle_file_delete`, `handle_dir_create` |
| `file.download`, `file.upload` | op | files | `rust/backend/src/files/transfer.rs` `handle_file_upload` |
| downloads written to disk in the window | state | fe-ui | `rust/frontend/src/ui/nav/files/download.rs` `non_clobbering_path`; `rust/frontend/src/ui/nav/files/transfer.rs` |
| selfie files `<dir>/selfie-<YYYYMMDD-HHMMSS>.png` (`<dir>` is `SOT_SELFIE_DIR`, else `$SOT_REPO_DIR/selfies`, else the window's working directory) | disk | fe-ui | `rust/frontend/src/ui/render/capture.rs` |
| `directory.list` | op | files | `rust/backend/src/files/tree_ops.rs` `handle_directory_list` |
| `kernel.request` | op | sidecars | `rust/backend/src/sidecars/ops.rs` `handle_kernel_request` |
| `math.render` | op | sidecars | `rust/backend/src/sidecars/ops.rs` `handle_math_render` |
| `repl.eval`, `repl.run_file`, `repl.interrupt`, `repl.execute` | op | sidecars | `rust/backend/src/sidecars/repl/ops.rs` `handle_repl_eval`, `handle_repl_run_file`, `handle_repl_interrupt`; `rust/backend/src/sidecars/repl/execute.rs` `handle_repl_execute` |
| `repl.frame` | event, bus | sidecars | `rust/backend/src/sidecars/repl/mod.rs` `Repl`; `rust/backend/src/server/events.rs` `write_repl_frame` |
| `pluto.open` | op | sidecars | `rust/backend/src/sidecars/ops.rs` `handle_pluto_open` |
| `monitor.subscribe`, `.unsubscribe`, `.history`, `monitor.tick` | op, event, bus | sidecars | `rust/backend/src/sidecars/ops.rs` `handle_monitor_subscribe`, `handle_monitor_unsubscribe`, `handle_monitor_history`; `rust/backend/src/sidecars/monitor.rs` `MonitorHub`; `rust/backend/src/server/events.rs` `write_monitor_tick` |
| kernel stdio ops (`kernel.hello`, `file.preview`, `file.parse`, `project.scan`, `markdown.tokenize`) | wire (stdio) | sidecars | `julia/kernel/src/ShipToolsKernel.jl`; `julia/kernel/src/preview.jl`; `julia/kernel/src/project_scan.jl`; `julia/kernel/src/tokenize.jl` |
| REPL stdio frames, `repl.ready` | wire (stdio) | sidecars | `julia/repl/src/ShipToolsRepl.jl`; `julia/repl/src/frames.jl` |
| Pluto `READY`/`OPEN`/`URL`/`ERR` lines | wire (stdio) | sidecars | `julia/pluto/start.jl` |
| MathJax `{id, tex, display}` | wire (stdio) | sidecars | `rust/backend/sidecars/mathjax/render.mjs` |
| plugin ABI (`FileType`, `matches`, `preview`, `file_type_for`) | ABI | sidecars | `core/src/ConceptExplorerCore.jl` `FileType`, `file_type_for` |
| core's Julia `TreeNode` (never constructed outside core's tests; not the wire's `TreeNode`) | ABI | sidecars | `core/src/ConceptExplorerCore.jl` `TreeNode` |
| core's Julia `PreviewPayload` (`mime`, `data`, `extras`: what a plugin's `preview` returns; the kernel sends it as `blob_base64`) | ABI | sidecars | `core/src/ConceptExplorerCore.jl` `PreviewPayload`; `julia/kernel/src/preview.jl` |
| `video.open`, `docs.open`, `quarto.open` | op | pages | `rust/backend/src/pages/ops.rs` `handle_video_open`, `handle_docs_open`, `handle_quarto_open` |
| `proxy.connect` and the loopback allowlist | op, state | pages | `rust/backend/src/pages/proxy.rs` `handle_proxy_connect`, `allowed_proxy_ports` |
| loopback page-URL grammar (`http` or `https`, host `127.0.0.1` or `localhost`, an explicit port) | rule | pages | `rust/protocol/src/page_url.rs` `loopback_port_from_url` |
| video, site-prefix, site-pool listeners and grant tables | endpoint, state | pages | `rust/backend/src/pages/video.rs` `Grants`, `register_video`; `rust/backend/src/pages/site/mod.rs` `spawn`, `spawn_pool`, `set_root` |
| window page-proxy listeners and arming | endpoint | pages | `rust/frontend/src/pages.rs` `serve_browser`, `Arm`; `rust/frontend/src/ui/page_proxy.rs` `ensure_proxy_for_url` |
| Pluto's page server and notebook workers | endpoint | sidecars | `julia/pluto/start.jl`; `julia/pluto/session_options.jl` `configure_session!` |
| `wglshow`'s page server, one per REPL child | endpoint | sidecars | `julia/repl/src/wgl.jl` `page_server`, `no_referrer_page`, `WGL_SERVER` |
| `lane.connect` | op | rows | `rust/backend/src/rows/ops/lane_bridge.rs` `handle_lane_connect` |
| `pty.open` (start a row, answer `attach_direct`) | op | rows | `rust/backend/src/rows/ops/pty.rs` `handle_pty_open` |
| `pty.write` | op | rows | `rust/protocol/src/ops/mod.rs` `PTY_WRITE` (no dispatch arm) |
| `pty.input`, `pty.screen` | op | rows | `rust/backend/src/rows/ops/pty.rs` `handle_pty_input`, `handle_pty_screen` |
| `workspace.create`, `.list`, `.activate`, `.destroy` | op | rows | `rust/backend/src/rows/ops/create.rs` `handle_workspace_create`; `rust/backend/src/rows/ops/list.rs` `handle_workspace_list`, `handle_workspace_activate`; `rust/backend/src/rows/ops/destroy.rs` `handle_workspace_destroy` |
| `workspace.reauth` | op | rows | `rust/backend/src/rows/reauth/mod.rs` `handle_workspace_reauth`, `answer_workspace_reauth` |
| `workspace.changed` | event, bus | rows | `rust/backend/src/rows/mod.rs` `WorkspaceChanged`; `rust/backend/src/server/events.rs` `write_workspace_changed` |
| `Workspace::phase` (the row's observer is its only writer) | state | rows | `rust/backend/src/rows/workspace.rs` `Phase`, `PhaseCell`, `apply_phase_observation`; `rust/backend/src/rows/run/probe.rs` `phase_str` |
| row guard, run gate | lock | rows | `rust/backend/src/rows/registry.rs` `capsule_guard`; `rust/backend/src/rows/gate.rs` `RunGate`, `StartPermit` |
| session name `sot-be-<slug>` | rule | rows | `rust/backend/src/rows/mod.rs` `session_name` |
| label-to-slug rule `slug()` (row slug, session name, socket and pipe names) | rule | rows | `rust/protocol/src/topology/endpoint.rs` `slug`; shell twin `comm/lib/comm-lib-identity.sh` `sot_slug`; window copy `rust/frontend/src/ui/session/switch.rs` `session_name_of` (see two owners) |
| default anchor row | state | rows | `rust/backend/src/rows/anchor.rs` `seed_default_row`; `rust/backend/src/rows/registry.rs` `default_id`, `set_default` |
| `accounts.list` | op | agents | `rust/backend/src/agents/ops.rs` `handle_accounts_list` |
| `agent.send`, `agent.filed` | op | messaging | `rust/backend/src/comm/mail/relay.rs` `handle_agent_send`, `handle_agent_filed` |
| `agent.message`, `agent.receipt` | event, bus | messaging | `rust/backend/src/comm/mail/bus.rs`; `rust/backend/src/server/events.rs` `write_agent_message`, `write_agent_receipt` |
| `comm.file` | op | messaging | `rust/backend/src/comm/mail/filer.rs` `handle_comm_file`, `comm_file_verdict` |
| `agent.join`; a row's `agent_handle` (stored in the row toml through the rows' API) | op, state | messaging | `rust/backend/src/comm/registry/join.rs` `handle_agent_join`; `rust/backend/src/rows/workspace.rs` `agent_handle` |
| work-state fields (`floor, question, waiting, done, note, state, summary, status_at, stop_at`) | state | messaging | `comm/work_state/comm-status.sh`; `comm/work_state/hooks/comm-status-idle.sh` |
| `clear_comm_unread` (clears `done` on view) | state | messaging | `rust/backend/src/comm/registry/registry.rs` `clear_comm_unread` |
| `update.check`, `update.apply` | op | distribution | `rust/backend/src/update.rs` `handle_update_check`, `handle_update_apply` |
| `topology.set` | op | topology | `rust/backend/src/topology/set.rs` `handle_topology_set` |
| `topology.changed` | event, bus | topology | `rust/backend/src/topology/store.rs` `TopologyChanged`; `rust/backend/src/server/events.rs` `write_topology_changed` |
| capsule lanes SOM0, SOA0 (v1-v3), SOSV | wire | capsule | `rust/log/src/lane/wire/mgmt.rs`; `rust/log/src/lane/wire/attach.rs`; `rust/log/src/lane/wire/supervisor.rs` |
| supervisor-lane build id `SUPERVISOR_LANE_BUILD_ID` (stamp `SOT_LOG_BUILD_SHA`: full sha, `-dirty`; carried in the lane hello and `version.query`, never compared) | wire (lane) | capsule | `rust/log/build.rs`; `rust/log/src/identity/exchange.rs`; `rust/log/src/attach_client/supervisor_client.rs` `SUPERVISOR_LANE_BUILD_ID` |
| rollout gate (`gate`, `RolloutEvidence`; `--assume-no-rollback-target`, which every daemon spawn passes) | rule | capsule | `rust/log/src/store/rollout.rs` `gate`; `rust/log/src/capsule/mod.rs` `RolloutEvidence`; `rust/log/src/bin/sot-capsule.rs`; `rust/backend/src/rows/spawn/detach.rs` `mode_flag` |
| `<sd>/rollout-evidence.json` and its reader `read_rollout_evidence` (provisional; no writer, no caller) | disk | capsule | `rust/log/src/store/rollout.rs` `read_rollout_evidence` |
| SDK helper-protocol 1 | wire | capsule | `rust/log/src/claude.rs`; `rust/log/claude-sdk-helper/src/protocol.ts` |
| sotd subcommand `session-socket-path` | CLI | topology | `rust/backend/src/main.rs` `parse_args`; `rust/protocol/src/topology/endpoint.rs` `session_socket_path` |
| sotd subcommands `topology ...`, `status`, `stdio-bridge` | CLI | topology | `rust/backend/src/main.rs` `parse_args`; `rust/backend/src/topology/cli.rs` `run`; `rust/backend/src/topology/status.rs` `run`; `rust/backend/src/topology/stdio_bridge.rs` `run` |
| sotd subcommand `ancestors` | CLI | messaging | `rust/backend/src/comm/registry/ancestors.rs` `run` |
| sotd subcommand `agent-exec` | CLI | agents | `rust/backend/src/agents/ops.rs` `agent_exec` |
| sot flags `--dial`, `--socket` | CLI | fe-net | `rust/frontend/src/cli.rs` `Cli`; `rust/frontend/src/net/dial.rs` `parse_dial_arg`, `resolve_connections` |
| sot flags `--capture`, `--ephemeral` | CLI | fe-ui | `rust/frontend/src/cli.rs` `Cli`; `rust/frontend/src/ui/render/capture.rs` |
| sot flag `--no-lease` | CLI | lifecycle | `rust/frontend/src/lease.rs` `lease_exempt` |
| sot flags `--update-status`, `--relaunched` | CLI | distribution | `rust/frontend/src/selfupdate.rs` `print_status`; `rust/frontend/src/cli.rs` `Cli` |
| sot flag `--token`, `SOT_TOKEN` | CLI | fe-net | `rust/frontend/src/cli.rs` `Cli` |
| sotd exit 0 (requested shutdown) | exit code | lifecycle | `rust/backend/src/lifecycle/shutdown.rs` `REASON`; `rust/protocol/src/ops/lease.rs` `EXIT_REQUESTED_SHUTDOWN` |
| sotd exit 75 (update restart) | exit code | distribution | `rust/backend/src/update.rs` `exit_for_update`; `rust/protocol/src/ops/lease.rs` `EXIT_UPDATE_RESTART` |
| sot exit 75/76 (relaunch, converge) | exit code | distribution | `rust/frontend/src/lease.rs` `exit_intent`, `close_now`; `rust/frontend/src/relaunch.rs` |
| sot-capsule exit 0/69/70 | exit code | capsule | `rust/log/src/supervisor/mod.rs`; `rust/log/src/bin/sot-capsule.rs` |
| `SOT_SOCKET`, `SOT_SESSION`, `SOT_WORKSPACE`, `SOT_WORKSPACE_ID`, `SOT_WORKSPACE_ROOT`, `SOT_MANUAL` | env | agents | `rust/backend/src/agents/awareness.rs` `awareness_env` |
| `SOT_COMM_NAME`, `SOT_COMM_SELF_FILE` (issued at spawn) | env | messaging | `rust/backend/src/agents/env.rs` `agent_env`; read `comm/registry/comm-context.sh`; `comm/registry/comm-join.sh` |
| `SOT_COMM_HOOKS`, `SOT_LOCK_WAIT_SECS`, `SOT_INBOX_LOCK_WAIT_SECS`, `SOT_INBOX_READ_WAIT_SECS`, `SOT_INBOX_READ_WARNING`, `SOT_SEND_TIMEOUT`, `SOT_COMM_ASKQ_ID`, `SOT_COMM_EXPERTISE`, `SOT_HB_CTX_TIMEOUT_TICKS`, `SOT_TURN_AUDITOR`, `SOT_AUDITOR_*` | env | messaging | `comm/lib/comm-lib-base.sh`; `comm/lib/comm-lib-inbox.sh`; `comm/work_state/hooks/comm-status-heartbeat.sh`; `comm/work_state/comm-turn-auditor.sh` |
| `SOT_COMM_SPAWN_WAIT`, `SOT_COMM_SPAWN_CAPSULE_WAIT`, `SOT_FE_ENDPOINT`, `SOT_SPAWN_ENDPOINT`, `SOT_NAV_DRY_RUN`, `SOT_ACCOUNT` | env | agents | `agents/spawn/comm-spawn.sh`; `agents/sot-fe/sot-fe`; `agents/spawn/comm-bootstrap.sh`; `rust/backend/src/agents/accounts.rs` `account_env` |
| `SOT_PROBE_READ_TIMEOUT`, `SOT_PROBE_READY_WAIT` | env | agents | `agents/spawn/comm-probe.sh` |
| `GH_OAUTH_CLIENT_ID`, `SOT_GH_SCOPES` (sot-gh-auth also honours gh's own `GH_HOST`, `GH_CONFIG_DIR`) | env | agents | `agents/sot-gh-auth.sh` |
| `SOT_BACKEND_LABEL`, `SOT_RELAY_ENDPOINT`, `SOT_RELAY_LABEL`, `SOT_RELAY_SOTD`, `SOT_RELAY_TARGET` | env | topology | `rust/protocol/src/topology/relay_units.rs` `relay_command_line`; `comm/lib/comm-lib-client.sh` `sot_relay_endpoint` |
| `SOT_JULIA_BIN`, `SOT_NODE_BIN`, `QUARTO_JULIA` | env | sidecars | `rust/backend/src/sidecars/julia.rs` `resolve_bin`; `rust/backend/src/sidecars/mathjax.rs` `default_script_path`; `QUARTO_JULIA` is set for quarto by `rust/backend/src/pages/ops.rs` `run_quarto` |
| which Julia binary the daemon runs (`julia::resolve_bin`: an absolute `SOT_JULIA_BIN`, juliaup's default channel, a verified PATH candidate) | rule | sidecars | `rust/backend/src/sidecars/julia.rs` `resolve_bin`; second choice `rust/backend/src/update.rs` `prepare_spec` (see two owners) |
| `SOT_WATCH_BUDGET` | env | files | `rust/backend/src/files/watcher.rs` `watch_budget` |
| `SOT_VIDEO_PORT`, `SOT_DOCS_PORT`, `SOT_PROXY_EXTRA_PORTS` | env | pages | `rust/backend/src/pages/video.rs` `video_port`; `rust/backend/src/pages/site/mod.rs` `site_port`; `rust/backend/src/pages/proxy.rs` `allowed_proxy_ports` |
| `SOT_SETTINGS`, `SOT_KEYBINDINGS`, `SOT_PROJECTS_ROOT`, `SOT_REMOTE_HOME` | env | fe-ui | `rust/frontend/src/ui/persist/discover.rs` `find_config_file`; `rust/frontend/src/ui/persist/settings.rs`; `rust/frontend/src/ui/input/keybindings.rs`; `rust/frontend/src/ui/session/picker.rs` |
| `SOT_SELFIE_DIR` | env | fe-ui | `rust/frontend/src/ui/render/capture.rs` |
| `SOT_FE_INSTANCE` | env | fe-net | `rust/frontend/src/net/identity.rs` `resolve_fe_instance_component` |
| `SOT_PROJECT_ROOT` | env | server | `rust/backend/src/main.rs` `main` |
| `SOT_UPDATE_MODE`, `SOT_UPDATE_REPO`, `SOT_UPDATE_ROOT`, `SOT_UPDATE_FETCHER`, `SOT_NO_UPDATE`, `SOT_APPLY*`, `SOT_INSTALL_*`, `SOT_PREFIX`, `SOT_BIN`, `SOT_FRONTEND_BIN`, `SOT_LAUNCH_*`, `SOT_LOG_KEEP`, `SOT_LOG_CAP_BYTES`, `SOT_REPO_DIR`, `SOT_HOST_NAME`, `SOT_RESTART_BE`, `SOT_BACKEND_LOG`, `SOT_BUILD_*` | env | distribution | `rust/backend/src/update.rs` `mode_from_env`; `rust/updater/src/fetch/mod.rs`; `rust/updater/src/manifest.rs`; `scripts/install.sh`; `scripts/launch-sot.sh`; `scripts/launch-sot.ps1`; `rust/protocol/build.rs` |
| `SOT_MEDIA_BIN`, `SOT_MEDIA_FONT_SCALE`, `SOT_MEDIA_KEEP`, `SOT_MEDIA_AGENT_TIMEOUT` | env | records | `docs/tools/docs-media.sh` |
| `SOT_TEST_*` seams read by production binaries: the daemon's shutdown and lease bounds | env | lifecycle | `rust/backend/src/lifecycle/shutdown.rs` `OVERRIDE_MS`; `rust/backend/src/lifecycle/lease.rs` `OVERRIDE_MS` |
| `SOT_TEST_*` seams read by production binaries: the connection read deadline | env | server | `rust/backend/src/server/conn.rs` `OVERRIDE_MS` |
| `SOT_TEST_*` seams read by production binaries: the registry lock's timings | env | messaging | `rust/backend/src/comm/registry/lock.rs` |
| `SOT_TCP_PORT`, `SOT_REMOTE_REPO`, `SOT_REMOTE_SOCKET` (retired: only scripts and skills still name them) | env | distribution | `scripts/shutdown-sot.ps1`; `scripts/install.sh`; `agents/claude/sot-setup/SKILL.md` |
| fan-in `(HostKey, IncomingEvt)` and per-host `OutgoingReq` senders | bus | fe-net | `rust/frontend/src/main.rs` `main`; `rust/frontend/src/net/transport/event.rs` `IncomingEvt`; `rust/frontend/src/net/transport/request.rs` `OutgoingReq` |
| `LinkGate` (one per host; the window's transport is its only writer) | state | topology | `rust/protocol/src/topology/ssh_bridge.rs` `LinkGate`; written `rust/frontend/src/net/transport/mod.rs` |
| per-host table (`host_connected`, `host_transports`, `host_resolved_dial`, `link_gates`, `declared_host`, `reconnect_now`) | state | fe-net | `rust/frontend/src/net/hosts.rs` `HostTable`; `rust/frontend/src/ui/connections.rs` |
| `FrontendIdentity` | state | fe-net | `rust/frontend/src/net/identity.rs` `FrontendIdentity`, `frontend_identity` |
| `Signal` and its tree registry, `Signal::spawn`, `Signal::spawn_std`, `Contained`, `Held`, `ChildGuard`, `fire` | state, lock | lifecycle | `rust/backend/src/lifecycle/child_signal.rs` `Signal`, `Signal::spawn`, `Signal::spawn_std`, `Contained`, `Held`, `ChildGuard`, `fire`; `rust/backend/src/lifecycle/contain.rs` `Tree` |
| `Leases`, its mutex and phase | state, lock | lifecycle | `rust/backend/src/lifecycle/lease.rs` `Leases`, `Phase` |
| window exit decision (`ExitReason`, `ExitStep`, `exit_intent`, `close_now`) | state | lifecycle | `rust/frontend/src/lease.rs` `ExitReason`, `ExitStep`, `exit_intent`, `close_now` |
| quit prompt, `request_quit` | UI | fe-ui | `rust/frontend/src/ui/app/exit.rs` `quit_prompt_key`; `rust/frontend/src/ui/app/handler.rs` |
| parent-death lease (fd-3 pipe; Windows `Local\sot-lease-*` mutex) | lock | capsule | `rust/log/src/supervisor/lease_win.rs`; `rust/log/src/supervisor/leg.rs` `SpawnLease`, `LegLease` |
| REPL `OUT_LOCK` | lock | sidecars | `julia/repl/src/ShipToolsRepl.jl` |
| the shell `with_lock` | lock | messaging | `comm/lib/comm-lib-registry-lock.sh` `with_lock` |

## Two owners today

Where two copies of one rule or one file exist, the owner is named and the other copy is listed. Kind: **pure** is a
move or the deletion of an identical copy; **extract** keeps behaviour but is not a move; **behaviour** changes what a
user or another process sees.

| Case | Owner | Copies live in | Every other copy becomes | Kind |
|---|---|---|---|---|
| `registry.json` written from two languages (join, leave, despawn, spawn, status, heartbeat, Stop hook, send/poll/spawn touch, destroy prune, `clear_comm_unread`, the daemon's row liveness stamp) | messaging | shell `comm/lib/comm-lib-registry.sh` `registry_replace` (under `with_lock`); Rust `rust/backend/src/comm/registry/registry.rs` `replace_registry`, `clear_comm_unread`, `stamp_last_seen` | The shell writers all call `registry_replace`; the Rust writers call the one `replace_registry`; each field group (identity, liveness, work state) has one documented owner | pure |
| A session's handle in five places: self-file, registry key, row toml `agent_handle`, hello `name`, capsule env `SOT_COMM_NAME` | messaging | `comm/lib/comm-lib-identity.sh` `sot_write_self_file`; `rust/backend/src/rows/workspace.rs` `agent_handle`; `rust/backend/src/agents/env.rs` `agent_env`; resolver `rust/backend/src/comm/registry/registry.rs` `comm_handle_for_workspace` | The row toml is the daemon's one record of which row holds the handle and `comm_handle_for_workspace` its one resolver; the self-file is the session's cache, the registry key the address book's index, hello `name` a display label, `SOT_COMM_NAME` the issued pin | behaviour |
| settings.toml on Windows is two files: the window reads a clone-local file through `SOT_SETTINGS`, sotd reads `<config>/settings.toml`; platform decides where (`<config>`), agents own `[trust]` | fe-ui | `scripts/launch-sot.ps1`; `rust/frontend/src/ui/persist/discover.rs` `find_config_file`; `rust/backend/src/agents/folder_trust.rs` `declared_root_prefix` | The window reads `<config>/settings.toml` like sotd; the repo-local settings file under `.sot/` (untracked; `.sot/settings.toml.example` shows its shape) stays a dev override | behaviour |
| Hand TOML parsers: hosts.toml, the row store, the settings and resume files, the `[trust]` section, and two shell `[trust]` header guards; each file's owner keeps its schema, the parser is the `toml` crate | platform | `rust/protocol/src/topology/mod.rs` `parse`; `rust/backend/src/rows/store/codec.rs` `parse_kv`, `toml_quote`, `toml_unquote`; `rust/frontend/src/ui/persist/settings.rs` `parse`; `rust/frontend/src/ui/persist/resume.rs` `strip_quotes`, `toml_quote`; `rust/backend/src/agents/folder_trust.rs`; `scripts/install.sh`; `scripts/sot-install-layout.ps1` | Each hand parser becomes `toml::from_str` into a typed struct; the shell guards become one daemon command | behaviour |
| Session name `sot-be-<slug>` re-derived in the window | rows | `rust/backend/src/rows/mod.rs` `session_name`; `rust/frontend/src/ui/session/switch.rs` `session_name_of` | The window reads `session_name` from `workspace.list` | behaviour |
| Workspace-key normalizers in two key spaces | fe-ui | `rust/frontend/src/ui/session/workspace_key.rs` `ws_key_of`, `caption_ws_key`, `reply_ws_key`, `lifecycle_key_of`, `active_repl_starting`, `migrate_default_slug_keys` | One workspace ref, resolved where the wire is read; the six become calls to it | behaviour |
| Host-name rules: `host_name()` lowercases the first label; `sot_host` matches it in shell; a derived handle keeps the raw `hostname -s`; the window uses its own env lookup; `comm_self_host` is a second name for `host_name`; `host_matches` bridges the case gap | platform | `rust/log/src/host/state_dir.rs` `host_name`; `comm/lib/comm-lib-base.sh` `sot_host`; `comm/lib/comm-lib-identity.sh` `sot_raw_host`; `agents/spawn/comm-spawn.sh`; `rust/frontend/src/ui/persist/resume.rs` `state_path`; `rust/backend/src/comm/mail/filer.rs` `comm_self_host`; `rust/backend/src/comm/registry/registry.rs` `host_matches` | `comm_self_host` is deleted (an alias); the shell scripts call `sot_host`; the host part of a derived handle stays raw by the handle rule | pure |
| State-dir rule written twice (Windows `...\sot` against `...\sot\state`; Unix fallback) | platform | `rust/log/src/host/state_dir.rs` `sot_state_dir`; `rust/backend/src/paths.rs` `state_dir` | `paths.rs` `state_dir` is deleted; sotd.log uses `sot_state_dir()` | behaviour |
| Config-dir rule written more than once | platform | `rust/log/src/host/state_dir.rs` `sot_config_dir`; `rust/frontend/src/ui/persist/resume.rs` `config_dir`; `rust/frontend/src/ui/persist/discover.rs` `find_config_file` | Both walks call `sot_config_dir()` | behaviour |
| Leg-gone check written twice (any lock error is ambiguous in one, string-matched contention in the other) | capsule | `rust/log/src/supervisor/journal/end_run.rs`; `rust/backend/src/rows/run/end_run.rs` `is_lock_contention`, `absence_proof` | Both call one function in the store | behaviour |
| Pipe and socket lane servers written as twins | capsule | `rust/log/src/lane/pipe_win/server.rs`; `rust/log/src/lane/socket_unix/server.rs` | One bridge generic over `LaneServer` | extract |
| Endpoint grammar parsed in each client and in shell | topology | `rust/frontend/src/net/dial.rs` `parse_dial_arg`; `rust/backend/src/comm/mail/hub_link.rs` `recipe_for`; `rust/backend/src/topology/dial.rs` `connect`; shell `comm/lib/comm-lib-client.sh` `sot_ssh_bridge` | One `Endpoint` type in the protocol crate; the Rust clients call it; the shell copy stays, pinned by a test | pure (Rust) |
| ssh option sets differ between the protocol recipe, the relay unit, the shell client, the hub-link dial and the monitor sampler | topology | `rust/protocol/src/topology/ssh_bridge.rs` `SSH_OPTS`; `rust/protocol/src/topology/relay_units.rs` `relay_command_line`; `comm/lib/comm-lib-client.sh` `_sot_ssh_control`; `rust/backend/src/topology/dial.rs`; `rust/backend/src/sidecars/monitor.rs` `spawn_source` | One option set and one stderr capture in `SshRecipe` | behaviour |
| Update-ownership rule and repo default twice | distribution | `rust/backend/src/update.rs` `backend_role_wanted`; `rust/frontend/src/selfupdate.rs` `backend_owns_updates`; `rust/updater/src/identity.rs` `DEFAULT_REPO` | One function in the protocol crate, one `from_env` in the updater | pure |
| Durable replace written several ways | platform | `rust/log/src/host/durable.rs` `publish_noreplace`; `rust/backend/src/durable.rs` `replace_file`; `rust/backend/src/files/io.rs` `write_file`; `rust/backend/src/files/concept.rs` `write`; `rust/backend/src/topology/store.rs` `write_atomic`; `rust/backend/src/topology/relay_units.rs` `write_atomic`; `rust/backend/src/agents/folder_trust.rs` `publish_trust_file`; `rust/backend/src/comm/registry/registry.rs` `write_synced` | One durable write | behaviour |
| Workspace confinement more than one way | files | `rust/backend/src/files/confine.rs` `path_within_root`, `canonical_under_root`; `rust/backend/src/files/tree.rs` `FilesMode`; `rust/backend/src/files/concept.rs` `target_to_path` | One `files::confine` | behaviour |
| "Is h listed" asked in shell and in Rust | messaging | `comm/mail/comm-send.sh`; `rust/backend/src/comm/mail/filer.rs` `comm_file_verdict`; `rust/backend/src/comm/mail/hub_link.rs` `registry_handles` | One predicate per language | pure |
| Liveness judged in shell and in Rust on one registry fact (`last_seen`, stamped by the session and by the daemon running its row); a third route with `NOT CONFIRMED` | messaging | `comm/lib/comm-lib-registry.sh` `sot_heartbeat_fresh`; `rust/backend/src/comm/mail/filer.rs` `heartbeat_fresh`; `comm/mail/comm-relay.sh` | One rule on one fact: the shell twin stays, pinned by `heartbeat_agrees_with_the_shell`; the not-mine leg goes with B2 | pure (a test) |
| "Unread" decided twice | messaging | `rust/backend/src/comm/wake/unread.rs` `scan`; `comm/lib/comm-lib-inbox.sh` `sot_unread` | One rule in each language, the hook counts through `sot_unread`; pinned by `unread_agrees_with_the_shell` on every line a writer emits | pure (a test) |
| Inbox lock and append, cursor, line hash, registry read and lock in shell and Rust | messaging | `comm/lib/comm-lib-inbox.sh` `sot_inbox_append`, `sot_cursor_offset`, `sot_line_hash`; `rust/backend/src/comm/mail/inbox.rs` `append_line`; `rust/backend/src/comm/wake/unread.rs` `line_hash`, `cursor_offset`; `comm/lib/comm-lib-registry-lock.sh` `with_lock`; `rust/backend/src/comm/registry/lock.rs` `acquire` | Kept twins while both languages write; one parity test per pair | pure (tests) |
| Hello literal `"protocol"` in the shell client against `PROTOCOL_VERSION` | wire | `comm/lib/comm-lib-client.sh` `sot_hello_frame`; `rust/protocol/src/lib.rs` `PROTOCOL_VERSION` | Pinned by a test | pure (a test) |
| "This computer's daemon" ensured twice, with the labels `sot` and `local` spelled at several sites; topology owns `local_daemon_label` | distribution | `scripts/lib/sot-daemon.sh` `sot_daemon_ensure`; `scripts/sot-local-daemon.ps1`; `scripts/launch-sot.ps1`; `scripts/restart-backend.sh`; `deploy/sotd.service`; `rust/protocol/src/topology/endpoint.rs` `local_daemon_label` | `sotd` defaults to the label; the scripts stop spelling it | behaviour |
| `install.json` written by two writers in two shapes | distribution | `scripts/install.sh`; `scripts/install-manifest.ps1`; `rust/updater/src/manifest.rs` `InstallManifest` | One writer, one schema | behaviour |
| Apply transaction in two languages | distribution | `scripts/sot-apply.sh`; `scripts/sot-apply.ps1` | One apply in Rust, one crash-loop rule | behaviour |
| Julia instantiate done twice | distribution | `scripts/install.sh`; `scripts/launch-sot.ps1`; `rust/updater/src/prepare.rs` | The updater's prepare owns it on every target | behaviour |
| Codex home resolved two ways | agents | `rust/backend/src/agents/accounts.rs` `account_home`; `agents/codex/bin/ccx`; `src/homes.jl` `codex_home` | `accounts.rs` honours `$CODEX_HOME`; the shell and Julia copies stay as twins | behaviour |
| Julia binary chosen twice | sidecars | `rust/backend/src/sidecars/julia.rs` `resolve_bin`; `rust/backend/src/update.rs` `prepare_spec` | `prepare_spec` takes `resolve_bin()`'s result | behaviour |
| Default handle derived twice | messaging | `comm/lib/comm-lib-identity.sh` `sot_derive_handle`; `agents/codex/bin/ccx` | `ccx` builds its default from the same pieces | behaviour |
| Label-to-slug rule written in Rust and in shell | rows | `rust/protocol/src/topology/endpoint.rs` `slug`; `comm/lib/comm-lib-identity.sh` `sot_slug` | The shell twin stays, pinned by one case table | pure (a test) |
| `.sot/worktree.toml` `display_prefix` parsed twice, byte-identical | agents | `agents/worktree/comm-worktree-new.sh`; `agents/worktree/comm-worktree-clean.sh` | One shell function both call | pure |
| Build provenance read from git twice; the product version stamp is wire's, the lane build id capsule's | wire | `rust/protocol/build.rs`; `rust/log/build.rs` | Each concept keeps its own stamp; the shared git reads are one helper | pure |
| Folder trust written two ways | agents | `rust/backend/src/agents/folder_trust.rs` `ensure_folder_trusted`; `agents/codex/bin/ccx` | `ccx`'s append is deleted where the bypass flag answers the prompt | behaviour |
| Row creation and removal announced twice: ring entries and the live bus | rows | `rust/backend/src/rows/ops/create.rs`; `rust/backend/src/rows/ops/destroy.rs`; `rust/backend/src/rows/mod.rs` `WorkspaceChanged` | `workspace.changed` is the one announcement | behaviour |
| Skill copies | agents | `.claude/skills/sot-setup/SKILL.md`; `agents/claude/sot-setup/SKILL.md`; `.claude/skills/sot-statusline-setup/statusline.sh`; `agents/claude/sot-statusline-setup/statusline.sh` | The `.claude` copies are deleted | pure |
| cli-role hello written twice | topology | `rust/backend/src/comm/mail/hub_link.rs` `link_once`; `rust/backend/src/topology/dial.rs` `connect` | One builder in the protocol crate | pure |
| Annotation header parsed twice | files | `rust/backend/src/files/concept.rs` `read_synced_against`; `rust/frontend/src/ui/preview/concept.rs` `split_frontmatter`, `strip_frontmatter` | One parser in the protocol crate | behaviour |
| cgroup kill written twice | rows | `rust/log/src/claude.rs`; `rust/backend/src/rows/spawn/row_scope.rs` `end` | `claude.rs` goes with the SDK producer's fate | behaviour |
| Video extensions written three times | sidecars | `rust/backend/src/pages/video.rs` `VIDEO_EXTS`; `rust/backend/src/files/preview/mod.rs`; `julia/plugins/video-file/src/ShipToolsVideoFile.jl` | One const now; plugin-declared bounds later | pure (Rust const) |
| `fe.sessions`: the window relays its computer's roster to other daemons | server | `rust/backend/src/clients.rs` `handle_fe_sessions`; `rust/frontend/src/net/transport/ops/workspace.rs` | Moves with the ruling on the hub link | behaviour |
