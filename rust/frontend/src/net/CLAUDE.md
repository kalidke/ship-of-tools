# rust/frontend/src/net: the window's connections to daemons (fe-net charter)

## Idea
One long-lived connection per dialled host, retried forever. The UI sends typed requests on a host's channel and gets
typed events back from one fan-in; it never touches bytes. Part of the frontend; the crate's other subsystems call in
through the channel types below.

## Owns
- The connection set, from `--dial <host>=<endpoint>` and `--socket` only (`dial::parse_dial_arg`,
  `dial::resolve_connections`).
- One task per host on the one-worker `sot-transport` runtime that `main.rs` builds (`transport::spawn`, then
  `connect_and_run`, `run_protocol`, `run_session`, `steady_loop`): connect, hello, a 30 s ping, reconnect with a
  doubling wait (`next_backoff_ms`: cap 5 s on a pipe, 30 s on ssh), and an ssh host's control child.
- Request ids and reply matching (`PendingKind`); the typed edges `OutgoingReq` (UI to daemon) and `IncomingEvt`
  (daemon to UI).
- The reconnect memory, `session-<host>.json` (`state::state_path`, `load`, `save`), and the throttle that writes it
  (`StateSaveGate`, `SessionState`).
- The only writes of `sot_protocol::topology::ssh_bridge::LinkGate`: up at any hello reply (`read_hello`), down when the session
  ends (`run_protocol`).
- The window's identity (`identity::FrontendIdentity`, `identity::frontend_identity`) and the per-host helpers
  (`hosts::resolve_default_host`, `hosts::lane_dial`) live here; the per-host table is `hosts::HostTable`, held by the
  window's `State` as `hosts` and written from `ui/`.

## Promises
- Events carry the dial `HostKey`; the daemon's declared host is display only (`IncomingEvt` tagging in `transport`).
- A hello reply arrives within 30 s or the attempt ends (`HELLO_TIMEOUT`, `read_hello_reply` in transport/hello.rs).
- No frame is half-read across a `select!`: reads go through one held future (`read_owned`).
- Every `figure.get` ends in exactly one result (`send_figure_get` records its `PendingKind` before it writes).
- A down ssh host costs at most two logins a minute (`next_backoff_ms`).
- A burst of replies costs one reconnect-memory write per 2 s, and the last revision is flushed when the session ends
  (`StateSaveGate`, `SessionState`'s drop).
- A pipe host is leased before its data connection (`connect_and_run` calls `Leases::before_data_connection`).
- The gate goes down when the session ends, except after a hello refusal (`run_protocol`).

## Connections
- In: `main.rs` makes one `outgoing_channel` per host and hands the sender and the `(HostKey, IncomingEvt)` fan-in
  receiver to the window; the window's `resumed` calls `hosts::spawn_transports`, which calls `transport::spawn` once
  per host.
- Out: `Leases::before_data_connection` (`lease.rs`) before a pipe connection; `sot_protocol::topology::ssh_bridge` (`SshRecipe`,
  `LinkGate`) for ssh hosts; the wire codec in `sot_protocol` (`codec::read_frame`, frame writes).

## Folders
- `transport/`: the per-host connection task: connect, hello, the steady loop, reply matching.
- `transport/ops/`: one file per op family under it: each op's request write and the event its reply becomes.

## Files
- `dial.rs`: the connection set from `--dial` and `--socket`.
- `hosts.rs`: per-host helpers (`resolve_default_host`, `lane_dial`), `HostTable`, and `spawn_transports`, which starts one
  transport task per host from `resumed`.
- `identity.rs`: this frontend's one declared identity and its `fe@<host>` address.
- `mod.rs`: declares the five parts.
- `state.rs`: the persisted reconnect memory, one `session-<host>.json` per host, and the throttle that saves it.
- `transport/`: the control transport, one task per host.

## Start here
`transport::spawn` for the life of one host's connection (then `connect_and_run`, `run_session`, `steady_loop`);
`dial::resolve_connections` for which hosts are dialled.
