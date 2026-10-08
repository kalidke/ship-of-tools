# rust/protocol/src/topology: how a process reaches a daemon (topology, charter)

hosts.toml declares which computers exist. It has one grammar and one writer, the hub's daemon, and every endpoint,
ssh login, link gate and relay unit is derived from it in one place. This folder is the protocol crate's half of that
subsystem; the backend's half (the `sotd topology` subcommands, `TopologyStore`, op `topology.set`, `sotd
stdio-bridge`) is built on it.

## Idea
Every Rust process names a daemon's endpoint, and starts an ssh login, in one way. A host name that is not
`[a-z0-9][a-z0-9._-]*` never reaches ssh, and this end runs no shell.

## Owns
- The hosts.toml grammar v2, its one search order (`locate`), `parse`, `serialize`, the edits (`TopologyEdit`,
  `apply`) and the endpoints derived from a parse (`plan`, `dial_endpoints`, `relay_endpoint`, `relay_socket_path`),
  in `mod.rs`.
- This box's own endpoint: the session socket or pipe path, the local daemon label, `slug`, `local_endpoint`
  (`$SOT_SOCKET` beats `$SOT_BACKEND_LABEL` beats the local label) and the plain host-name grammar
  (`is_plain_host_name`), in `endpoint.rs`.
- The ssh recipe (`SshRecipe`), its options `SSH_OPTS`, which turn ssh sharing off as the relay unit's do and with
  which the daemon's monitor also starts its ssh, and `LinkGate` (`spawn_sync`, `spawn_async`, `probe`, and `command`
  for a caller that contains the child), in `ssh_bridge.rs`.
- The lane dial `DaemonLaneEndpoint`, the attach worker's endpoint over op `lane.connect`, in `lane_client.rs`.
- The hub's systemd unit text for each relayed host, in `relay_units.rs`.
- Generated hub-relay path classification (relay_host_for_path in mod.rs), distinct from this computer's daemon endpoint.

## Promises
- `SshRecipe::new` checks both the ssh target and the host against the plain host-name grammar.
- Gate clones share one spawn-admission decision with `set_up(false)`: a gated child starts before close completes or is refused; after close returns no unadmitted gated child starts until reopen. Command preparation alone admits no spawn; the transport reconnect probe is the ungated exception (`LinkGate`).
- Every ssh started from `SSH_OPTS` turns sharing off (`ControlMaster=no`, `ControlPath=none`, `ControlPersist=no`):
  the bridges `SshRecipe` builds and the daemon's monitor sampler (`argv_has_no_shell_and_the_stated_option_set` pins
  the list).
- A production lane dial's local connect uses the platform connector's fixed `CONNECT_BOUND` retry budget; an attempt or
  wait in progress finishes first (Unix's 20 ms sleep, Windows's 200 ms wait). Its handshake has a separate bound and
  can be cancelled; refusals remain typed. Its child owner bounds teardown to 2 s, confirms reaping on success and
  reports termination, reap or deadline failures (`lane_child.rs`).
- A lane dial to a local socket or pipe goes through `sot_log::identity::connect_own::connect_own`, so it speaks only to an endpoint this OS account serves.
- A malformed hosts.toml is an error naming the line; an unknown key inside `[host.<name>]` is a warning, not fatal.
- On an SSH route an initial supervisor attempt may park at most one voyage login as an optional optimization; spare spawn failure leaves ordinary voyage fallback available. A local route parks none. The first voyage consumes a usable spare once or uses an ordinary gated dial; a spent endpoint starts no further spare, with pre-voyage abandonment governed by ADR 0045 (`start_spare`, `take_spare`).

- `DaemonLaneEndpoint::new` fixes the endpoint's route and initializes its private spare state. Callers cannot replace the route or construct an endpoint literal; a shared `LinkGate` changes liveness only. Tests exercise dial sequences and owned-child lifetimes.

- A failed supervisor handshake, unproven supervisor hello or failed Status abandons its parked spare. Spare state changes under its mutex and child teardown runs after that mutex is released; a teardown failure is reported, never counted as a confirmed reap.

- Production lane handshakes use `CONNECT_BOUND`; fixture bounds belong to the endpoint's test configuration under `cfg(any(test, feature = "test-handshake-bound"))` and affect only that endpoint (`with_test_handshake_bound`).

- A generated relay socket is remote even when reached locally; page and lane clients reuse its exact resolved path and do not acquire a window lease for its remote daemon.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: relay_host_for_path, `SshRecipe::new`,
`is_plain_host_name`, `LinkGate`, `SSH_OPTS`, `DaemonLaneEndpoint`, `SshRecipe`, `recipe_for`, `dial_and_call_tracked`,
`sotd topology plan|sync|status`, `sotd session-socket-path`, `launch-sot.sh`, `Get-SotTopologyPlan`,
`scripts/lib/sot-daemon.sh`, `sotd stdio-bridge`, `TopologyStore`, `topology.set`, `topology.changed`, `sot_ssh_bridge`,
`_sot_is_plain_host_name`, `comm/lib/comm-lib-client.sh`, `sot_slug`, `comm/lib/comm-lib-identity.sh`, `slug`. Uses:
`dispatch`, `sotd stdio-bridge`, `Signal::spawn_std`, `Signal::output`, `ContainedStd`, `Signal`,
`child_signal::process`, `lane.connect`, `Endpoint`, `DaemonLaneEndpoint`, `sot_state_dir`, `sot_config_dir`,
`host_name`, `state_dir_hash`, `boot_identity`, `process_created`, `IdentityExchange`.

## Folders
- `rust/protocol/src/topology/`: this folder.
- `rust/backend/src/topology/`: the daemon's half: the `sotd topology` verbs, `TopologyStore`, op `topology.set`, `sotd status`, `sotd stdio-bridge`, the hub's relay-unit apply and refresh.

## Files
- `mod.rs`: the hosts.toml parser, search rule, derivations, edits and status table.
- `endpoint.rs`: this box's own daemon endpoint, the label, the slug and the host-name grammar.
- `ssh_bridge.rs`: the ssh recipe, its argv and `LinkGate`.
- `lane_client.rs`: `DaemonLaneEndpoint`, the immutable daemon route over local, gated relay and SSH dials, the lane handshake and the first-voyage spare state (only an SSH route parks a spare)
- `lane_client_tests.rs`: wire, refusal, handshake and local-transport behavior against test-owned peers; the two cases
  with another account's listener (refusal, full backlog) need passwordless `sudo -n` and skip without it except on CI
- `lane_client_ownership_tests.rs`: counted endpoint fixtures and spare consumption, fallback, destruction and retry behavior
- `lane_child.rs`: the piped lane child, error diagnosis, cancellation and bounded teardown
- `lane_child_tests.rs`: owner-entry teardown and child-lock deadline witnesses, error reporting and observed exit/reaping
- `relay_units.rs`: the unit text `sotd topology apply` writes for each relayed host.
- `tests.rs`: the parser, search rule, edit and status-table tests.

## Start here
`mod.rs`'s module doc for the grammar; `ssh_bridge.rs` `SshRecipe::new` for any change to an ssh login;
`endpoint.rs` `local_endpoint` for how a process finds its own daemon.

## Rules
- The admission witness pauses one contested start, observes actual close-lock contention or completed close before release, and records every admitted child start against the owner's down transition; channel rendezvous use the existing hosted job hang guard, not a product timing claim. Both delayed-release teardown witnesses arm from the child owner's own start and deadline, never from fixture setup.
- `relay_units.rs` lives here, not in the backend, because the backend's relay_refresh test calls
  `relay_command_line()` and sot-backend has no library target.
- Read hosts.toml only through `locate`, `load` and `parse`; build an ssh login only through `SshRecipe`.
- The local endpoint is never the literal `sot`: on Windows the daemon's label is `local`.
- Over an ssh child, the lane dial returns a daemon's answer, a refused hello included, as the daemon gave it; the
  child's last stderr line is added once, after the error's own words, and only to a failure that is not the daemon's
  answer: by `diagnose` to the client's own write or read error, by `read_reply` to a reply that ended early, ran over
  the cap or did not parse, and by `run_handshake` to the bound.
- A test build can give an `SshRecipe` a fixture command (`with_test_command`, compiled only under `cfg(any(test, feature = "test-handshake-bound"))`): every start the recipe drives, the window's control probe, gated lane dials and the parked spare, then runs that program and argv in place of `ssh`, through the same `LinkGate` admission. An ordinary build has no fixture command.
