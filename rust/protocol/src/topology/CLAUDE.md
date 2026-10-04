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
- A malformed hosts.toml is an error naming the line; an unknown key inside `[host.<name>]` is a warning, not fatal.

## Connections
- In: the window's control transport is the only writer of `LinkGate`; the window's `parse_dial_arg` (which checks the host with `is_plain_host_name`) and the page
  proxy read endpoints and the gate; sot-log's attach worker drives `DaemonLaneEndpoint` on its own threads; the
  daemon's hub link and comm forward build logins with `SshRecipe`; `sotd topology
  plan|sync|status|apply|relay-endpoint|relay-sockets` call the parser, the derivations and `relay_units`.
- Out: the lane dial calls sot-log's client, challenge and transport (`Client`, `IdentityExchange`).
- Shell twins: comm-lib-client.sh's `sot_ssh_bridge` spells the same ssh child, its `_sot_is_plain_host_name` the grammar of
  `endpoint::is_plain_host_name`, and comm-lib-identity.sh's `sot_slug` the same slug; change each pair together.

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
