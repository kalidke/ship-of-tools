# rust/frontend/src/net: the window's connections to daemons (fe-net charter)

## Idea
One long-lived connection per dialled host, retried forever. The UI sends typed requests on a host's channel and gets
typed events back from one fan-in; it never touches bytes. Part of the frontend; the crate's other subsystems call in
through the channel types below.

## Owns
- The connection set, from `--dial <host>=<endpoint>` and `--socket` only (`dial::parse_dial_arg`,
  `dial::resolve_connections`).
- One task per host on the one-worker `sot-transport` runtime that `main.rs` builds (`transport::spawn`, then
  `connect_and_run`): connect, hello, a 30 s ping, reconnect with a doubling wait (`next_backoff_ms`: cap 5 s on a
  pipe, 30 s on ssh), and an ssh host's control child.
- Request ids and reply matching (`PendingKind`); the typed edges `OutgoingReq` (UI to daemon) and `IncomingEvt`
  (daemon to UI).
- The reconnect memory, `session-<host>.json` (`state::state_path`, `load`, `save`).
- The only writes of `sot_protocol::ssh_bridge::LinkGate`: up at any hello reply (`run_session`), down when the session
  ends (`run_protocol`).
- The window's identity (`FrontendIdentity`) and the per-host maps on `State` are also fe-net's, but they still live in
  the UI module; they move here later.

## Promises
- Events carry the dial `HostKey`; the daemon's declared host is display only (`IncomingEvt` tagging in `transport`).
- A hello reply arrives within 30 s or the attempt ends (`HELLO_TIMEOUT`, `run_session`).
- No frame is half-read across a `select!`: reads go through one held future (`read_owned`).
- Every `figure.get` ends in exactly one result (`send_figure_get` records its `PendingKind` before it writes).
- A down ssh host costs at most two logins a minute (`next_backoff_ms`).
- A pipe host is leased before its data connection (`connect_and_run` calls `Leases::before_data_connection`).
- The gate goes down when the session ends, except after a hello refusal (`run_protocol`).

## Connections
- In: `main.rs` makes one `outgoing_channel` per host and keeps the sender; it holds the `(HostKey, IncomingEvt)`
  fan-in receiver and calls `transport::spawn`.
- Out: `Leases::before_data_connection` (`lease.rs`) before a pipe connection; `sot_protocol::ssh_bridge` (`SshRecipe`,
  `LinkGate`) for ssh hosts; the wire codec in `sot_protocol` (`codec::read_frame`, frame writes).

## Folders
- `transport/`: the per-host connection task.

## Files
- `dial.rs`: the connection set from `--dial` and `--socket`.
- `mod.rs`: declares the three parts.
- `state.rs`: the persisted reconnect memory, one `session-<host>.json` per host.
- `transport/`: the control transport, one task per host.

## Start here
`transport::spawn` for the life of one host's connection; `dial::resolve_connections` for which hosts are dialled.
