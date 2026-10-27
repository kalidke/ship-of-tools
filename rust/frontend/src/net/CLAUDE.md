# rust/frontend/src/net: the window's connections to daemons (fe-net charter)

## Idea
One long-lived connection per dialled host, retried forever; a remote host's is one login, its link, carrying the
control connection, the host's pages and its rows as channels. The UI sends typed requests on a host's channel and gets
typed events back from one fan-in; it never touches bytes. Part of the frontend; the crate's other subsystems call in
through the channel types below.

## Owns
- The connection set, from `--dial <host>=<endpoint>` and `--socket` only (`dial::parse_dial_arg`,
  `dial::resolve_connections`).
- One task per host on the one-worker `sot-transport` runtime that `main.rs` builds (`transport::spawn`, then
  `connect_and_run`, `run_protocol`, `run_session`, and `steady_loop` in transport/steady.rs): connect, hello, a 30 s ping, reconnect on
  platform's `Redial` (`redial_for`: from 200 ms, cap 5 s on a pipe, 30 s on ssh or a relay), and a remote host's link
  (`sot_topology::link::Link`, started in `connect_and_run`, dropped when its session ends).
- Request ids and reply matching (`PendingKind`); the typed edges `OutgoingReq` (UI to daemon) and `IncomingEvt`
  (daemon to UI).
- The reconnect memory, `session-<host>.json` (`state::state_path`, `load`, `save`), and the throttle that writes it
  (`StateSaveGate`, `SessionState`).
- The only writes of each remote host's `sot_topology::link::LinkHandle`: up at an accepted hello (`run_session`), down
  when the session ends (`run_protocol`).
- The window's identity (`identity::FrontendIdentity`, `identity::frontend_identity`) and the per-host helpers
  (`hosts::resolve_default_host`, `hosts::lane_dial`) live here; the per-host table is `hosts::HostTable`, held by the
  window's `State` as `hosts` and written from `ui/`.

## Promises
- Events carry the dial `HostKey`; the daemon's declared host is display only (`IncomingEvt` tagging in `transport`).
- One link per dialled remote host, owned by that host's task: the task starts the link (`Link::start` from the host's
  recipe or relay path), runs the control session over channel 0, puts the host's `LinkHandle` up at the accepted hello
  and down when the session ends; a refused hello leaves it down, and the link drops with the session. A far end that
  refuses the link itself (a bridge from before the link, another link number) is shown as a hello refusal is.
- Pages and rows open channels, never logins: the page proxy's `pipe_one` opens a channel on the host's handle and runs
  `pipe_one_over` over it; the attach endpoint's `LaneDial::Link` opens one per dial. A link that is down closes a
  browser connection at once and makes a lane dial fail `LinkDown`; one that ends mid-handshake or mid-page ends it
  the same way, whatever the page's copies wait on. A page's open waits at most `LINK_REPLY_BOUND`.
  Tests `a_remote_host_costs_one_login_for_control_pages_and_rows` (transport/tests.rs, one relay connection for
  control, two pages and three lane dials) and `pages::tests::a_down_link_closes_a_browser_connection_at_once`.
- The link's start waits `SSH_LOGIN_BOUND` (30 s) for its `link.hello` answer, and each reply on the connect path (the
  hello, then the preamble's tree.root and preview.get) arrives within the same 30 s, or the attempt ends with a
  disconnect that names the step (`read_connect_reply` in transport/hello.rs).
- No frame is half-read across a `select!`: reads go through one held future (`read_owned`).
- Every `figure.get` ends in exactly one result (`send_figure_get` records its `PendingKind` before it writes).
- A `page.reply` answer is about the handle the confirmed reply was sent for, never whatever the window shows by the
  time it lands: `send_page_reply` records `PendingKind::PageReply { handle }` before it writes, and `IncomingEvt::PageReplied`
  carries that handle with the daemon's `outcome` and `detail` (an error reply is outcome `error` with its words; an
  answer that does not parse is never `sent`). A connection that ends before the answer yields outcome `unknown` for that handle (`PendingGuard`), once. The
  request is made only from the window's reply bar (fe-ui); the transport adds nothing to it and never retries it. Tests `ops::pages::tests::a_page_reply_answer_carries_its_handle_outcome_and_reason`
  `reply_wait_end_tests::a_page_reply_without_its_answer_is_unknown_when_the_connection_ends` and the golden
  `every_request_kind_and_its_error_reply_match_the_golden`.
- A result-tree completion carries the canonical workspace id and result/attempt serials saved at issuance through the request-id pending entry; the existing fan-in envelope supplies its dial HostKey. These identities are local and change no daemon payload.
- A down ssh host costs at most two logins a minute, and a daemon that answers the hello and then drops costs no more:
  the wait starts over only after a session that lasted `STABLE`, or at F5 (`redial_for`, `Redial`). That is every
  login the window makes to the host.
- A burst of replies costs one reconnect-memory write per 2 s, and the last revision is flushed when the session ends
  (`StateSaveGate`, `SessionState`'s drop).
- A pipe host is leased before its data connection (`connect_and_run` calls `Leases::before_data_connection`); a
  sent lease request is never dropped, and one unanswered for `LEASE_REPLY_WAIT` (5 s) is named on the status line as a
  disconnect (`before_data_connection`'s `unanswered`).
- A blocked steady-state request or ping write never suspends the held reader; reply/event progress and pending cleanup remain on the connection task.
- A connection that has brought no byte from the daemon for three ping intervals (90 s) ends with the disconnect
  `nothing from the daemon in 90s`: the window's half of the daemon's own 90 s rule, which holds because the daemon
  runs no op inline that can outlast it (server charter). Bytes, not frames, keep it: a large reply still arriving is
  the daemon talking (`SILENT_PINGS`, `Heard` in transport/steady.rs); the reads after the outgoing side closed end at
  the same bound. The loop polls its sends before its read and the deadline last, so a flood of events cannot starve a
  request, and a frame already readable when the deadline passes is read, not cut.
- Unix parsing classifies generated hub-relay paths once; Relay remains distinct from Pipe through control, lease, page and lane selection: a relay host has a link, no lease.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `OutgoingReq`, `IncomingEvt`,
`HostTable`, `lane_dial`, `ResolvedDial`, Dial::Relay, ResolvedDial::Relay. Uses: sot_topology::relay_host_for_path, `Frame`, `codec::read_frame`, `codec::write_frame`, `hello`,
`PROTOCOL_VERSION`, `rust/protocol/src/ops/mod.rs`, `rust/protocol/src/ops/`, `version_line`, `--version`,
`SshRecipe::new`, `is_plain_host_name`, `Link`, `LinkHandle`, `LinkRoute`, `connect_local`, `Redial`, `STABLE`, `Leases::before_data_connection`, `sot_state_dir`,
`sot_config_dir`, `host_name`, `tree.root`, `tree.children`, `directory.list`, `nav.toggle_hidden`,
`preview.get`, `preview.set_scale`, `image.crop`, `concept.read`, `concept.write`, `concept.list`, `file.read`,
`file.write`, `file.delete`, `file.download`, `file.upload`, `dir.create`, `repl.eval`, `repl.run_file`,
`repl.interrupt`, `repl.execute`, `kernel.request`, `math.render`, `pluto.open`, `monitor.subscribe`,
`monitor.unsubscribe`, `monitor.history`, `repl.frame`, `monitor.tick`, `video.open`, `docs.open`, `quarto.open`,
`page.reply`, `proxy.connect`, `ensure_proxy_for_url`, `pipe_one`.

## Folders
- `transport/`: the per-host connection task: connect, hello, the steady loop, reply matching.
- `transport/ops/`: one file per op family under it: each op's request write and the event its reply becomes.

## Files
- `dial.rs`: the connection set from `--dial` and `--socket`.
- `hosts.rs`: per-host helpers (`resolve_default_host`, `lane_dial`), `HostTable`, and `spawn_transports`, which starts one
  transport task per host from `resumed`.
- `identity.rs`: this frontend's one declared identity and its `fe@<host>` address.
- `mod.rs`: declares the five parts, and in tests the in-process link fixture (`link_fixture_tests.rs`).
- `state.rs`: the persisted reconnect memory, one `session-<host>.json` per host, and the throttle that saves it.
- `transport/`: the control transport, one task per host, a remote host's over its link.

## Start here
`transport::spawn` for the life of one host's connection (then `connect_and_run`, `run_session`, `steady.rs` `steady_loop`);
`dial::resolve_connections` for which hosts are dialled.
