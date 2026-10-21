# Who owns what: one owner per concept

Every concept has exactly one owning subsystem. The subsystems are [wire](../rust/protocol/CLAUDE.md),
[topology](../rust/protocol/src/topology/CLAUDE.md), [server](../rust/backend/src/server/CLAUDE.md),
[lifecycle](../rust/backend/src/lifecycle/CLAUDE.md), [rows](../rust/backend/src/rows/CLAUDE.md),
[agents](../agents/CLAUDE.md), [capsule](../rust/log/CLAUDE.md), [platform](../rust/log/src/host/CLAUDE.md),
[messaging](../rust/backend/src/comm/CLAUDE.md), [files](../rust/backend/src/files/CLAUDE.md),
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
| runtime base `<runtime>` (`runtime_sot_dir`: a private `$XDG_RUNTIME_DIR`, else a private `/run/user/<uid>`, else `/tmp/sot-<uid>`); `SOT_RUNTIME_DIR` (`runtime_dir`: the capsule lane sockets only; no production writer, a test seam) | disk rule | platform | `rust/log/src/host/state_dir.rs` `runtime_sot_dir`, `runtime_dir` |
| host name `host_name()`, `SOT_SELF_HOST` | machine fact | platform | `rust/log/src/host/state_dir.rs` `host_name`, `kernel_host_name` (copies: `rust/backend/src/comm/mail/filer.rs` `comm_self_host`, `rust/frontend/src/ui/persist/resume.rs` `state_path`, `comm/lib/comm-lib-base.sh` `sot_host`, `comm/registry/comm-context.sh`, `agents/spawn/comm-despawn.sh`) |
| `state_dir_hash` (lane socket names) | machine fact | platform | `rust/log/src/host/state_dir.rs` `state_dir_hash` |
| durable write (fsync, no-clobber rename, publish) | primitive | platform | `rust/log/src/host/durable.rs` `publish_noreplace`, `fsync_dir`, `ensure_container`; `rust/backend/src/durable.rs` `write` |
| redial pace of a long-lived connection or child (`Redial`, `STABLE`) | rule | platform | `rust/log/src/host/redial.rs` `Redial`, `STABLE`; callers `rust/frontend/src/net/transport/mod.rs` `spawn`, `redial_for`; `rust/backend/src/comm/mail/hub_link.rs` `hold_link`; `rust/log/src/attach_client/rules/reconnect.rs` `ReconnectState`; `rust/log/src/attach_client/worker/converge.rs` `probe_supervisor_lane`; `rust/backend/src/sidecars/kernel.rs` `supervisor_loop` |
| bounded file locks | primitive | platform | `rust/log/src/host/lock.rs` `lock_writer`, `lock_supervisor`, `try_lock_daemon` |
| a held folder: one its maker holds by a lock (`holder.lock`) for its life, and the removal of one whose maker has ended | primitive | platform | `rust/log/src/host/held_dir.rs` `HeldDir`, `remove_ended`; `rust/log/src/host/lock.rs` `lock_new`, `lock_held` |
| volume preflight | primitive | platform | `rust/log/src/host/volume.rs` `preflight_volume` |
| storage-exhaustion recognition | primitive | platform | `rust/log/src/host/storage.rs` `storage_exhaustion` |
| Windows SDDL/SID helpers | primitive | platform | `rust/log/src/host/winsec.rs` `owner_protected_descriptor`, `sid_string_from_process` |
| peer challenge (OS peer, then wire identity) | primitive | platform | `rust/log/src/identity/challenge.rs` `exchange_identity`; `rust/log/src/identity/exchange.rs`; `rust/log/src/identity/deadline.rs` `run_with_deadline`; per-OS `rust/log/src/identity/challenge_unix.rs`, `rust/log/src/identity/challenge_win.rs`, `rust/log/src/identity/challenge_macos.rs` |
| private-dir and socket-dir checks | primitive | platform | `rust/backend/src/paths.rs` `ensure_private_dir`, `secure_private_dir`, `secure_socket_dir` |
| resource dir, `SOT_RESOURCE_ROOT` | machine fact | platform | `rust/backend/src/paths.rs` `resource_dir` |
| `\\?\` verbatim strip | primitive | platform | `rust/backend/src/paths.rs` `simplify_verbatim` |
| `<state>/daemon.lock` | disk, lock | server | `rust/backend/src/server/listen.rs` `take_daemon_lock`, `lock_daemon`; primitive `rust/log/src/host/lock.rs` `try_lock_daemon`|
| `<state>/sotd.log` (Windows `<state>\state\sotd.log`) | disk | server | `rust/backend/src/daemon_log/mod.rs` `TeeWriter` (stdout gets a copy only when it is not a regular file, `stdout_is_file`; a log over 64 MiB is, after a daemon write, copied (its last 64 MiB) to `sotd.log.1` and truncated in place, `roll_log`); path `rust/backend/src/paths.rs` `state_dir` |
| umask 077, boot refusals | process rule | server | `rust/backend/src/main.rs` `apply_umask`, `parse_args` |
| session socket/pipe path rule `<runtime>/sessions/<label>.sock`, `\\.\pipe\sot-<USER>-<label>` | disk rule | topology | `rust/protocol/src/topology/endpoint.rs` `session_socket_path` |
| local daemon label (`sot`, `local`) | setting | topology | `rust/protocol/src/topology/endpoint.rs` `local_daemon_label`; spelled at several script sites (see two owners) |
| this box's comm relay endpoint (derived when asked; no relay variable overrides it; on the hub it is the daemon's own endpoint, `SOT_SOCKET` first) | rule | topology | `rust/protocol/src/topology/mod.rs` `relay_endpoint`; `rust/backend/src/topology/cli.rs` `relay_endpoint_cmd`; asked, never derived, by `comm/lib/comm-lib-client.sh` `sot_relay_endpoint` |
| generated hub-relay endpoint classification | rule | topology | `rust/protocol/src/topology/mod.rs` `relay_host_for_path`; `rust/frontend/src/net/dial.rs` `parse_dial_arg` consumes it |
| the bound session socket/pipe, its DACL and inbound buffer, live-socket refusal | disk, endpoint | server | `rust/backend/src/server/listen.rs` `run_local`, `bind_session`, `refuse_live_socket`, `session_pipe_security_descriptor` |
| `<state>/held.json` | disk | lifecycle | `rust/backend/src/lifecycle/lease.rs` `HeldRecord`, `persist`; name `rust/protocol/src/ops/lease.rs` `HELD_RECORD_FILE` |
| `<state>/relaunch.request` | disk | distribution | `rust/frontend/src/relaunch.rs` `relaunch_sentinel_path`, `spawn_watcher`; the one writer is `scripts/relaunch-sot.ps1` (the Windows update entry calls its `-Converge` from the installed checkout) |
| `<state>/fe-commands/` | disk | fe-ui | `rust/frontend/src/ui/control/file_channel.rs`; `rust/frontend/src/ui/control/command.rs` |
| `<state>/fe-state.json` | disk | fe-ui | `rust/frontend/src/ui/control/file_channel.rs` `fe_state_path` |
| `<state>/session-<host>.json` | disk | fe-net | `rust/frontend/src/net/state.rs` `state_path`, `SessionMemory` |
| `<config>/state-<host>.toml` (window resume) | disk | fe-ui | `rust/frontend/src/ui/persist/resume.rs` `state_path`, `GlobalState` |
| `<config>/workspaces-<host>/<slug>.toml` | disk | rows | `rust/backend/src/rows/store/mod.rs` `workspaces_dir`, `toml_path_for`, `save` |
| `<config>/sessions-<host>/<label>.toml` (the older row-file shape) | disk | rows | `rust/backend/src/rows/store/mod.rs` `sessions_dir`, `scan_disk` |
| boot migrations of the row store (Windows config dir, unsuffixed fold, sessions read) | disk | rows | `rust/backend/src/rows/store/migrate.rs` `migrate_legacy_state_dirs`; `rust/backend/src/rows/store/mod.rs` `scan_disk` |
| `<config>/settings.toml`: the file and every section but `[trust]` | disk, setting | fe-ui | `rust/frontend/src/ui/persist/settings.rs` `Settings`, `load_layered`, `merge_text`, `strip_comment`; `rust/frontend/src/ui/persist/discover.rs` `find_config_file` |
| `[trust] root_prefix` in settings.toml | setting | agents | `rust/backend/src/agents/trust_declaration.rs` `declare_trust`, `read_trust_declaration` |
| `<config>/keybindings.toml` | disk, setting | fe-ui | `rust/frontend/src/ui/input/keybindings.rs` `KeyBindings`, `load_layered` |
| `<config>/hosts.toml`, `SOT_HOSTS` | disk, setting | topology | `rust/protocol/src/topology/mod.rs` `locate`, `load`, `parse` |
| `[monitor]` roster in hosts.toml (the grammar is topology's; the sidecars read it) | setting | topology | `rust/protocol/src/topology/mod.rs` `monitor_targets`; `rust/backend/src/sidecars/monitor.rs` `load_hosts`, `sampling_roster` |
| the folders a page's tracked link may reach | rule | pages | `rust/backend/src/pages/site/links.rs` `MOUNT_FOLDERS`, `Bounds` |
| a row's capsule state dir `<state>/workspaces/<id>` (name) | disk rule | rows | `rust/backend/src/rows/spawn/state_root.rs` `state_dir_for` |
| removal of a row's state dir (close's tombstone, the start sweep) | disk rule | rows | `rust/backend/src/rows/state_folder.rs` `close_row_folder`, `sweep_state_folders`; the registries it asks `rust/backend/src/rows/store/mod.rs` `named_workspace_ids` |
| `<sd>/supervisor.lock` | disk, lock | capsule | `rust/log/src/supervisor/journal/fence.rs` `lock_supervisor`; `rust/log/src/host/lock.rs` `lock_supervisor` |
| `<sd>/drawer.voyage` (pointer) | disk | capsule | `rust/log/src/supervisor/journal/pointer.rs` |
| `<sd>/supervisor-journal/` | disk | capsule | `rust/log/src/supervisor/journal/mod.rs` `begin`, `finish` |
| `<sd>/voyages/<id>/{writer.lock, seg/, blobs/sha256/}` | disk, lock | capsule | `rust/log/src/store/voyage.rs`; names repeated `rust/backend/src/rows/run/end_run.rs` `absence_proof` |
| `<sd>/row-scopes` and the row's systemd scope `sot-row-<hash>-<uuid>.scope` | disk, process | rows | `rust/backend/src/rows/spawn/row_scope.rs` `capture`, `listed`, `end`, `unit_name`; `rust/backend/src/rows/spawn/row_scope_aim.rs` `aim`, `v2_root` (shell twin of `v2_root`: `scripts/tests/rc-gate.sh` `R`) |
| supervisor lane socket/pipe `supervisor-<hash>` | endpoint | capsule | `rust/log/src/lane/socket_unix/mod.rs`; `rust/log/src/lane/pipe_win/mod.rs` |
| voyage lane socket/pipe `voyage-<id>` | endpoint | capsule | `rust/log/src/lane/socket_unix/mod.rs`; `rust/log/src/lane/pipe_win/mod.rs` |
| hub relay sockets `sot-host-<h>.sock`, relay units and the `zz-sot-relay-command.conf` drop-in | disk, endpoint | topology | `rust/protocol/src/topology/mod.rs` `relay_socket_path`, `runtime_relay_dir`; `rust/protocol/src/topology/relay_units.rs` `relay_service_unit`, `RELAY_COMMAND_DROPIN`; `rust/backend/src/topology/relay_units.rs` `apply`, `refresh` |
| reverse-tunnel template `sot-relay-tunnel@.service` | disk | topology | `rust/backend/src/topology/relay_units.rs` `TUNNEL_TEMPLATE`; `rust/protocol/src/topology/relay_units.rs` `tunnel_unit` |
| `<home>/.config/systemd/user/sotd.service` | disk | distribution | `deploy/sotd.service`; renderer `scripts/lib/sot-daemon.sh` `render_sotd_unit` |
| boot wiring `<home>/.config/systemd/user/default.target.d/sotd.conf` (and an old `default.target.wants/sotd.service` link) | disk | distribution | writer `scripts/lib/sot-daemon.sh` `sot_boot_wants`, `sot_boot_keep`; remover `sot_boot_unwants`, used by the retire in `rust/backend/src/install/unix.rs` (`sotd install`) |
| host pins `topology.conf` in `sotd.service.d/`, `sot-relay-tunnel@.service.d/` and `sot-host-relay-<h>.socket.d/` under `<home>/.config/systemd/user` | disk | topology | text `rust/protocol/src/topology/relay_units.rs` `host_pin`, `daemon_pin`, `install_pin`, `hub_pins`; hosts `rust/protocol/src/topology/mod.rs` `sotd_hosts`; writers `rust/backend/src/topology/relay_units.rs` `pin_daemon`, `converge_hub_pins`; reader `rust/backend/src/install/role.rs` `pin_names_only`, `pin_hosts` |
| `<home>/.local/bin/sot-launch` wrapper | disk | distribution | `scripts/lib/sot-daemon.sh` `render_sot_launch` |
| `$PREFIX/{bin/*(.prev), repo/{base,versions/<tag>,current}, julia/current}` | disk | distribution | `rust/backend/src/install/` (`sotd install`, run by `scripts/install.sh` and `scripts/install.ps1` from a release, or `--archive <folder>` packed by `scripts/pack-build.sh`; on Windows a fresh install, and an update through `sotd update stage` and `scripts/sot-apply.ps1`); `scripts/sot-apply.sh` |
| `$PREFIX/install.json` | disk | distribution | `rust/backend/src/install/record.rs` `text`, `write_whole`, `set_pin` (`sotd install`; rewritten on Unix only as `role::host` and `role::record_gate` allow); `scripts/sot-apply.sh` and `scripts/sot-apply.ps1` (version, tag and commit); `scripts/install-manifest.ps1` (a hand install); `rust/updater/src/manifest.rs` `InstallManifest` |
| `$PREFIX/updates/` (stages, `pending-<t>.json`, `last-good-<t>.json`, `just-applied-<t>`, `bad-<tag>-<t>`) | disk | distribution | `rust/updater/src/lib.rs` `stage`; `rust/updater/src/pending.rs`; `scripts/sot-apply.sh`; `scripts/sot-apply.ps1` |
| `updates/.lock` (mkdir mutex) | lock | distribution | `rust/updater/src/lock.rs`; `scripts/sot-apply.sh` |
| a release check's scratch folder `std::env::temp_dir()`/`sot-updater-*/`, a held folder, and its removal by the check that made it or, for a check whose process ended first, by a later check | disk | distribution | `rust/updater/src/fetch/mod.rs` `Fetcher::latest`, `scratch_dir`, `remove_ended_scratch`; primitive `rust/log/src/host/held_dir.rs` `HeldDir`, `remove_ended` |
| `$PREFIX/logs/`; Windows `logs\supervisor.log`, `launcher.pid`, `launch-status.txt` | disk, lock | distribution | `scripts/lib/sot-daemon.sh` `sot_prune_logs`; `scripts/launch-sot.ps1` |
| `<home>/.sot-comm/bin/`, `.sot-comm-installed`, `VERSION` | disk | distribution | `src/comm_bin.jl` `_publish_comm_bin!`; `src/install.jl` `install_comm` |
| skills, launchers and Codex plugin installed into `<home>/.claude`, `<home>/.local/bin`, `<home>/.agents/plugins`, `$CODEX_HOME/AGENTS.md` | disk | distribution | `src/skills.jl`; `src/launchers.jl`; `src/codex.jl`; `src/adapters.jl` `_install_adapter` |
| hook entries merged into every Claude `settings.json` | disk | distribution | `src/claude_hooks.jl` |
| the comm folder rule `<home>/.sot-comm`, `SOT_COMM_HOME` | disk rule | messaging | `rust/backend/src/comm/mod.rs` `sot_comm_home`, `rust/backend/src/comm/home.rs` `comm_home`; the scripts' path variables `comm/lib/comm-lib-base.sh` |
| the comm folder's modes (0700 folders, 0600 files; `bin/`, `VERSION` excepted) | disk rule | messaging | `rust/backend/src/main.rs` `apply_umask` (every `sotd comm`); `rust/backend/src/comm/home.rs` `ensure`, `tighten`; `comm/lib/comm-lib-base.sh` (umask); twins `src/install.jl` `install_comm`, the top-of-script `umask 077` of `comm/work_state/hooks/comm-status-heartbeat.sh`, `comm/work_state/hooks/comm-status-idle.sh` and `comm/work_state/comm-turn-auditor.sh`, and the subshell of `agents/sot-gh-auth.sh`; exception `agents/worktree/comm-worktree-new.sh` (the caller's mask) |
| `inbox/<h>.jsonl` | disk | messaging | `rust/backend/src/comm/mail/inbox.rs` `file_frame_from`, `append_line`, reached only through `rust/backend/src/comm/mail/filer.rs` `file_here` (and the one-time Windows move in `hub_link.rs`) |
| `inbox/<h>.lock` | lock | messaging | `rust/backend/src/comm/mail/inbox.rs` `take_lock`, `read_lock`, `lock_identity`, `INBOX_LOCK_WAIT_ENV`, `INBOX_READ_WAIT_ENV` |
| `inbox-lock-manager` | disk | messaging | `rust/backend/src/comm/mail/inbox.rs` `record_at_start`; written at start `rust/backend/src/comm/mail/mod.rs` `record_at_boot` |
| `read/<h>.cursor` | disk | messaging | `rust/backend/src/comm/mail/cursor.rs` `offset`, `write` (written only by `rust/backend/src/comm/cli/poll.rs` `poll`), `counts` (the one unread rule) |
| `registry.json` | disk | messaging | `rust/backend/src/comm/registry/registry.rs` `update`, `edit_entry`, `replace_registry`, `write_registry`, `stamp_last_seen`, `read_doc`, `read_registry_fresh`; skeleton `rust/backend/src/comm/home.rs` `ensure` |
| `.registry.lock` and reclaim markers | lock | messaging | `rust/backend/src/comm/registry/lock.rs` `acquire` |
| `self/<host>__<ws>.txt` | disk | messaging | `rust/backend/src/comm/identity/self_file.rs` `slot_leaf`, `name_for`, `conflict`, `write`; pinned at spawn `rust/backend/src/agents/env.rs` `capsule_supervisor_env` |
| derived handle `<repo>-<host>` (safe components and shortened host digest; raw host component retained) | rule | messaging | `rust/backend/src/comm/identity/derive.rs` `derive`, `tier`, `sanitize_component`, `handle_host`; no launcher builds a handle of its own (decision 0031) |
| a process's comm identity and the agent-layer check | rule | messaging | `rust/backend/src/comm/identity/context.rs` `resolve`; `rust/backend/src/comm/identity/layers.rs` `require_agent`, `layers`; the Windows table from `comm/lib/comm-lib-sotd.sh` `_sot_msys_table` |
| `sotd comm` and the wrappers and shell door that run it | CLI | messaging | `rust/backend/src/comm/cli/mod.rs` `run`; `comm/mail/`, `comm/registry/`; `comm/lib/comm-lib-sotd.sh` |
| `state/*.tick`, `stop-feedback-*`, `askq-*`, `dialog-*`, `turn-*` | disk | messaging | `comm/work_state/hooks/comm-status-heartbeat.sh`; `comm/work_state/hooks/comm-status-idle.sh`; `comm/work_state/hooks/comm-status-blocked.sh`; `comm/work_state/comm-status.sh` (`dialog-*`, `turn-*`) |
| `<root>/.concept/` | disk | files | `rust/backend/src/files/concept.rs` `ConceptStore`, `target_to_path` |
| annotation header grammar (fences and synced_against) | rule | wire | `rust/protocol/src/annotation.rs` `split_frontmatter`, `synced_against`; consumers in files and fe-ui |
| `<root>/.sot-trash/` | disk | files | `rust/backend/src/files/io.rs` `trash_file`, `trash_file_fallback` |
| `<image>.scale.json` sidecars | disk | files | `rust/backend/src/files/preview/scale.rs` `merge_scale_sidecar` |
| physical-scale schema and validity | rule | wire | `rust/protocol/src/physical_scale.rs` `PhysicalScale`, `parse_physical_scale`; consumers in files and fe-ui |
| `<ws>/.sot/captures/` | disk | files | `rust/backend/src/files/preview/crop.rs` `handle_image_crop` |
| `<ws>/.sot/runs/<run_id>/` | disk | sidecars | `rust/backend/src/sidecars/repl/execute.rs` |
| a Julia child's or an updater command's temporary folder `std::env::temp_dir()`/`sotd-julia-*/child-*/`, in its process's held folder, and that folder's removal at the daemon's controlled end and, for a process that ended without one, at the next start | disk | sidecars | `rust/backend/src/sidecars/child/tmp.rs` `ChildTmp`, `retire`, `InHeld`, `remove_own`, `sweep_gone`; primitive `rust/log/src/host/held_dir.rs` `HeldDir`, `remove_ended` |
| `<home>/.claude-auth/<name>/` and its allowlisted links | disk | agents | `rust/backend/src/agents/accounts.rs` `ensure_account_links`, `SHARED_ENTRIES`, `CLAUDE_ACCOUNTS_DIR` |
| `.claude.json` folder-trust entries | disk | agents | `rust/backend/src/agents/folder_trust.rs` `ensure_folder_trusted`, `publish_trust_file`; file selection at `rust/backend/src/agents/env.rs` `account_spawn_env` |
| folder-trust entries `[projects."<root>"]` in `$CODEX_HOME/config.toml` | disk | agents | `agents/codex/bin/ccx` |
| an agent's default home: Codex `$CODEX_HOME`, else `<home>/.codex`; Claude `<home>/.claude` | rule | agents | `rust/backend/src/agents/accounts.rs` `account_home`, `claude_config_dir`; twins `agents/codex/bin/ccx`, `src/homes.jl` `codex_home` (see two owners) |
| `.sot/worktree.toml` | disk, setting | agents | `agents/worktree/comm-worktree-new.sh`; `agents/worktree/comm-worktree-clean.sh` |
| `<home>/.sot-comm/gh-device-auth.json` (a pending GitHub device code: `request` writes it 0600, `poll` removes it on every exit) | disk | agents | `agents/sot-gh-auth.sh` |
| `.sot/hosts.toml.example` | disk | topology | `.sot/hosts.toml.example` |
| `.sot/{settings,keybindings}.toml.example` | disk | fe-ui | `.sot/settings.toml.example`; `.sot/keybindings.toml.example` |
| process `sotd` | process | server | `rust/backend/src/main.rs` `main` |
| the Linux lifetime guard (the launched process, the daemon's parent and a subreaper; it kills what the daemon started when the daemon ends) | process | lifecycle | `rust/backend/src/lifecycle/daemon_children/guard.rs` `require_one_thread`, `install`, `drain`, `guard_pid` |
| process `sot` (the window) | process | fe-ui | `rust/frontend/src/main.rs` `main` |
| starting this computer's daemon (`sot_daemon_ensure`, `sot-local-daemon.ps1`) | process | distribution | `scripts/lib/sot-daemon.sh` `sot_daemon_ensure`; `scripts/sot-local-daemon.ps1` |
| window supervisor (respawn on 75/76, crash-loop rollback) | process | distribution | `scripts/launch-sot.ps1`; `scripts/lib/sot-daemon.sh` `render_sot_launch` |
| process `sot-capsule supervise` (spawned by rows) | process | capsule | `rust/log/src/bin/sot-capsule.rs`; `rust/log/src/supervisor/mod.rs` `supervise`; spawn `rust/backend/src/rows/spawn/detach.rs` `spawn_detached_supervisor`, forked by the durable parent `rust/backend/src/rows/spawn/durable/accept.rs` `accept` (Windows: created by `spawn_detached`) |
| process `sotd durable-parent` (the capsule-only birth parent, one per daemon, started once in `main`'s prologue before the runtime, never restarted) | process | rows | `rust/backend/src/rows/spawn/durable/parent.rs` `run`; start `rust/backend/src/rows/spawn/durable/proxy.rs` `start_before_runtime` |
| a capsule birth's claim on `<sd>/supervisor.lock`, carried from acceptance to the supervisor's first act | disk, lock | capsule | `rust/log/src/supervisor/birth_claim.rs` `BirthClaim`; `rust/log/src/host/lock.rs` `HandoverLock`; adoption `rust/log/src/supervisor/main_loop.rs` `take_authority`; taken by `rust/backend/src/rows/spawn/durable/accept.rs` `accept` |
| process `sot-capsule run` (leg) | process | capsule | `rust/log/src/supervisor/leg.rs` `build_run_command`; `rust/log/src/capsule/writer_loop/mod.rs` `run` |
| the agent program (claude, codex): the launch recipe (the leg runs it) | process | agents | `rust/backend/src/agents/argv.rs` `agent_argv`; `rust/log/src/capsule/producer/pty/mod.rs`; `rust/log/src/capsule/producer/conpty/producer.rs` |
| Julia kernel per workspace | process | sidecars | `rust/backend/src/sidecars/kernel.rs` `Kernel`, `run_one_generation` |
| Julia REPL per workspace and restart retirement | process, state | sidecars | `rust/backend/src/sidecars/repl/mod.rs` `Repl`, `restart_with_project`, `REPL_RESTART_WAIT`; `rust/backend/src/sidecars/repl/supervisor.rs` `spawn_supervisor`, `supervisor_task` |
| Pluto per daemon | process | sidecars | `rust/backend/src/sidecars/pluto.rs` `Pluto`, `spawn_supervisor` |
| a REPL or Pluto child's own X display on Linux (Xvfb, its cookie file), the child's end when it ends, and the end of every process that holds its cookie | process, disk | sidecars | `rust/backend/src/sidecars/display/mod.rs` `Display`, `start`, `ended`, `end`, `Cookie`; `rust/backend/src/sidecars/display/holders.rs` `end`; `rust/backend/src/sidecars/child/tmp.rs` `ChildTmp::for_user_code`, `display_ended`, `retire` |
| MathJax (node) per daemon | process | sidecars | `rust/backend/src/sidecars/mathjax.rs` `MathJax`, `spawn_supervisor`, `supervisor_task`; the constructor supplies `Signal` |
| monitor sampler (`bash -s`, `ssh <alias> bash -s`) | process | sidecars | `rust/backend/src/sidecars/monitor.rs` `spawn_source`, `SAMPLER_SH` |
| quarto render child | process | pages | `rust/backend/src/pages/ops.rs` `run_quarto` |
| `git` child of a site open | process | pages | `rust/backend/src/pages/site/links.rs` `run_git` |
| `gio trash` and its wait budget | process, deadline | files | `rust/backend/src/files/io.rs` `trash_file`, `TRASH_WAIT`; `rust/backend/src/lifecycle/child_signal.rs` `ContainedStd::wait_within` |
| hub-link ssh | process | messaging | `rust/backend/src/comm/mail/hub_link.rs` `recipe_for`, `link_once` |
| comm forward ssh (guest to hub) | process | topology | `rust/backend/src/topology/dial.rs` `dial_and_call`, called by `rust/backend/src/comm/mail/filer.rs` `file_comm` |
| `sotd stdio-bridge` | process | topology | `rust/backend/src/topology/stdio_bridge.rs` `run`, `connect` |
| relay-unit ssh per relayed connection | process | topology | `rust/protocol/src/topology/relay_units.rs` `relay_command_line`; `rust/protocol/src/topology/ssh_bridge.rs` `SshRecipe` |
| window control ssh per host | process | fe-net | `rust/frontend/src/net/transport/mod.rs` `connect_and_run` |
| lane-dial ssh, including the first-voyage spare | process | topology | `rust/protocol/src/topology/lane_client.rs` `DaemonLaneEndpoint`, `dial`, `start_spare`, `take_spare`; `rust/protocol/src/topology/lane_child.rs` `BridgedClient` owns each child and its bounded teardown |
| supervisor-attempt spare abandonment | contract | capsule | `rust/log/src/lane/client.rs` `Endpoint::drop_spare`; called by `rust/log/src/attach_client/worker/converge.rs` `connect_supervisor_lane`, `converge_on_ready`; implemented by `rust/protocol/src/topology/lane_client.rs` `DaemonLaneEndpoint` |
| page-proxy ssh per browser connection | process | pages | `rust/frontend/src/pages.rs` `pipe_one`, `dial` |
| browser opener | process | pages | `rust/frontend/src/pages.rs` `open_url_in_browser`, `open_html_in_browser` |
| drawer shell (portable-pty) | process | fe-ui | `rust/frontend/src/ui/drawer/terminal/pty.rs` `LocalTerminal`, `spawn` |
| updater children (curl, gh, tar, zipinfo, unzip, PowerShell, git, julia instantiate, npm) | process | distribution | `rust/updater/src/fetch/mod.rs`; `rust/updater/src/fetch/archive.rs`; `rust/updater/src/prepare.rs`; daemon lifetime through `rust/backend/src/update.rs` `UpdaterSpawner`, window lifetime through `rust/frontend/src/selfupdate.rs` `WindowSpawner` |
| updater caller spawning policy, `Spawner` | interface | distribution | `rust/updater/src/spawn.rs` `Spawner`; `rust/backend/src/update.rs` `UpdaterSpawner`; `rust/frontend/src/selfupdate.rs` `WindowSpawner` |
| `systemctl` calls of the relay refresh | process | topology | `rust/backend/src/topology/relay_units.rs` `run_systemctl` |
| turn auditor (`claude -p`) | process | messaging | `comm/work_state/comm-turn-auditor.sh` |
| sotd tokio runtime and accept loop | thread | server | `rust/backend/src/main.rs` `main`, `runtime` (built after the serving prologue); `rust/backend/src/server/listen.rs` `run_local` |
| per-connection task and writer | thread | server | `rust/backend/src/server/conn.rs` `handle_connection`, `serve_control`; `rust/backend/src/server/reply.rs` `write_frame_within` |
| off-loop jobs: the `POOLED` permits (4 per connection) and the jobs without one | thread, lock | server | `rust/backend/src/server/reply.rs` `spawn_job`, `OFFLOOP_CONCURRENCY`; `rust/backend/src/server/dispatch.rs` `place`, `INLINE`, `POOLED` |
| lease ticker (1 s) | thread | lifecycle | `rust/backend/src/lifecycle/lease.rs` `ticker`, `tick` |
| shutdown backstop thread | thread | lifecycle | `rust/backend/src/lifecycle/shutdown.rs` `run`, `shutdown_bound` |
| hub link task | thread | messaging | `rust/backend/src/comm/mail/hub_link.rs` `spawn_link` |
| work-state poll task (workspace.list refresh) | thread | rows | `rust/backend/src/rows/work_state/poll.rs` `run` |
| row liveness stamp task (each running row's `last_seen`, every minute) | thread | messaging | `rust/backend/src/comm/registry/liveness.rs` `run` |
| comm wake tick (2 s) | thread | messaging | `rust/backend/src/comm/wake/mod.rs` `run`, `TICK` |
| comm dialog tick (5 s, Unix) | thread | messaging | `rust/backend/src/comm/wake/dialog.rs` `run`, `DIALOG_EVERY` |
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
| `sot-signal-exit` (INT and TERM, with a runtime of its own) | thread | lifecycle | `rust/backend/src/lifecycle/signal_exit.rs` `install` |
| `sot-fe-command-watch` | thread | fe-ui | `rust/frontend/src/ui/control/file_channel.rs` |
| `sot-selfupdate` | thread | distribution | `rust/frontend/src/selfupdate.rs` `spawn_startup_selfcheck` |
| `sot-term-reader` | thread | fe-ui | `rust/frontend/src/ui/drawer/terminal/pty.rs` `run_reader` |
| proxy manager task | thread | pages | `rust/frontend/src/pages.rs` `spawn_proxy_manager`, `serve_browser` |
| lease holder and reader tasks | thread | lifecycle | `rust/frontend/src/lease.rs` `spawn_holder`, `Leases` |
| attach worker, reader, supervisor probe threads | thread | capsule | `rust/log/src/attach_client/worker/mod.rs`; `rust/log/src/attach_client/worker/steady.rs`; `rust/log/src/supervisor/probe/mod.rs` |
| supervisor worker per operation | thread | capsule | `rust/log/src/supervisor/lifecycle.rs` `Lifecycle`; `rust/log/src/supervisor/oneshot.rs` `endrun_inner`, `reset_inner` |
| storage wait: durable state-root probe and its backoff | state, thread | capsule | `rust/log/src/supervisor/storage/mod.rs` `Wait`, `advance`, `probe` |
| lane accept and reaper threads | thread | capsule | `rust/log/src/lane/socket_unix/accept.rs`; `rust/log/src/lane/socket_unix/conn.rs`; `rust/log/src/lane/pipe_win/accept.rs`; `rust/log/src/lane/pipe_win/conn.rs`; `rust/log/src/lane/pending.rs` `PendingJoins`, `Claimed` |
| frame format, codec, 1 MiB cap | wire | wire | `rust/protocol/src/lib.rs` `Frame`; `rust/protocol/src/codec.rs` `read_frame`, `write_frame`, `MAX_ENVELOPE_BYTES` |
| `PROTOCOL_VERSION` | wire | wire | `rust/protocol/src/lib.rs` `PROTOCOL_VERSION` (the shell literal in `comm/lib/comm-lib-client.sh` `sot_hello_frame`, held to it by `the_shell_hello_is_this_protocols_hello`, which runs it) |
| product version, `is_release_build` | wire | wire | `rust/protocol/src/version.rs` `app_version`, `is_release_build`; `rust/protocol/build.rs` |
| IR `TreeNode`, `PreviewPayload`, `BlobDescriptor` | wire | wire | `rust/protocol/src/ir.rs` |
| `hello` | op | server | `rust/backend/src/server/hello.rs` `handle_hello`, `protocol_gate`, `admit_hello`, `register_hello`; the host account record `rust/backend/src/clients.rs` `admit_account`; every Rust client builds it with `rust/protocol/src/ops/session.rs` `HelloReq::this_process` |
| `ping` | op | server | `rust/backend/src/server/conn.rs` `handle_ping` |
| `version.query` | op | server | `rust/backend/src/clients.rs` `handle_version_query` |
| `fe.presence`; the active frontend | op, state | server | `rust/backend/src/clients.rs` `handle_fe_presence`, `ActiveFrontend`, `resolve_active` |
| which window a command goes to | state | server | `rust/backend/src/clients_show.rs` `ClientsSnapshot::window_for`, `ClientsSnapshot::in_use` |
| `fe.command.send` and the `fe.command` bus | op, bus | server | `rust/backend/src/clients.rs` `handle_fe_command_send`; `rust/backend/src/server/events.rs` `write_fe_command` |
| `FeCommand` meaning and its handling in the window | wire payload | fe-ui | `rust/frontend/src/ui/control/command.rs`; `rust/frontend/src/ui/control/dispatch.rs` |
| `fe.sessions` | op | server | `rust/backend/src/clients.rs` `handle_fe_sessions`, `declare_sessions` (see two owners) |
| client roster `by_conn`, `disconnected` | state | server | `rust/backend/src/clients.rs` `Clients`, `ClientGuard`, `disconnected_since` |
| a window's state across its connections (the REPL runs it numbered, its presence, the owner of its pages) | state | server | `rust/backend/src/clients_window.rs` `Window`, `Inner::arrive`, `Inner::leave`, `Inner::report`, `Clients::away_sites` |
| revision ring, `session_id`, replay at hello | state | server | `rust/backend/src/session.rs` `Session`, `bump`, `replay_after`; `rust/backend/src/server/hello.rs` `handle_hello` |
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
| `repl.frame` | event, bus | sidecars | `rust/backend/src/sidecars/repl/mod.rs` `Repl`, `ReplFrameMsg`; `rust/backend/src/server/events.rs` `write_repl_frame`, `repl_frame_for` |
| a REPL run's number: the daemon's one counter, and a window's own numbers for its own runs and whether each still runs | state | sidecars | `rust/backend/src/sidecars/repl/numbers.rs` `next`, `CallerNumbers` (`ran`, `end_then`, `running`) |
| `pluto.open` | op | sidecars | `rust/backend/src/sidecars/ops.rs` `handle_pluto_open` |
| `monitor.subscribe`, `.unsubscribe`, `.history`, `monitor.tick` | op, event, bus | sidecars | `rust/backend/src/sidecars/ops.rs` `handle_monitor_subscribe`, `handle_monitor_unsubscribe`, `handle_monitor_history`; `rust/backend/src/sidecars/monitor.rs` `MonitorHub`; `rust/backend/src/server/events.rs` `write_monitor_tick` |
| kernel stdio ops (`kernel.hello`, `file.preview`, `file.parse`, `project.scan`, `markdown.tokenize`) | wire (stdio) | sidecars | `julia/kernel/src/ShipToolsKernel.jl`; `julia/kernel/src/preview.jl`; `julia/kernel/src/project_scan.jl`; `julia/kernel/src/tokenize.jl` |
| REPL stdio frames, `repl.ready` | wire (stdio) | sidecars | `julia/repl/src/ShipToolsRepl.jl`; `julia/repl/src/frames.jl` |
| Pluto `READY`/`OPEN`/`URL`/`ERR` lines | wire (stdio) | sidecars | `julia/pluto/start.jl` |
| MathJax `{"ready":true}`, then `{id, tex, display}` | wire (stdio) | sidecars | `rust/backend/sidecars/mathjax/render.mjs` |
| a child's output read as lines: the bytes up to each `\n`, decoded lossily, so no byte a child writes ends its reader; the strict line readers are refused | rule | wire | `rust/protocol/src/child_lines.rs` `ChildLines`, `blocking_child_lines`; `rust/clippy.toml` (child-output group) |
| plugin ABI (`FileType`, `matches`, `preview`, `file_type_for`) | ABI | sidecars | `core/src/ConceptExplorerCore.jl` `FileType`, `file_type_for` |
| core's Julia `TreeNode` (never constructed outside core's tests; not the wire's `TreeNode`) | ABI | sidecars | `core/src/ConceptExplorerCore.jl` `TreeNode` |
| core's Julia `PreviewPayload` (`mime`, `data`, `extras`: what a plugin's `preview` returns; the kernel sends it as `blob_base64`) | ABI | sidecars | `core/src/ConceptExplorerCore.jl` `PreviewPayload`; `julia/kernel/src/preview.jl` |
| `video.open`, `docs.open`, `quarto.open` | op | pages | `rust/backend/src/pages/ops.rs` `handle_video_open`, `handle_docs_open`, `handle_quarto_open` |
| Quarto render-failure text: bounded ERROR/tail selection and full-stderr transport guidance | rule | pages | `rust/backend/src/pages/ops.rs` `quarto_render_error`; `rust/backend/src/pages/ops_error_tests.rs` |
| `proxy.connect` and the loopback allowlist | op, state | pages | `rust/backend/src/pages/proxy.rs` `handle_proxy_connect`, `allowed_proxy_ports` |
| loopback page-URL grammar (`http` or `https`, host `127.0.0.1` or `localhost`, an explicit port) | rule | pages | `rust/protocol/src/page_url.rs` `loopback_port_from_url`, `with_loopback_port` |
| video suffix and MIME decision in Rust | rule | wire | `rust/protocol/src/video_path.rs` `video_mime` |
| video, site-prefix, site-pool listeners and grant tables | endpoint, state | pages | `rust/backend/src/pages/video.rs` `Grants`, `register_video`; `rust/backend/src/pages/site/mod.rs` `spawn`, `spawn_pool`, `set_root` |
| window page-proxy listeners and arming | endpoint | pages | `rust/frontend/src/pages.rs` `serve_browser`, `Arm`, `PageListener`; `rust/frontend/src/ui/page_proxy.rs` `ensure_proxy_for_url`, `PageSource`, `bind_proxy_listener`, `armed_url` |
| Pluto's page server, notebook workers and supervisor proxy grant | endpoint, state | sidecars | `julia/pluto/start.jl`; `julia/pluto/session_options.jl` `configure_session!`; `rust/backend/src/sidecars/pluto.rs` `supervisor_task`, `bound_pluto_port` |
| `wglshow`'s page server, bound once per REPL lifetime | endpoint | sidecars | `julia/repl/src/wgl.jl` `page_server`, `wgl_server`, `no_referrer_page`, `WGL_SERVER` |
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
| label-to-slug rule `slug()` (row slug, session name, socket and pipe names) | rule | rows | `rust/protocol/src/topology/endpoint.rs` `slug` (the shell's `sot_slug` runs it through `sotd comm name-part slug`); window copy `rust/frontend/src/ui/session/switch.rs` `session_name_of` (see two owners) |
| default anchor row | state | rows | `rust/backend/src/rows/anchor.rs` `seed_default_row`; `rust/backend/src/rows/registry.rs` `default_id`, `set_default` |
| `accounts.list` | op | agents | `rust/backend/src/agents/ops.rs` `handle_accounts_list` |
| `agent.send` (a UI notification; mail is refused) | op | messaging | `rust/backend/src/comm/mail/relay.rs` `handle_agent_send` |
| `agent.message` (a UI notification) | event, bus | messaging | `rust/backend/src/comm/mail/bus.rs`; `rust/backend/src/server/events.rs` `write_agent_message` |
| the hub's linked daemons and their offers (`link` hello role) | state, op | messaging | `rust/backend/src/comm/mail/links.rs` `Links`, `LinkEnd`; `rust/protocol/src/ops/session.rs` `LINK_ROLE` |
| `comm.file` | op | messaging | `rust/backend/src/comm/mail/filer.rs` `handle_comm_file`, `comm_file_verdict` |
| `agent.join`; a row's `agent_handle` (stored in the row toml) and its bound handle | op, state | rows | `rust/backend/src/rows/ops/join.rs` `handle_agent_join`; `rust/backend/src/rows/workspace.rs` `agent_handle`; `rust/backend/src/rows/handle.rs` `bound_handle` |
| work-state fields (`floor, question, waiting, dialog, done, note, state, summary, status_at`) | state | rows | `rust/backend/src/rows/work_state/mod.rs`; `comm/work_state/comm-status.sh` |
| the Stop hook's `stop_at` mark | state | messaging | `rust/backend/src/comm/cli/door.rs` `stop_mark` (called by `comm/work_state/hooks/comm-status-idle.sh`); read `rust/backend/src/comm/wake/mod.rs` `stop_hook_running`; deleted by rows' `stop` |
| a Claude row's `dialog`, read from its screen (`dialog_of`), and its write | rule | rows (the tick), messaging (the screen reader) | `rust/backend/src/comm/wake/screen.rs` `dialog_of`, `main_box_shown`, `is_dialog_footer`; `rust/backend/src/rows/work_state/dialog.rs` `due`, `stamp_for`, `stamp_dialog` |
| `read_clear` (clears `done` on view) and every work-state write | state | rows | `rust/backend/src/rows/work_state/mod.rs` `apply_event`, `write_event`, `read_clear`; `sotd work-state` (`cli.rs`) |
| `update.check`, `update.apply` | op | distribution | `rust/backend/src/update.rs` `handle_update_check`, `handle_update_apply` |
| `sotd update stage <folder> --prefix <prefix>`, `sotd update pointer --prefix <prefix> --target <target>`, `sotd update direction <version> --prefix <prefix>`, `sotd update repls --endpoint <endpoint>` | command | distribution | `rust/backend/src/update/cli.rs` `stage_cli`, `pointer_cli`, `direction_cli`, `repls_cli`; `rust/backend/src/update.rs` `stage_prepare_arm_with` |
| The Windows update entry's decisions: the target channel (`scripts/install.ps1`'s prelude), the direction, the `+` stop, the live-work ask, the path by what runs (converge sentinel, armed with a step, applied here), the one verdict line on every exit, the pin | rule | distribution | `scripts/install.ps1` `Invoke-Entry`, `Get-Pack`; `rust/backend/src/install/windows.rs` `update_or_repair`, `update`, `supervisor`, `end_state`; `rust/backend/src/install/verdict.rs`; `rust/updater/src/pending.rs` `InstalledBuild::admits`, `pin_note` |
| The Unix update entry's decisions: the target channel from `install.json` (`scripts/install.sh`'s prelude), the direction (never backwards unless told), the role recovered from the record, the daemon restart, its verification and its live-REPL question, the one verdict line on every exit, the socket an enrolled frontend's window dials for this computer's own daemon | rule | distribution | `scripts/launch-sot.sh` (`SOT_OWN_DAEMON`), `rust/backend/src/install/unix.rs` `decide`, `question`, `check`, `daemon_after`; `role.rs` `recorded`, `plan_dials_others`; `verdict.rs`; `daemon.rs`; `rust/backend/src/update/cli.rs` `direction_cli`, `repls_cli` |
| What may be armed into an install, and which pointer may apply onto it: strictly above `install.json`'s version for an automatic writer, a record of the install armed over | rule | distribution | `rust/updater/src/pending.rs` `arm`, `InstalledBuild`, `stale_reason`; `rust/backend/src/update.rs` `arm_refusal`, `apply_refusal`; `scripts/sot-apply.sh` `ptr_refusal`; `scripts/sot-apply.ps1` `Get-PointerRefusal` |
| `topology.set` | op | topology | `rust/backend/src/topology/set.rs` `handle_topology_set` |
| `topology.changed` | event, bus | topology | `rust/backend/src/topology/store.rs` `TopologyChanged`; `rust/backend/src/server/events.rs` `write_topology_changed` |
| capsule lanes SOM0, SOA0 (v1-v3), SOSV | wire | capsule | `rust/log/src/lane/wire/mgmt.rs`; `rust/log/src/lane/wire/attach.rs`; `rust/log/src/lane/wire/supervisor.rs` |
| supervisor-lane build id `SUPERVISOR_LANE_BUILD_ID` (stamp `SOT_LOG_BUILD_SHA`: full sha, `-dirty`; carried in the lane hello and `version.query`, never compared) | wire (lane) | capsule | `rust/log/build.rs`; `rust/log/src/identity/exchange.rs`; `rust/log/src/attach_client/supervisor_client.rs` `SUPERVISOR_LANE_BUILD_ID` |
| rollout gate (`gate`, `RolloutEvidence`; `--assume-no-rollback-target`, which every daemon spawn passes) | rule | capsule | `rust/log/src/store/rollout.rs` `gate`; `rust/log/src/capsule/mod.rs` `RolloutEvidence`; `rust/log/src/bin/sot-capsule.rs`; `rust/backend/src/rows/spawn/detach.rs` `mode_flag` |
| `<sd>/rollout-evidence.json` and its reader `read_rollout_evidence` (provisional; no writer, no caller) | disk | capsule | `rust/log/src/store/rollout.rs` `read_rollout_evidence` |
| SDK helper-protocol 1 | wire | capsule | `rust/log/src/claude.rs`; `rust/log/claude-sdk-helper/src/protocol.ts` |
| sotd subcommand `session-socket-path` | CLI | topology | `rust/backend/src/main.rs` `main`; `rust/protocol/src/topology/endpoint.rs` `session_socket_path` |
| sotd subcommand `log-path` | CLI | server | `rust/backend/src/main.rs` `main`; `rust/backend/src/paths.rs` `state_dir` |
| sotd subcommands `topology ...`, `status`, `stdio-bridge` | CLI | topology | `rust/backend/src/main.rs` `parse_args`; `rust/backend/src/topology/cli.rs` `run`; `rust/backend/src/topology/status.rs` `run`; `rust/backend/src/topology/stdio_bridge.rs` `run` |
| sotd subcommand `ancestors` | CLI | messaging | `rust/backend/src/comm/registry/ancestors.rs` `run` |
| sotd subcommand `trust declare` | CLI | agents | `rust/backend/src/agents/trust_declaration.rs` `declare_trust`; routing `rust/backend/src/main.rs` |
| sotd subcommand `agent-exec` | CLI | agents | `rust/backend/src/agents/ops.rs` `agent_exec` |
| sotd subcommand `durable-parent` (private; its standard input is the daemon's channel) | CLI | rows | `rust/backend/src/rows/spawn/durable/parent.rs` `run`; routing `rust/backend/src/main.rs` |
| sot flags `--dial`, `--socket` | CLI | fe-net | `rust/frontend/src/cli.rs` `Cli`; `rust/frontend/src/net/dial.rs` `parse_dial_arg`, `resolve_connections` |
| sot flags `--capture`, `--ephemeral` | CLI | fe-ui | `rust/frontend/src/cli.rs` `Cli`; `rust/frontend/src/ui/render/capture.rs` |
| sot flag `--no-lease` | CLI | lifecycle | `rust/frontend/src/lease.rs` `lease_exempt` |
| sot flags `--update-status`, `--relaunched` | CLI | distribution | `rust/frontend/src/selfupdate.rs` `print_status`; `rust/frontend/src/cli.rs` `Cli` |
| sot flag `--token`, `SOT_TOKEN` | CLI | fe-net | `rust/frontend/src/cli.rs` `Cli` |
| sotd exit 0 (requested shutdown) | exit code | lifecycle | `rust/backend/src/lifecycle/shutdown.rs` `REASON`; `rust/protocol/src/ops/lease.rs` `EXIT_REQUESTED_SHUTDOWN` |
| the serving daemon's one raw exit, and the child fire that comes first (`FIRE_WAIT`) | process rule | lifecycle | `rust/backend/src/lifecycle/shutdown.rs` `exit`, `terminal` |
| sotd exit 0, 1 and 101 (the main future's result) | exit code | server | `rust/backend/src/main.rs` `complete_main` |
| sotd's end by INT and TERM (the fire and the children's folders, then death by the signal) | exit code | lifecycle | `rust/backend/src/lifecycle/signal_exit.rs` `install`; `rust/backend/src/lifecycle/shutdown.rs` `exit_by_signal` |
| sotd exit 75 (update restart) | exit code | distribution | `rust/backend/src/update.rs` `exit_for_update` (committed under the lease by lifecycle's `Leases::commit_update`); `rust/protocol/src/ops/lease.rs` `EXIT_UPDATE_RESTART` |
| sot exit 75/76 (relaunch, converge) | exit code | distribution | `rust/frontend/src/lease_exit.rs` `exit_intent`, `close_now`; `rust/frontend/src/relaunch.rs` |
| sot-capsule exit 0/69/70 (supervise) and 71 (run: storage exhaustion) | exit code | capsule | `rust/log/src/supervisor/mod.rs`; `rust/log/src/capsule/mod.rs` `EXIT_LEG_STORAGE_FULL`; `rust/log/src/bin/sot-capsule.rs` `leg_exit_code` |
| `SOT_SOCKET`, `SOT_SESSION`, `SOT_WORKSPACE`, `SOT_WORKSPACE_ID`, `SOT_WORKSPACE_ROOT`, `SOT_MANUAL` | env | agents | `rust/backend/src/agents/awareness.rs` `awareness_env` |
| `SOT_COMM_SELF_FILE` (issued at spawn) | env | messaging | `rust/backend/src/agents/env.rs` `capsule_supervisor_env`; read `rust/backend/src/comm/identity/context.rs` `resolve`, `rust/backend/src/comm/cli/join.rs` `join` |
| `SOT_COMM_MSYS`, `SOT_COMM_BIN`, `SOT_COMM_TEST_HOST` | env | messaging | set by `comm/lib/comm-lib-sotd.sh` `sot_comm` (the test host by a suite); read `rust/backend/src/comm/identity/layers.rs`, `rust/backend/src/comm/cli/mod.rs` `bin_dir`, `rust/backend/src/comm/identity/context.rs` `raw_host` |
| `SOT_COMM_HOOKS`, `SOT_LOCK_WAIT_SECS`, `SOT_INBOX_LOCK_WAIT_SECS`, `SOT_INBOX_READ_WAIT_SECS`, `SOT_SEND_TIMEOUT`, `SOT_COMM_ASKQ_ID`, `SOT_COMM_EXPERTISE`, `SOT_HB_CTX_TIMEOUT_TICKS`, `SOT_TURN_AUDITOR`, `SOT_AUDITOR_*` | env | messaging | `rust/backend/src/comm/registry/registry.rs` `command_lock_wait`; `rust/backend/src/comm/mail/inbox.rs`; `comm/lib/comm-lib-sotd.sh`; `comm/work_state/hooks/comm-status-heartbeat.sh`; `comm/work_state/comm-turn-auditor.sh` |
| `SOT_COMM_SPAWN_CAPSULE_WAIT`, `SOT_FE_ENDPOINT`, `SOT_SPAWN_ENDPOINT`, `SOT_NAV_DRY_RUN`, `SOT_ACCOUNT` | env | agents | `agents/spawn/comm-spawn.sh`; `agents/sot-fe/sot-fe`; `agents/spawn/comm-bootstrap.sh`; `rust/backend/src/agents/accounts.rs` `account_env` |
| `SOT_PROBE_READ_TIMEOUT`, `SOT_PROBE_READY_WAIT` | env | agents | `agents/spawn/comm-probe.sh` |
| `GH_OAUTH_CLIENT_ID`, `SOT_GH_SCOPES` (sot-gh-auth also honours gh's own `GH_HOST`, `GH_CONFIG_DIR`) | env | agents | `agents/sot-gh-auth.sh` |
| `SOT_BACKEND_LABEL`, `SOT_RELAY_SOTD`, `SOT_RELAY_TARGET` | env | topology | `rust/protocol/src/topology/relay_units.rs` `relay_command_line`; `rust/protocol/src/topology/endpoint.rs` `local_endpoint`; `comm/lib/comm-lib-client.sh` `sot_daemon_endpoint` |
| `SOT_JULIA_BIN`, `SOT_NODE_BIN`, `QUARTO_JULIA` | env | sidecars | `rust/backend/src/sidecars/julia.rs` `resolve_bin`; `rust/backend/src/sidecars/mathjax.rs` `default_script_path`; `QUARTO_JULIA` is set for quarto by `rust/backend/src/pages/ops.rs` `run_quarto` |
| Which Julia binary the daemon runs: absolute override, juliaup default channel, then a verified PATH candidate; Windows app-execution aliases refused by file tag | rule | sidecars | `rust/backend/src/sidecars/julia.rs` `resolve_bin`; read by `rust/backend/src/update.rs` `prepare_julia` and `rust/backend/src/pages/ops.rs` `run_quarto` |
| `SOT_WATCH_BUDGET` | env | files | `rust/backend/src/files/watcher.rs` `watch_budget` |
| `SOT_VIDEO_PORT`, `SOT_DOCS_PORT`, `SOT_PROXY_EXTRA_PORTS` | env | pages | `rust/backend/src/pages/video.rs` `video_port`; `rust/backend/src/pages/site/mod.rs` `site_port`; `rust/backend/src/pages/proxy.rs` `allowed_proxy_ports` |
| `SOT_SETTINGS`, `SOT_KEYBINDINGS`, `SOT_PROJECTS_ROOT`, `SOT_REMOTE_HOME` | env | fe-ui | `rust/frontend/src/ui/persist/discover.rs` `find_config_file`; `rust/frontend/src/ui/persist/settings.rs`; `rust/frontend/src/ui/input/keybindings.rs`; `rust/frontend/src/ui/session/picker.rs` |
| `SOT_SELFIE_DIR` | env | fe-ui | `rust/frontend/src/ui/render/capture.rs` |
| `SOT_FE_INSTANCE` | env | fe-net | `rust/frontend/src/net/identity.rs` `resolve_fe_instance_component` |
| `SOT_PROJECT_ROOT` | env | server | `rust/backend/src/main.rs` `main` |
| `SOT_UPDATE_MODE`, `SOT_UPDATE_REPO`, `SOT_UPDATE_ROOT`, `SOT_UPDATE_FETCHER`, `SOT_NO_UPDATE`, `SOT_APPLY*`, `SOT_INSTALL_*`, `SOT_PREFIX`, `SOT_BIN`, `SOT_FRONTEND_BIN`, `SOT_LAUNCH_*`, `SOT_REPO_DIR`, `SOT_HOST_NAME`, `SOT_RESTART_BE`, `SOT_BACKEND_LOG`, `SOT_BUILD_*` | env | distribution | `rust/backend/src/update.rs` `mode_from_env`; `rust/updater/src/fetch/mod.rs`; `rust/updater/src/manifest.rs`; `rust/backend/src/install/mod.rs`; `scripts/install.sh`; `scripts/install.ps1`; `scripts/launch-sot.sh`; `scripts/launch-sot.ps1`; `rust/protocol/build.rs` |
| `SOT_MEDIA_BIN`, `SOT_MEDIA_FONT_SCALE`, `SOT_MEDIA_KEEP`, `SOT_MEDIA_AGENT_TIMEOUT` | env | records | `docs/tools/docs-media.sh` |
| `SOT_TEST_*` seams read by production binaries: the daemon's shutdown and lease bounds | env | lifecycle | `rust/backend/src/lifecycle/shutdown.rs` `OVERRIDE_MS`; `rust/backend/src/lifecycle/lease.rs` `OVERRIDE_MS` |
| `SOT_TEST_MAIN_OUTCOME`, `SOT_TEST_GATES`, `SOT_TEST_RELEASE_BUILD`, `SOT_TEST_PROLOGUE_THREAD`: read only by a binary built with `daemon-lifetime-faults` (an installed binary reads none) | env | lifecycle | `rust/backend/src/main.rs` `injected_outcome`; `rust/backend/src/lifecycle/test_gates.rs`; `rust/backend/src/update.rs` `Updater::from_env` |
| `SOT_TEST_*` seams read by production binaries: the connection read deadline | env | server | `rust/backend/src/server/conn.rs` `OVERRIDE_MS` |
| `SOT_TEST_*` seams read by production binaries: the registry lock's timings | env | messaging | `rust/backend/src/comm/registry/lock.rs` |
| `SOT_TEST_*` seams read by production binaries: the supervisor's recovery and journal-publish delays | env | capsule | `rust/log/src/supervisor/lifecycle.rs` `spawn_recovery`; `rust/log/src/supervisor/journal/mod.rs` `test_publish_delay` |
| `SOT_TCP_PORT`, `SOT_REMOTE_REPO`, `SOT_REMOTE_SOCKET` (retired: only scripts and skills still name them) | env | distribution | `scripts/shutdown-sot.ps1`; `agents/claude/sot-setup/SKILL.md` |
| fan-in `(HostKey, IncomingEvt)` and per-host `OutgoingReq` senders | bus | fe-net | `rust/frontend/src/main.rs` `main`; `rust/frontend/src/net/transport/event.rs` `IncomingEvt`; `rust/frontend/src/net/transport/request.rs` `OutgoingReq` |
| `LinkGate` (one per host; the window's transport is its only writer) | state | topology | `rust/protocol/src/topology/ssh_bridge.rs` `LinkGate`; written `rust/frontend/src/net/transport/mod.rs` |
| per-host table (`host_connected`, `host_transports`, `host_resolved_dial`, `link_gates`, `declared_host`, `reconnect_now`) | state | fe-net | `rust/frontend/src/net/hosts.rs` `HostTable`; `rust/frontend/src/ui/connections.rs` |
| `FrontendIdentity` | state | fe-net | `rust/frontend/src/net/identity.rs` `FrontendIdentity`, `frontend_identity` |
| waking the window from another thread (`MainWake`: a sender queues its event, then wakes; at most one user event outstanding) and the window handle (`MainWindow`, `!Send`, its `Arc<Window>` private to it but for wgpu's surface) | type, rule | fe-ui | `rust/frontend/src/ui/app/mod.rs` `MainWake`, `main_window`; `App::user_event` in `rust/frontend/src/ui/app/handler.rs` |
| native pane harness layout and monitor receipt | test rule | fe-ui | `rust/frontend/src/ui/app/native_pane_tests.rs` `pane_child_settings`, `child`, `PaneDriver::resumed`, `layout_state`, `setup_state`, `verdict`; in-memory Laptop preset, no settings-file discovery |
| native harness ticker and dispatch | test state, rule | fe-ui | `rust/frontend/src/ui/app/native_children.rs` `HarnessTick`; pending tick count, one work step per positive consumption; progress credits the count, close and pane perform one step; production wake handling remains in App |
| native progress ledger and report snapshot | test state, lock | fe-ui | `rust/frontend/src/ui/app/tests.rs` `NativeProgressLedger`, `PhaseLedger`, `ProgressSnapshot`; real enqueue/dequeue observations, per-second rates, output after unlock; samples count consumed ticker credit within a phase, with one actual native-state observation per batch |
| steady control read/write scheduling | state, rule | fe-net | `rust/frontend/src/net/transport/steady.rs` `steady_loop`; request correlation remains in PendingGuard; the silence bound `SILENT_PINGS` |
| `Signal` and its tree registry, `Signal::spawn`, `Signal::spawn_std`, `Signal::output`, `Contained`, `ContainedStd`, `fire`, macOS group EPERM and rejected-observation evidence | state, lock, rule | lifecycle | `rust/backend/src/lifecycle/child_signal.rs` `Signal`, `Signal::spawn`, `Signal::spawn_std`, `Signal::output`, `Contained`, `Contained::wait_until_exited`, `Contained::interrupt`, `ContainedStd`, `fire`; `rust/backend/src/lifecycle/contain.rs` `Tree`, `request`, `interrupt`, `macos::checked_no_live_group` |
| process-start rule: each call of a function in `rust/clippy.toml`'s process-spawns group outside `Signal::spawn`, `Signal::spawn_std` and `Signal::output` is a reasoned exception; what the group does not hold is named in rust/backend/src/lifecycle/CLAUDE.md | rule | lifecycle | `rust/clippy.toml` (process-spawns group); `rust/backend/src/lifecycle/child_signal.rs` `Signal::spawn`, `Signal::spawn_std`, `Signal::output` |
| test-wait rule: a test runs a child to its end through `sot_log::test_wait::run_within`, whose failure carries the child's command, exit status and both streams (`try_run_within` returns that failure as a value instead, for a cleanup, which must not panic, or a probe whose failure a skip names); a child the test holds while it runs (it writes the child's input, uses its pid, signals it or runs it beside other work) it waits on with `sot_log::test_isolated::drain(..).wait_within` when it piped the child's streams, which returns or fails with the status and both streams, and otherwise with `sot_log::test_isolated::wait_within`; every other wait a test makes on a socket, pipe, channel or child whose other end is another process or the code under test has a bound (`within`, `recv_timeout`, `tokio::time::timeout`) | rule | capsule | `rust/log/src/test_wait.rs` `run_within`, `try_run_within`, `Ran`, `within`; `rust/clippy-tests/clippy.toml` |
| `Leases`, its mutex and phase | state, lock | lifecycle | `rust/backend/src/lifecycle/lease.rs` `Leases`, `Phase` |
| window exit decision (`ExitReason`, `ExitStep`, `exit_intent`, `close_now`, the stay) | state | lifecycle | `rust/frontend/src/lease_exit.rs` `ExitReason`, `ExitStep`, `exit_intent`, `close_now`, `Leases::stay`, `stay_line` |
| no-lease notice and precedence | rule | lifecycle | `rust/frontend/src/lease.rs` `lease_notice` |
| quit prompt, `request_quit`, the close dialog | UI | fe-ui | `rust/frontend/src/ui/app/exit.rs` `quit_prompt_step`, `request_quit`; `rust/frontend/src/ui/app/dialog.rs` `close_dialog`, `quit_prompt_click`; `rust/frontend/src/ui/input/keypress.rs` `keyboard_input`; `rust/frontend/src/ui/input/mouse.rs` `mouse_input`; `rust/frontend/src/ui/input/global_keys.rs` `confirm_quit_key` |
| returned-loop runtime shutdown | process rule | fe-ui | `rust/frontend/src/ui/app/mod.rs` `App::run`, `shutdown_transport` |
| final window process deadline | process rule | fe-ui | `rust/frontend/src/ui/app/exit.rs` `ExitDeadline`; `rust/frontend/src/ui/app/mod.rs` `App::run` |
| new-row account selection | rule | fe-ui | `rust/frontend/src/ui/session/picker.rs` `selected_account`; daemon default resolution stays in agents |
| parent-death lease (fd-3 pipe; Windows `Local\sot-lease-*` mutex) | lock | capsule | `rust/log/src/supervisor/lease_win.rs`; `rust/log/src/supervisor/leg.rs` `SpawnLease`, `LegLease` |
| REPL `OUT_LOCK` | lock | sidecars | `julia/repl/src/ShipToolsRepl.jl` |
| result target resolution and row attachment names | rule | fe-ui | `rust/frontend/src/ui/session/workspace_key.rs` `resolve_listed_workspace`; row facts from `workspace.list` |
| pending-result lifetime and acknowledgement | state, rule | fe-ui | `rust/frontend/src/ui/session/badge.rs` `PendingNav`, result/attempt serials and admission; authoritative list/destroy invalidation, cursor, preview and presentation hooks; fe-net carries the saved attempt through its existing pending entry and event |
| text from outside the window drawn in a cell run, a strip name or the Monitor's chart: a CSI sequence is removed, `\r\n`, `\n`, a lone `\r`, U+0085, U+2028 and U+2029 end a line, any other control character is a space, and where only one row exists the lines are joined by ` ⏎ ` | rule | fe-ui | `rust/frontend/src/ui/chrome/status.rs` `cell_lines`, `cell_row` |
| text into cosmic-text or usvg: nothing else sets a buffer's text, and every bidi paragraph separator but `\n` and `\r` becomes a space, an SVG's character references included, so no line shaped holds two paragraphs | rule | fe-ui | `rust/frontend/src/ui/render/text.rs` `set_buffer_text`, `set_buffer_rich_text`; `rust/clippy.toml` (text-into-the-shaper group); `rust/frontend/src/ui/preview/image/svg.rs` `one_paragraph_svg` |

## Two owners today

Where two copies of one rule or one file exist, the owner is named and the other copy is listed. Kind: **pure** is a
move or the deletion of an identical copy; **extract** keeps behaviour but is not a move; **behaviour** changes what a
user or another process sees.

| Case | Owner | Copies live in | Every other copy becomes | Kind |
|---|---|---|---|---|
| A session's handle in four places: self-file, registry key, row toml `agent_handle`, hello `name` | messaging | `rust/backend/src/comm/identity/self_file.rs` `write`, `pin_row`; `rust/backend/src/rows/handle.rs` `bound_handle` | The row toml is the daemon's one record of which row holds the handle and `bound_handle` its one resolver; the self-file is the session's record (a named row's start writes it), the registry key the address book's index, hello `name` a display label | behaviour |
| settings.toml on Windows is two files: the window reads a clone-local file through `SOT_SETTINGS`, sotd reads `<config>/settings.toml`; platform decides where (`<config>`), agents own `[trust]` | fe-ui | `scripts/launch-sot.ps1`; `rust/frontend/src/ui/persist/discover.rs` `find_config_file`; `rust/backend/src/agents/trust_declaration.rs` `read_trust_declaration` | The window reads `<config>/settings.toml` like sotd; the repo-local settings file under `.sot/` (untracked; `.sot/settings.toml.example` shows its shape) stays a dev override | behaviour |
| Hand TOML parsers: hosts.toml, the row store, the settings and resume files; each file's owner keeps its schema, the parser is the `toml` crate | platform | `rust/protocol/src/topology/mod.rs` `parse`; `rust/backend/src/rows/store/codec.rs` `parse_kv`, `toml_quote`, `toml_unquote`; `rust/frontend/src/ui/persist/settings.rs` `parse`; `rust/frontend/src/ui/persist/resume.rs` `strip_quotes`, `toml_quote` | Each remaining hand parser becomes `toml::from_str` into a typed struct | behaviour |
| Workspace view/reply/lifecycle key compatibility | fe-ui | `rust/frontend/src/ui/session/workspace_key.rs` `ws_key_of`, `reply_ws_key`, `lifecycle_key_of`, `active_repl_starting`, `migrate_default_slug_keys` | Command ingress resolves listed rows once; the remaining view/reply/lifecycle compatibility helpers keep their existing key spaces | behaviour (remaining) |
| Remaining host-name copies: the window's resume lookup, comm_self_host alias and host_matches compatibility | platform | `rust/frontend/src/ui/persist/resume.rs` `state_path`; `rust/backend/src/comm/mail/filer.rs` `comm_self_host`; `rust/backend/src/comm/registry/registry.rs` `host_matches` | Shell registry facts and self slots now use sot_host; raw host remains a handle component and legacy-slot input. Remaining copies require their own proved deletion. | pure |
| State-dir rule written twice (Windows `...\sot` against `...\sot\state`; Unix fallback) | platform | `rust/log/src/host/state_dir.rs` `sot_state_dir`; `rust/backend/src/paths.rs` `state_dir` | `paths.rs` `state_dir` is deleted; sotd.log uses `sot_state_dir()` | behaviour |
| Config-dir rule written more than once | platform | `rust/log/src/host/state_dir.rs` `sot_config_dir`; `rust/frontend/src/ui/persist/resume.rs` `config_dir`; `rust/frontend/src/ui/persist/discover.rs` `find_config_file` | Both walks call `sot_config_dir()` | behaviour |
| Leg-gone check written twice (any lock error is ambiguous in one, string-matched contention in the other) | capsule | `rust/log/src/supervisor/journal/end_run.rs`; `rust/backend/src/rows/run/end_run.rs` `is_lock_contention`, `absence_proof` | Both call one function in the store | behaviour |
| Pipe and socket lane servers written as twins | capsule | `rust/log/src/lane/pipe_win/server.rs`; `rust/log/src/lane/socket_unix/server.rs` | One bridge generic over `LaneServer` | extract |
| Endpoint grammar parsed in each client and in shell | topology | `rust/frontend/src/net/dial.rs` `parse_dial_arg`; `rust/backend/src/comm/mail/hub_link.rs` `recipe_for`; `rust/backend/src/topology/dial.rs` `connect`; `rust/backend/src/topology/stdio_bridge.rs` `local_endpoint`; shell `comm/lib/comm-lib-client.sh` `sot_ssh_bridge`, `sot_dial` | One `Endpoint` type in the protocol crate; the Rust clients call it; the shell copy stays, pinned by a test | pure (Rust) |
| ssh option sets differ between the protocol recipe, the relay unit, the shell client, the hub-link dial and the monitor sampler | topology | `rust/protocol/src/topology/ssh_bridge.rs` `SSH_OPTS`; `rust/protocol/src/topology/relay_units.rs` `relay_command_line`; `comm/lib/comm-lib-client.sh` `_sot_ssh_control`; `rust/backend/src/topology/dial.rs`; `rust/backend/src/sidecars/monitor.rs` `spawn_source` | One option set and one stderr capture in `SshRecipe` | behaviour |
| Repo default twice | distribution | `rust/updater/src/identity.rs` `DEFAULT_REPO` | One `from_env` for the repo default in the updater (who runs a backend is already one function, `sot_protocol::topology::runs_backend`, read by the prepare, the window and the apply's role check) | pure |
| Durable replace written several ways | platform | `rust/log/src/host/durable.rs` `publish_noreplace`; `rust/backend/src/durable.rs` `replace_file`; `rust/backend/src/files/io.rs` `write_file`; `rust/backend/src/files/concept.rs` `write`; `rust/backend/src/topology/store.rs` `write_atomic`; `rust/backend/src/topology/relay_units.rs` `write_atomic`; `rust/backend/src/agents/folder_trust.rs` `publish_trust_file`; `rust/backend/src/comm/registry/registry.rs` `write_synced` | One durable write | behaviour |
| Workspace confinement more than one way | files | `rust/backend/src/files/confine.rs` `path_within_root`, `canonical_under_root`; `rust/backend/src/files/tree.rs` `FilesMode`; `rust/backend/src/files/concept.rs` `target_to_path` | One `files::confine` | behaviour |
| "Is h listed" asked in two places | messaging | `rust/backend/src/comm/cli/send.rs` `deliver`; `rust/backend/src/comm/mail/filer.rs` `file_here`; `rust/backend/src/comm/mail/hub_link.rs` `registry_handles` | One predicate | pure |
| "This computer's daemon" ensured twice, with the labels `sot` and `local` spelled at several sites; topology owns `local_daemon_label` | distribution | `scripts/lib/sot-daemon.sh` `sot_daemon_ensure`; `scripts/sot-local-daemon.ps1`; `scripts/launch-sot.ps1`; `scripts/restart-backend.sh`; `deploy/sotd.service`; `rust/protocol/src/topology/endpoint.rs` `local_daemon_label` | `sotd` defaults to the label; the scripts stop spelling it | behaviour |
| `install.json` written by an install and by a hand install | distribution | `rust/backend/src/install/record.rs` (`sotd install`, every system); `scripts/install-manifest.ps1` (a Windows hand install); `rust/updater/src/manifest.rs` `InstallManifest` | One writer, one schema | behaviour |
| Apply transaction in two languages | distribution | `scripts/sot-apply.sh`; `scripts/sot-apply.ps1` | One apply in Rust, one crash-loop rule | behaviour |
| Julia instantiate done three times | distribution | `rust/backend/src/install/julia.rs` `instantiate`; `scripts/sot-install-layout.ps1` `Initialize-InstallLayout` (the Windows launcher's first start); `rust/updater/src/prepare.rs` | The updater's prepare owns it on every target | behaviour |
| Codex home resolved two ways | agents | `rust/backend/src/agents/accounts.rs` `account_home`; `agents/codex/bin/ccx`; `src/homes.jl` `codex_home` | `accounts.rs` honours `$CODEX_HOME`; the shell and Julia copies stay as twins | behaviour |
| Which hosts run `sotd` decided twice | topology | `rust/protocol/src/topology/mod.rs` `sotd_hosts`; `rust/backend/src/install/role.rs` `from_status`; `runs_backend` (the same rule over the named host, a `frontend` entry taking the install's record) | The installer reads `sotd topology status`'s table, pinned to `sotd_hosts` by `install::role::tests::the_role_and_the_pin_agree_on_who_runs_sotd` | pure (a test) |
| `.sot/worktree.toml` `display_prefix` parsed twice, byte-identical | agents | `agents/worktree/comm-worktree-new.sh`; `agents/worktree/comm-worktree-clean.sh` | One shell function both call | pure |
| Build provenance read from git twice; the product version stamp is wire's, the lane build id capsule's | wire | `rust/protocol/build.rs`; `rust/log/build.rs` | Each concept keeps its own stamp; the shared git reads are one helper | pure |
| Folder trust written two ways | agents | `rust/backend/src/agents/folder_trust.rs` `ensure_folder_trusted`; `agents/codex/bin/ccx` | `ccx`'s append is deleted where the bypass flag answers the prompt | behaviour |
| Row creation and removal announced twice: ring entries and the live bus | rows | `rust/backend/src/rows/ops/create.rs`; `rust/backend/src/rows/ops/destroy.rs`; `rust/backend/src/rows/mod.rs` `WorkspaceChanged` | `workspace.changed` is the one announcement | behaviour |
| Skill copies | agents | `.claude/skills/sot-setup/SKILL.md`; `agents/claude/sot-setup/SKILL.md`; `.claude/skills/sot-statusline-setup/statusline.sh`; `agents/claude/sot-statusline-setup/statusline.sh` | `.claude/skills/` is canonical and `agents/claude/` its install payload; both stay, pinned byte-identical by `agents/tests/test-skill-copies.sh` | pure (a test) |
| cgroup kill written twice | rows | `rust/log/src/claude.rs`; `rust/backend/src/rows/spawn/row_scope.rs` `end` | `claude.rs` goes with the SDK producer's fate; until then its `Fence::discover` reads the root as the literal `/sys/fs/cgroup`, not through `row_scope_aim::v2_root`, so it refuses on a hybrid host | behaviour |
| Rust video suffix/MIME classification and its Julia twin | wire | `rust/protocol/src/video_path.rs` `video_mime`; `julia/plugins/video-file/src/ShipToolsVideoFile.jl` matches method | Rust consumers call video_mime; Rust and the unchanged Julia matches method execute the same suffix corpus, including leading-dot filenames | pure (remaining language twin) |
| `fe.sessions`: the window relays its computer's roster to other daemons | server | `rust/backend/src/clients.rs` `handle_fe_sessions`; `rust/frontend/src/net/transport/ops/workspace.rs` | Moves with the ruling on the hub link | behaviour |
