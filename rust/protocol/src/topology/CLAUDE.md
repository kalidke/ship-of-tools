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
- The ssh recipe (`SshRecipe`), its options and `LinkGate`, in `ssh_bridge.rs`.
- The lane dial `DaemonLaneEndpoint`, the attach worker's endpoint over op `lane.connect`, in `lane_client.rs`.
- The hub's systemd unit text for each relayed host, in `relay_units.rs`.

## Promises
- `SshRecipe::new` checks both the ssh target and the host against the plain host-name grammar.
- While a host's `LinkGate` is down, no gated spawn starts ssh.
- A lane dial's connect and handshake are each bounded (`CONNECT_BOUND`) and can be cancelled; refusals come back typed;
  no ssh child outlives its client.
- A lane dial to a local socket or pipe goes through `sot_log::identity::connect_own::connect_own`, so it speaks only to an endpoint this OS account serves.
- A malformed hosts.toml is an error naming the line; an unknown key inside `[host.<name>]` is a warning, not fatal.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `SshRecipe::new`,
`is_plain_host_name`, `LinkGate`, `DaemonLaneEndpoint`, `SshRecipe`, `recipe_for`, `dial_and_call_tracked`,
`sotd topology plan|sync|status`, `sotd session-socket-path`, `launch-sot.sh`, `Get-SotTopologyPlan`,
`scripts/lib/sot-daemon.sh`, `TopologyStore`, `topology.set`, `topology.changed`, `sot_ssh_bridge`,
`_sot_is_plain_host_name`, `comm/lib/comm-lib-client.sh`, `sot_slug`, `comm/lib/comm-lib-identity.sh`, `slug`. Uses:
`dispatch`, `sotd stdio-bridge`, `ChildGuard`, `Signal`, `child_signal::fired`, `child_signal::process`,
`lane.connect`, `Endpoint`, `DaemonLaneEndpoint`, `sot_state_dir`, `sot_config_dir`, `host_name`, `state_dir_hash`,
`boot_identity`, `process_created`, `IdentityExchange`.

## Folders
- `rust/protocol/src/topology/`: this folder.
- `rust/backend/src/topology/`: the daemon's half: the `sotd topology` verbs, `TopologyStore`, op `topology.set`, `sotd status`, `sotd stdio-bridge`, the hub's relay-unit apply and refresh.

## Files
- `mod.rs`: the hosts.toml parser, search rule, derivations, edits and status table.
- `endpoint.rs`: this box's own daemon endpoint, the label, the slug and the host-name grammar.
- `ssh_bridge.rs`: the ssh recipe, its argv and `LinkGate`.
- `lane_client.rs`: `DaemonLaneEndpoint`, the lane dial over ssh or a socket.
- `lane_client_tests.rs`: the lane dial's tests against a stub daemon.
- `relay_units.rs`: the unit text `sotd topology apply` writes for each relayed host.
- `tests.rs`: the parser, search rule, edit and status-table tests.

## Start here
`mod.rs`'s module doc for the grammar; `ssh_bridge.rs` `SshRecipe::new` for any change to an ssh login;
`endpoint.rs` `local_endpoint` for how a process finds its own daemon.

## Rules
- `relay_units.rs` lives here, not in the backend, because the backend's relay_refresh test calls
  `relay_command_line()` and sot-backend has no library target.
- Read hosts.toml only through `locate`, `load` and `parse`; build an ssh login only through `SshRecipe`.
- The local endpoint is never the literal `sot`: on Windows the daemon's label is `local`.
- Over an ssh child, the lane dial returns a daemon's answer, a refused hello included, as the daemon gave it;
  the child's last stderr line is added once, after the error's own words, and only to a failure in which the daemon
  sent nothing: by `diagnose` to the client's own write or read error, by `read_reply` to a read that ended before a
  frame, and by `run_handshake` to the bound.
