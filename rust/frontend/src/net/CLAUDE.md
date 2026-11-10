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
- A drawing sent only to a daemon that reads one: each connection keeps its own hello's `HelloRes::takes_drawing`
  (`SessionState`), and a `selection.set` that carries a drawing, on a connection whose hello did not say it, is answered
  `SelectionReply::Unsupported` and never written (an older daemon would read it as a clear); a clear and a text selection
  still go. Tests: `a_drawing_is_written_only_on_a_connection_whose_hello_took_one`,
  `the_accepted_hello_sets_whether_the_connection_takes_a_drawing_and_posts_plain_text`.
- A post sent only to a daemon whose door posts plain text: each connection keeps its own hello's
  `HelloRes::posts_plain_text` (`SessionState`), and a `chat.post`, on a connection whose hello did not say it, is answered
  `ChatEvt::PostNotSent` and never written (that daemon sends the text as given, so `<!channel>` would ping). Tests:
  `a_post_is_written_only_on_a_connection_whose_hello_posts_plain_text`, `the_accepted_hello_sets_whether_the_connection_takes_a_drawing_and_posts_plain_text`.
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
  An older daemon's generic unknown-op answer to an op (exactly `{"error": "unknown op: <op>"}`, no code, no other field;
  `ops::is_unsupported`, which names no op) ends that one request as outcome `unsupported` with the daemon's words, shown
  to the person as before, and the connection goes on serving; the same words for another op, extra fields or a success
  that does not parse stay outcome `error`. A future op declares itself the same way. The
  request is made only from the window's reply bar (fe-ui); the transport adds nothing to it and never retries it. Tests `ops::pages::tests::a_page_reply_answer_carries_its_handle_outcome_and_reason`,
  `ops::pages::tests::only_the_real_unknown_op_answer_is_unsupported`, `tests::an_unknown_op_reply_ends_only_that_request_as_unsupported`,
  `reply_wait_end_tests::a_page_reply_without_its_answer_is_unknown_when_the_connection_ends` and the golden
  `every_request_kind_and_its_error_reply_match_the_golden`.
- A request that must not run twice is written on one connection at most. Each `OutgoingReq` kind says whether it is
  replayable (`OutgoingReq::replayable`, one arm per kind); the requests wait in an `Outbox`, and `Outbox::retire` ends
  every waiting one that is not replayable when a connection ends (`run_protocol`) and when the next is about to send its
  hello (`run_session`), with the one outcome its kind already gives a connection that ended (`PendingGuard`: `page.reply`
  is `unknown`, an eval its error frame), so a request is never written on a later connection and never reports twice.
  Marked: `page.reply`, `repl.eval`, `repl.run_file`, `chat.post` (`unknown`), `selection.set` (`Ended`), `chat.watch` and `chat.history` (no event: the chat owner watches again on every `Connected`, and a replayed read could be taken as the new watch's snapshot; the drawer's own page or thread read (B5) is dropped the same way and must ask again on `Connected`). Limit: the other writes (`ConceptWrite`, `FileWrite`,
  `FileDelete`, `DirCreate`, `FileUpload`, `WorkspaceCreate`, `WorkspaceDestroy`, `ToggleHidden`, `AgentSend`,
  `ReplInterrupt`) stay replayable, because a connection's end gives their callers no outcome yet; a request sent during
  the few milliseconds of the next connection's hello and preamble is that connection's. Tests in transport/outbox_tests.rs:
  `a_reply_queued_between_connections_is_not_written_on_the_next`, `a_reply_cut_during_its_write_and_one_behind_it_are_not_written_again`,
  `an_accepted_reply_with_no_answer_is_unknown_once_and_not_written_again` (the real reconnect loop against a fixture
  daemon that counts the requests it receives) and `a_retirement_ends_the_unreplayable_and_keeps_the_rest_in_order`.
- A daemon `bus.lost` evt is decoded into the typed `IncomingEvt::BusLost { bus, skipped }` (a missing count reads as
  0), never the generic `Event`; the window's one arm logs it and gives a lost `chat.changed` to the chat owner, which watches again; no other pane recovers from loss yet. Tests
  `reply_tests::bus_lost_evt_decodes_into_its_typed_event`.
- The project chat is typed edges on the host's one control connection, so a remote host's chat costs no login and no
  channel (M7, asserted inside `a_remote_host_costs_one_login_for_control_pages_and_rows`): `OutgoingReq::{ChatResolve, ChatWatch, ChatUnwatch, ChatHistory, ChatPost}`
  and `IncomingEvt::Chat(ChatEvt)` (transport/ops/chat.rs). A reply carries the request it answers (the row, target,
  history query or post), and is `Done`, `Unsupported` (an older daemon's exact unknown-op answer, which ends only that
  request: `old_daemon_chat_refusal_preserves_other_requests`) or `Refused(code)` (`Other` for a code the window does not
  know). `chat.unwatch` is fire-and-forget. `chat.changed` and `chat.status` are decoded into `ChatEvt::Changed` and
  `ChatEvt::Status`; one that does not parse is a warning and a `ChatEvt::Unreadable`, a loss the owner resyncs from. A post is written on one connection at most
  (`ChatPost` is not replayable) and its outcome is `Done(Unknown)` unless the reply proves it was not made: an error
  reply whose code is not a refusal `ChatCode` (`handler_error`), an answer that does not parse and a connection that
  ends first are unknown, never refused, so the window never offers to resend (tests
  `a_post_answered_by_a_handler_error_is_unknown_never_refused`, `a_chat_post_without_its_answer_is_unknown_and_never_written_again` (which also pins the dropped watch and read),
  `a_chat_reply_carries_its_request_and_its_refusal`, `chat_events_decode_into_their_typed_events`, which pins `Unreadable`). Limits: fe-net does
  not order posts, does not resync after `bus.lost`, a `chat.status` or an `Unreadable`, and does not pace resyncs; a
  watch, history or resolve in flight when a connection ends gets no event. Those are the chat owner's (`ui/drawer/chat/`,
  fe-ui's contract), which `ui/events.rs` routes `IncomingEvt::Chat`, `BusLost` and each link's coming and going to.
- The row's selection is one typed edge, `OutgoingReq::SelectionSet { req }` (a text selection, a drawing or a clear) and
  `IncomingEvt::Selection { req, reply }` (transport/ops/selection.rs). A reply carries the request it answers, so a clear's
  reply differs from a set's, and is `Done(state)`, `Unsupported` (an older daemon's exact unknown-op answer), `Refused { code,
  error }` (also an answer that does not parse) or `Ended`. It is not replayable: a request waiting, or written and unanswered,
  when its connection ends yields one `Ended` and is never written on a later connection, because the one owner that knows the
  current selection, the window's selection share, sets it again after a reconnect (the drawing plan's orderings), and a stale
  replay could publish a selection the person withdrew. Tests `a_selection_reply_is_one_typed_event`,
  `a_selection_set_ends_once_when_its_connection_ends_and_is_never_replayed`; `ui/events.rs` drops the event until that owner exists.
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
`page.reply`, `selection.set`, `chat.resolve`, `chat.watch`, `chat.unwatch`, `chat.history`, `chat.post`, `chat.changed`, `chat.status`, `proxy.connect`, `ensure_proxy_for_url`, `pipe_one`.

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
