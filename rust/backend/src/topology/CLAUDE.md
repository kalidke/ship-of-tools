# rust/backend/src/topology: what this daemon derives from hosts.toml (topology)

The daemon's half of topology: it keeps the parsed declared topology for this box, applies edits to it on the hub, and
answers the two argv commands that read it, `sotd status` and `sotd stdio-bridge`, plus the one-shot blocking client
the CLI commands dial the daemon with. Part of topology.

## Files
- `dial.rs`: the one-shot blocking daemon client (and `forward_comm_file`).
- `mod.rs`: declares the five modules below.
- `set.rs`: `handle_topology_set`, the daemon side of op `topology.set`.
- `status.rs`: `sotd status`, declared plus live state fanned out to every reachable daemon.
- `stdio_bridge.rs`: `sotd stdio-bridge`, the byte shuttle between stdin/stdout and this box's own daemon endpoint.
- `store.rs`: `TopologyStore`, the daemon's cached view of hosts.toml, and `write_atomic`.

## Start here
`store.rs` `TopologyStore::refresh` for the daemon's view; `set.rs` `handle_topology_set` for the op; `status.rs` `run`
for `sotd status`; `stdio_bridge.rs` `run` for the bridge.

## Rules
- hosts.toml is parsed only by `sot_protocol::topology` (`TopologyStore::refresh` calls `topology::parse`).
- `TopologyStore::refresh` is the one re-read path: it compares mtime and size, never watches, and a malformed file
  keeps the last good parse.
- Only the daemon whose own file names it as hub applies `topology.set`; every other daemon refuses with `not_hub`,
  naming the hub (`handle_topology_set`).
- `sotd status` bounds each probe by `PROBE_TIMEOUT`.
- `sotd stdio-bridge` writes nothing of its own to stdout and exits 0 only on a clean EOF (`stdio_bridge::run`).
- `sotd status` and `sotd stdio-bridge` are answered in main's early argv match, before the umask, log or state dir.
