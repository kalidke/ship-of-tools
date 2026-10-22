# rust/topology: topology (contract, drafted)

hosts.toml declares which computers exist, and everything a process needs to reach one is derived from it in one place:
the endpoints, the ssh login, the link gate that ends logins on close, the relay and pin units (the exceptions are the
limits below: two launcher and installer inputs, the shell clients' copies, a literal path in `sync`, and the
no-argument bridge's endpoint). The library half is the
crate `sot-topology`; the daemon's half (`sotd topology`, `topology.set`, `sotd status`, `sotd same-version`, `sotd
stdio-bridge`, the dial) stays in the `sotd` binary under `rust/backend/src/topology/`, because it needs the roster,
rows and the lease record, which are not crates yet (pass 5c moves the op table into the binary and gives each service
a handler with an explicit context). The crate depends on `sot-protocol` (wire), `sot-platform` and `sot-log` (the
capsule's lane client trait and local connectors); `sot-protocol` depends on platform alone. The crate starts no child
the daemon owns: the daemon half of `sotd` depends on `sot-containment` and starts its ssh through it (guarantee 5), and
the starts the crate makes itself are the named exceptions there. Nothing re-exports it.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on and the limit it has. The Tests section holds the test
of each.

1. **hosts.toml has one grammar and one search order.** `locate` is `$SOT_HOSTS`, else `<config dir>/hosts.toml`
   (`sot_platform::host::state_dir::sot_config_dir`); `load` reads it and `parse` is the only reader of its text
   (`lib.rs`; the daemon's store, the monitor's roster, the CLI and the window call these, nothing else reads the file).
   Exactly one `hub`, which names a listed host with `daemon = true`; a section name is a plain host name; an unknown
   top-level key or section, a duplicate key and a malformed line are errors naming the line; an unknown key inside
   `[host.<name>]` is a warning collected on the `Topology`, not an error. A duplicate key is refused within a section,
   and a `[monitor]` label is refused when any earlier `[monitor]` header gave it. A monitor target (an empty one is
   its label) must not begin with `-` (ssh would read it as an option). Limits: the reader is a subset of TOML
   (strings, booleans, integers, comments), not the `toml` crate; a `[monitor]` target is otherwise any string, since
   it may be `user@host` or an alias; `serialize`, which every edit writes through, is a canonical rewrite, so a
   `topology.set` drops the file's comments and any unknown host key a hand edit added.
2. **Writers of hosts.toml exclude one another, and `topology.set` and `sync` never replace a newer version.** The
   hub's daemon applies an edit (`topology.set`) only where its own loaded file names itself the hub (`not_hub`
   otherwise), and holds `TopologyStore::edit_lock` from its read to its confirming re-read: an in-process mutex and a
   lock file beside hosts.toml (`hosts.toml.lock`, the platform's kernel lock, which fails closed after its bounded
   retry with `io`), so two edits, in this process or another, neither lose an update nor share a temporary file (each
   write goes to a temporary name of its own and is renamed). It re-validates `serialize`d text through `parse` before
   it writes and fires `topology.changed` once through the re-read a hand edit takes, also when it refuses the edit.
   On a box that is not the hub, `sotd topology sync` fetches the hub's file, and replaces the local copy under the
   same lock only if the copy still equals the text `sync` read before it fetched (`publish_synced`); a copy that
   changed meanwhile is kept and sync says so. Limits: a hand edit is a writer outside the lock the daemon only notices
   (at its next re-read, by size and modification time, never a watcher); the rename is not fsynced; whoever reaches
   the daemon's socket may send `topology.set` (the server's peer check and hello are the only gate); a file that is
   currently invalid is refused and the last good parse is kept; `sync` fetches the literal path
   `~/.config/sot/hosts.toml` on the hub, not the hub's own `locate` answer.
3. **A name that reaches ssh from the Rust tree is checked, and no shell runs on this end.** `SshRecipe::new` refuses a
   target or a host outside `[a-z0-9][a-z0-9._-]*` (so none begins with `-`); the argv is built, never a string a local
   shell parses, and the remote command is one argv element. `sotd topology sync --hub` takes a plain host name, a
   monitor target cannot begin with `-` (guarantee 1), `sotd stdio-bridge --host` takes a plain host name, and the
   Windows launcher checks `SOT_HOST_NAME` and `SOT_HOST` against the same grammar (`Test-SotPlainHostName`, run by
   the launcher suite `test-launcher-leases.ps1` in release-gate.yml). Limits: the far end's shell parses the one remote
   command string (the PATH prelude and `sotd stdio-bridge [--host <h>]`, with the host already checked); the
   installer's own `ssh <alias> true` takes the alias a person typed and is the installer's, an accepted exception; the
   shell clients' low-level bridge (`comm/lib/comm-lib-client.sh` `sot_ssh_bridge`) takes its arguments unchecked, its
   callers check them.
4. **Every ssh the Rust tree starts turns connection sharing off.** `SSH_OPTS` carries `ControlMaster=no`,
   `ControlPath=none`, `ControlPersist=no` (a command-line option wins over `~/.ssh/config`), so no master forks away
   from the tree its owner ends; `SshRecipe`'s bridges, `sotd topology sync`, the monitor's sampler and the installer's
   probe start from that list, and the hub's relay command line (`relay_command_line`) spells the same three. Limits:
   the shell client's bridge (`comm/lib/comm-lib-client.sh:237`) deliberately asks for `ControlMaster=auto` and
   `ControlPersist=600`, so the shell clients are outside this guarantee; the relay command line omits
   `ConnectTimeout`, and the relay service has no login bound of its own (systemd's `ServerAlive` pair ends a wedged
   network, nothing ends a login that is merely slow).
5. **A login the window or the lane client starts is admitted through the gate, or it is a named exception.**
   `LinkGate::spawn_sync` and `spawn_async` start nothing while the link is down; `set_up(false)` and these starts share
   one admission lock, so a start either completes (with its watch, guarantee 6) before a close returns or is refused,
   and after a close returns no such child starts until the gate is reopened. `LinkGate::command` only checks the flag
   and prepares a command: it reserves nothing, and a command prepared before a close can be started after it. The
   daemon half does exactly that, and starts the prepared command through `sot-containment` (`dial_and_call` with a
   gate of its own that is always up); `sotd topology sync` and the monitor's sampler build their ssh from `SSH_OPTS`
   and start it through containment without the gate. The starts outside containment are exceptions with their reasons
   in the clippy allows: the window's control probe (`LinkGate::probe`, ungated by design: it discovers that a link is
   back), the window's page proxy and the lane client's ssh (`spawn_sync`, `spawn_async`: they run in the window, which
   has no containment domain), and the installer's own probe. Limit: the `kill_on_drop` async child is ended by its
   drop, not by the gate.
6. **A close of the gate ends the login a dial is waiting on.** A login's start and its `LinkWatch` are taken under the
   one admission lock (`LinkGate::spawn_sync` and `spawn_async` return both; `watch` alone for a relay dial that
   starts no ssh here), so a close either finds the watch and ends the dial (`LinkDown`, the handshake cancelled
   through the stream's own cancel) or finds the link down and no login starts. A parked spare is registered from the
   moment it starts and one whose link closed since is never used (`take_spare`). Limits: registration is not
   cancellation: a parked spare is not ended by the close, it stays alive until it is used (and rejected) or its
   endpoint is dropped (queue); a login that does not honor its cancel still waits for its own bound.
7. **A lane dial is bounded, typed and its child's end is bounded.** A local dial goes through `connect_own` (only
   an endpoint this OS account serves) and its fixed `CONNECT_BOUND` retry budget, which an attempt in progress
   finishes; its handshake is bounded separately (2 s) and cancellable; on an ssh or relay route the handshake waits
   `SSH_LOGIN_BOUND`, the one constant for "the window waits for one login": the control connection, the page proxy
   and the lane dial all read it. `lane.connect` refusals stay typed (`Refused` ends, `Unreachable` and `Undetermined`
   retry in the caller). A daemon's own answer over ssh, a refusal included, is returned as given, and the child's last
   stderr line is added once and only to a failure that is not the daemon's answer. The piped child's teardown has a
   2 s budget, confirms reaping on success and reports a termination, reap or deadline failure (`lane_child.rs`).
   Limits: the 2 s teardown is a budget the code checks, not a kernel guarantee, and a process in uninterruptible
   sleep outlasts it; the handshake's bound holds only while the direct child's death closes its pipes: a
   cancellation kills the leader alone and the read is a blocking file read, so a descendant that keeps the child's
   stdout open holds the handshake past its bound (the class containment's `Signal::output` bounded; queue). The lane
   identity is the daemon's own report (`lane.connect`'s `pid` and `created`, bound only by the hello the dial runs over
   the stream); nothing here re-verifies it, so the dial trusts the daemon it reached.
8. **The first voyage's spare is an optimization that never changes an answer.** An ssh route's supervisor attempt
   parks at most one voyage login; a local route parks none; a spare spawn failure leaves the ordinary dial; the first
   voyage tries a live spare once and, when its handshake fails however it failed, makes one ordinary gated dial,
   whose result is the dial's; a failed supervisor handshake abandons the spare (`start_spare`, `take_spare`,
   `drop_spare`). Spare state changes under its mutex and teardown runs after the mutex is released.
9. **Endpoints are derived by one function each and never guessed.** `local_endpoint` is `$SOT_SOCKET`, else
   `$SOT_BACKEND_LABEL`'s session socket, else the local label's (`sot` on Unix, `local` on Windows, never a literal);
   `plan`, `dial_endpoints`, `relay_endpoint` and `relay_socket_path` give every other endpoint from a parse and the
   asking host, and `relay_host_for_path` accepts only exactly a generated relay path of a plain host name, so a
   window that reaches a relay socket locally still treats its daemon as remote. A path a unit or an ssh login names
   is derived on the box that owns it. Limits: `relay_socket_path` trusts its caller's host name (`sotd stdio-bridge
   --host` refuses a name outside the grammar before it derives a path); `sotd stdio-bridge` with no argument takes the
   local label's socket and ignores `$SOT_SOCKET` and `$SOT_BACKEND_LABEL` (queue); the scripts spell the labels at
   several sites (`docs/ownership.md`), which the Rust side cannot enforce.
10. **A unit sotd writes starts only where hosts.toml gives it.** `sotd.service` carries a `ConditionHost` pin of the
    hosts that run sotd (`daemon` or `frontend`, `sotd_hosts`), the reverse tunnels' template and each relay socket one
    of the hub; with no hosts.toml `sotd.service` is pinned to the host whose install wrote it (`install_pin`). A pin
    matches the kernel hostname exactly or followed by a dot and a domain. `apply` writes the pins first and stops
    before any enable if one cannot be written. Limits: a box whose hosts.toml name differs from its kernel hostname
    is not matched (`SOT_SELF_HOST` is not seen by systemd); the pins' effect under real systemd condition evaluation
    is tested in `rust/backend/tests/unit_pin.rs`; the generated relay socket units still carry an
    `[Install]` section, so `systemctl --user enable` and `disable` change a link in the home that every host shares,
    which the pin does not stop (the boot-start rule of `sotd.service` has not been applied to them; queue item 17).
11. **Every `systemctl` call topology makes is given a wait budget, and a stuck manager is a named failure.** Each call
    runs through `sot_containment::process().output` with `SYSTEMCTL_BOUND` (30 s) and fails `did not exit within`
    that bound; `apply` collects each unit's failure and runs the rest, `refresh` collects all but a failed reload and
    redoes every step at its next run, and the daemon's refresh at start runs on a thread nobody waits for. Limits:
    the bound is containment's budget with its limits (the child is created before its wait starts, and the timeout
    cleanup reaps without a wall-clock ceiling: `rust/containment/CLAUDE.md` guarantee 4), not a strict 30 s return; it
    is per call, and a complete CLI refresh makes `2 + 3N` calls for N enabled relay hosts (inventory, reload, and
    reset, show and restart per host; the start adds the MainPID probe), so a manager that answers the first and
    stalls the rest can cost minutes (a manager that answers none fails at the first call); `apply` is a dry run unless
    `--yes`. The reported relay-socket hang is not reproduced and its cause is not established.
12. **`sotd stdio-bridge` adds nothing to the stream.** It connects through `connect_own` to the local label's socket
    (no argument), to the hub's relay socket for a plain host name (`--host`) or to a local endpoint a caller on this
    box names (`--endpoint`), puts nothing of its own on stdout, copies bytes both ways unchanged until either side
    reaches EOF, and exits 0 on a clean end and 1 on a failure. Limits: it copies until EOF, not for a bound (its
    caller, ssh or a systemd unit, owns the lifetime); a failure usually prints one stderr line, but a host name that
    contains a newline prints several, and the two copy directions can each print a diagnosis; the no-argument form
    ignores `$SOT_SOCKET` and `$SOT_BACKEND_LABEL`; its integration suite is Unix-only.

## Owns
hosts.toml's grammar, search rule, edits and status table; this box's own endpoint, the label, `slug` and the plain
host-name grammar; the ssh recipe, `SSH_OPTS`, `SSH_LOGIN_BOUND`, `LinkGate` and `LinkWatch`; the lane dial
`DaemonLaneEndpoint` and its piped child; the unit text sotd generates (the hub's relay socket, service and command
drop-in per relayed host, the pins), the tunnel and relay unit names, and `apply_plan`'s convergence; in `sotd`:
`TopologyStore`, `topology.set` and `topology.changed`, `sotd topology`'s verbs, `sotd status`, `sotd same-version`,
`sotd stdio-bridge`, and the one-shot `dial_and_call`.

## Calls in
`sot_protocol::{op, Frame, Kind, HelloReq, HANDOFF_ROLE, LaneConnectReq, LaneConnectRes, codec, child_lines}` (wire);
`sot_platform::host::{state_dir, env, inherit_none}` and `sot_platform::identity::{challenge, peer, exchange}`;
`sot_log::lane::{client::Client, client::Endpoint, connect_own, socket_unix, pipe_win, transport}`; serde, tokio
(oneshot, process, time). In `sotd`: `sot_containment::{process, Signal, Contained, ContainedStd, guard_pid}` and the
server's roster and rows for `topology.set`.

## Calls out
The window (`rust/frontend`): `LinkGate`, `SshRecipe`, `DaemonLaneEndpoint`, `relay_host_for_path`,
`is_plain_host_name`, `local_endpoint`, `session_socket_path`, `slug`, `SSH_LOGIN_BOUND`; the daemon: `load`, `parse`,
`plan`, the unit text and `SshRecipe`; the monitor reads `Topology::monitor_targets` and `SSH_OPTS`; the installers
read `SSH_OPTS` and run `sotd topology pin|plan|sync` and `sotd same-version`.

## Does not
Hold a connection beyond one dial (the lane client keeps its streams and a parked login; fe-net and the daemon's link hold the long-lived ones), decide when anything ends (lifecycle), start a daemon child
outside containment except through the named exceptions, run a shell on this end, decide who may send `topology.set`
(server's admission), or interpret a daemon's replies beyond the lane handshake.

## Platforms
Same on all three except: `endpoint.rs` (a Windows named pipe `\\.\pipe\sot-<user>-<slug>` and the label `local`; a
Unix socket under the runtime directory and the label `sot`), `connect_own` (Unix: the listener's account; Windows: the
pipe server's account at identification level), the unit text, pins and `sotd topology apply`, `refresh` and the
refresh at start (Linux with a user manager; elsewhere they report no manager or do nothing), and `keep_no_descriptor`
(Unix only; Windows children inherit what std leaves inheritable, which is nothing).

## Tests
One per guarantee. Library paths are under `rust/topology/src/`; daemon-half paths under `rust/backend/src/topology/`.

| # | guarantee | test |
|---|---|---|
| 1 | one grammar and search order | `tests.rs`: `v2_parses`, `two_hubs_fail`, `hub_must_be_listed`, `v2_hub_must_be_a_daemon_host`, `unknown_top_level_key_still_errors`, `unknown_host_key_is_a_collected_warning_not_an_error`, `duplicate_keys_and_labels_fail`, `locate_honours_sot_hosts`, `a_monitor_target_that_reads_as_an_ssh_option_is_refused`, `a_monitor_label_repeated_under_a_second_header_is_refused` |
| 2 | one writer, serialized | `set_tests.rs`: `add_flag_remove_each_land_atomically_and_broadcast`, `non_hub_daemon_refuses`, `hand_edit_on_disk_is_picked_up_by_the_next_op_and_broadcasts_once`, `concurrent_edits_each_land`, `a_refused_edit_still_announces_the_hand_edit_it_noticed`; `cli_tests.rs`: `sync_replaces_only_the_version_it_read`, `sync_refuses_a_fetched_file_that_does_not_list_self`, `resolve_hub_prefers_the_local_copy_over_a_disagreeing_flag` |
| 3 | names checked before ssh | `ssh_bridge_tests.rs`: `recipe_rejects_a_leading_dash_in_either_half`, `recipe_rejects_grammar_outside_a_plain_host_name`, `argv_has_no_shell_and_the_stated_option_set`; `cli_tests.rs`: `sync_refuses_a_hub_that_is_not_a_plain_host_name`; the launcher: `scripts/tests/test-launcher-leases.ps1` (Windows PowerShell 5.1, release-gate.yml, not run by the landing job) |
| 4 | sharing off | `ssh_bridge_tests.rs`: `argv_has_no_shell_and_the_stated_option_set`; `relay_units_tests.rs`: `relay_ssh_turns_multiplexing_off_over_an_ssh_config_that_turns_it_on` |
| 5 | gated starts | `ssh_bridge_tests.rs`: `spawn_admission_and_link_close_are_one_decision`, `a_down_gate_spawns_no_child`, `a_down_gate_starts_no_fixture_command`; `dial_tests.rs`: `a_dropped_dial_takes_its_ssh_tree` (the daemon's start is contained); the exceptions' reasons are the clippy allows' |
| 6 | a close ends the watched login | `lane_client_tests.rs`: `an_ssh_login_in_flight_ends_when_its_link_closes`, `a_relay_login_in_flight_ends_when_its_link_closes`, `dial_across_a_link_close`; `lane_client_ownership_tests.rs`: `a_spare_parked_before_a_link_close_is_not_used_after_it`, `a_spare_handshake_in_flight_ends_when_its_link_closes`; `ssh_bridge_tests.rs`: `a_close_after_a_watched_start_ends_its_watch` |
| 7 | bounded, typed dial and teardown | `lane_client_tests.rs`: `refusal_codes_map_to_typed_errors`, `a_silent_daemon_is_unreachable_within_two_seconds`, `a_burst_of_ssh_logins_slower_than_the_local_bound_all_answer`, `a_daemons_answer_over_ssh_is_never_replaced_by_its_stderr`, `a_socket_another_account_listens_on_is_refused_by_the_lane_dial`, `a_full_foreign_backlog_ends_the_lane_dial_within_its_bound`; `lane_child_tests.rs`: `lane_child_teardown_finishes_within_its_bound`, `lane_child_teardown_reports_termination_reap_and_timeout_errors`; the one login constant holds by construction (the window reads `SSH_LOGIN_BOUND`) |
| 8 | spare | `lane_client_ownership_tests.rs`: `a_first_voyage_dial_takes_the_spare_the_supervisor_dial_started`, `a_second_supervisor_dial_starts_no_second_spare`, `a_dead_spare_uses_one_fresh_voyage_login`, `a_spare_that_dies_in_its_handshake_uses_one_fresh_voyage_login`, `endpoint_destruction_reaps_the_parked_spare` |
| 9 | endpoints derived once | `tests.rs`: `plan_lines_are_stable`, `relay_socket_path_is_a_sibling_of_the_relay_socket`, `only_the_complete_generated_path_is_a_hub_relay`, `a_planned_relay_dial_is_recognized_and_the_hubs_own_is_not`; `endpoint_tests.rs`: `session_socket_path_honours_xdg_runtime_dir` (the launcher's label has no behavior test here); `rust/backend/tests/stdio_bridge.rs`: `stdio_bridge_refuses_a_host_outside_the_grammar` |
| 10 | pins | `relay_units_tests.rs`: `a_pin_names_each_host_and_its_domain_forms_only`, `unit_names_and_pin_paths`, `relay_units_carry_the_host_and_the_declared_path`; backend `relay_units_tests.rs`: `refresh_pins_the_daemon_and_the_tunnels`; `rust/backend/tests/unit_pin.rs` (real systemd condition evaluation, pin-write refusal); the `[Install]` limit has no test |
| 11 | bounded `systemctl` | backend `relay_units_tests.rs`: `a_hung_systemctl_ends_at_its_bound_and_says_so`, `refresh_writes_and_reloads_in_order`, `refresh_after_a_failed_reload_reloads_and_restarts_again`, `the_relay_probe_takes_its_tree` |
| 12 | stdio-bridge | `rust/backend/tests/stdio_bridge.rs` (every test there) |

Windows- and macOS-specific arms run on the hosted runner of their OS (`endpoint_windows_tests.rs`, the Windows arms of
`dial_tests.rs`).

## Start here
`lib.rs`'s module doc for the grammar; `ssh_bridge.rs` `SshRecipe::new` for any change to an ssh login;
`endpoint.rs` `local_endpoint` for how a process finds its own daemon; `rust/backend/src/topology/relay_units.rs` for
what `apply` and `refresh` do to systemd.

## Rules
- Read hosts.toml only through `locate`, `load` and `parse`; build an ssh login only through `SshRecipe`.
- The local endpoint is never the literal `sot`: on Windows the daemon's label is `local`.
- `relay_units.rs` (the unit text) lives in the crate, not the binary, because the backend's `relay_refresh` test calls
  `relay_command_line()` and `sot-backend` has no library target.
- A test build can give an `SshRecipe` a fixture command (`with_test_command`, under `cfg(any(test, feature =
  "test-handshake-bound"))`): every start the recipe drives then runs that program in place of `ssh`, through the same
  `LinkGate` admission. An ordinary build has no fixture command; `sot-backend`'s dev-dependencies and the window's
  `test-pane-timing` feature turn `test-handshake-bound` on.
