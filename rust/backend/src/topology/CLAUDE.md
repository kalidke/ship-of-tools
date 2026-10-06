# rust/backend/src/topology: what this daemon derives from hosts.toml (topology)

The daemon's half of topology: it keeps the parsed declared topology for this box, applies edits to it on the hub, and
answers the two argv commands that read it, `sotd status` and `sotd stdio-bridge`, plus the one-shot blocking client
the CLI commands dial the daemon with. Part of topology.

## Files
- `cli.rs`: `sotd topology`, the verbs a box runs for itself: argv, `sync`, `set`, status's cache line and the edit parser.
- `dial.rs`: the one-shot blocking daemon client; its `connect` applies `connect_own`'s rule (`own_socket`, `own_pipe`) to a `unix:` or `pipe:` endpoint.
- `mod.rs`: declares the seven modules below.
- `relay_units.rs`: `sotd topology apply` and `refresh`, the hub's systemd --user relay units and drop-ins, and the hub's relay-refresh thread (`spawn_refresh_at_start`).
- `set.rs`: `handle_topology_set`, the daemon side of op `topology.set`.
- `status.rs`: `sotd status`, declared plus live state fanned out to every reachable daemon.
- `stdio_bridge.rs`: `sotd stdio-bridge`, the byte shuttle between stdin/stdout and this box's own daemon endpoint, a hub relay socket, or a local endpoint its caller names (`--endpoint`), reached through `connect_own`.
- `store.rs`: `TopologyStore`, the daemon's cached view of hosts.toml, and `write_atomic`.

## Start here
`cli.rs` `run` for a `sotd topology` verb; `relay_units.rs` `apply` and `refresh` for the hub's relay units;
`store.rs` `TopologyStore::refresh` for the daemon's view; `set.rs` `handle_topology_set` for the op; `status.rs` `run`
for `sotd status`; `stdio_bridge.rs` `run` for the bridge.

## Rules
- hosts.toml is parsed only by `sot_protocol::topology` (`TopologyStore::refresh` calls `topology::parse`).
- `TopologyStore::refresh` is the one re-read path: it compares mtime and size, never watches, and a malformed file
  keeps the last good parse.
- Only the daemon whose own file names it as hub applies `topology.set`; every other daemon refuses with `not_hub`,
  naming the hub (`handle_topology_set`).
- `sotd status` bounds each probe by `PROBE_TIMEOUT`.
- `sotd stdio-bridge` and `dial::connect` reach a local socket or pipe only after `sot_log::identity::connect_own`'s rule passes.
- `sotd stdio-bridge` writes nothing of its own to stdout and exits 0 only on a clean EOF (`stdio_bridge::run`).
- `sotd status` and `sotd stdio-bridge` are answered in main's early argv match, before the umask, log or state dir.
- `apply`, `relay-sockets` and `refresh` act only on the hub (`topology::relay_units::require_hub`).
- `apply` is a dry run unless given `--yes`, and one failing unit does not stop the others.
- `refresh_at_start` acts only when this process is sotd.service's MainPID (`supervised_by_systemd`), and the daemon
  never waits on it (main spawns it on its own thread).
- In `refresh`, only `daemon-reload` is fatal.
- The relay refresh's `systemctl` calls run through `Signal::output`, so what one leaves running dies with it.
- The dial's ssh runs through `Signal::spawn_std`: dropping its `ChildGuard` in dial.rs, or a `Track::cancel`, kills
  the ssh tree through its `ContainedStd` before the child is reaped.
