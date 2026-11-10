# rust/protocol: wire (contract)

The wire is what the window, the daemon, the shell clients and the daemon's other-computer links speak: the frame, its
codec and size cap, every op's payload, the protocol integer and the product version. It does not own the capsule's
row frames (`rust/capsule/src/row/`: capsule's) or the sidecars' own protocols (the kernel's and the
REPL's JSON lines: sidecars'). The crate (`sot-protocol`) is shared by the window and the
daemon and owns no runtime state. It depends on platform only, for the host name and the OS account a hello
declares. Topology (hosts.toml, endpoints, the ssh recipe and the lane client) is the crate `sot-topology`, which
depends on this one; the payload of op `topology.set` is topology's (`sot_topology::TopologySetReq`), and only the op's
name is in this crate's one op table.

## Guarantees
1. **An envelope is at most 1 MiB in every reader and writer.** `MAX_ENVELOPE_BYTES`, its newline not counted;
   `write_frame` and `write_frame_blocking` fail with `EnvelopeTooLarge` before writing a byte; `read_frame` refuses an
   envelope one byte past the cap, never reading a line with no newline to its end; `read_frame_blocking` caps while
   it reads. Rests on: one constant and one codec; the blocking envelope writer is the blob writer with no blob, and the
   hand-decoded hello of the capsule's row socket (`sot-capsule`'s `RowHelloExchange`) keeps the same cap on its whole line.
2. **A blob is exactly its declared length.** A frame whose payload has `blob.len` is followed by exactly that many
   bytes; `write_frame` refuses a blob whose length differs from the declared one (or a blob the payload does not
   declare) before writing a byte; `read_frame` reads them and grows its buffer only with bytes that arrived, reserving
   at most 64 KiB up front (`BLOB_RESERVE_BOUND`), never the length the envelope declares; `read_envelope` reads the envelope alone, for a reader that must not read a blob it has not
   admitted; the envelope-only `read_frame_blocking` never reads a blob tail (an admission reads it first), and the pair
   `write_frame_blocking_blob` and `read_frame_blocking_blob` holds this guarantee for a blocking connection: the
   writer refuses a mismatch before a byte, the reader reads exactly the declared length and names a shortfall. A row
   reader judges a row by its hello's row number (the capsule's `ROW_PROTOCOL`), never by a frame's `v`.
3. **A payload grows only by defaulted fields; any other change raises `PROTOCOL_VERSION`, and hello refuses the
   mismatch.** Payload types are plain serde structs, so a client in another language needs only the op name and the
   JSON shape. The integer (4 since pass 6c-1a2, which changed what `lane.connect` pipes: its request is `{target, token}` and
   `DaemonVersion` lost its lane protocol integer, keeping an always-empty `lane_build` so an older reader still parses the
   answer, `an_older_readers_daemon_version_parses_a_new_answer`) is declared by every hello (`HelloReq::this_process`), by the shell client's literal
   (held to it by a test that runs it), and refused on mismatch by the daemon's hello, the server's check of this
   integer (`rust/backend/src/server/hello_protocol_gate_tests.rs`, where `hello_refuses_protocol_3` pins the last release's integer as refused, and `rust/backend/tests/admission.rs`). serde does not
   enforce the defaulted-field rule: it is a maintenance obligation, held by review and by previous-shape tests only for
   the hello and version query (`ops/session_tests.rs`, where the last release's `DaemonVersion` is frozen and the last release's whole `HelloRes` parses with `takes_drawing` absent), the lease
   (`ops/lease_wire_tests.rs`) and docs opens (`ops/pages_tests.rs`). `DocsOpenReq.direct` is an additive field defaulting
   to `ReplyScope::None`; an older frontend's payload without it grants no direct scope. Adding authority must never
   change that default; incompatible scope vocabulary changes follow this same protocol-number rule. The families
   with none are agent, browse, lane, monitor, pty, repl, update, workspace and the preview payloads (`ir.rs`), and the
   other page payloads have no previous-shape fixtures.
4. **A child's output is read by lines, lossily, so no byte ends a reader.** A line is the bytes up to `\n`, less a
   `\r` before it, decoded lossily, through `ChildLines` or `blocking_child_lines` only; `rust/clippy.toml`'s
   child-output group refuses tokio's and std's strict line readers. Each user keeps its own proof (the sidecars'
   children, the window's ssh stderr, the hub link, the dial, the lane client).
5. **The version string is for people; policy asks `is_release_build`.** (The stamp is a snapshot: `build.rs` reads git
   when the build script last ran, so a commit made after it and before the next build script run is not in it.) The string is bare `X.Y.Z` only for a CI
   build on its clean release tag; every other build carries `+src`, `+<sha>` or `+<sha>-dirty`; the predicate and
   the string never disagree. Rests on: `build.rs` stamping the inputs, one definition of each.
6. **The shared grammars have one parser.** The annotation header (two complete trimmed `---` fence lines, CRLF and
   indentation tolerated, an unclosed header has no hash, the body bytes kept); the physical scale (a string unit and
   a nonempty axes array of string names with finite positive `nm_per_px`); the loopback page URL; the video suffix
   classification (the Julia twin is exercised against the same corpus); page reply text (`page_reply::line` and
   `check_line`). Reply text trims ASCII spaces, is nonempty and at most `MAX_TEXT` (512) UTF-8 bytes, and refuses
   controls and the listed invisible formatting characters, naming the first code point. Every canonical line begins
   with the one `MARK`, `[sot-page] `. `check_line` accepts exactly a canonical marked line, without rewriting it. The chat text grammar (`chat::{text_segments, escape_text, post_text_fits}`): a bare `<` opens a token (`Markup`) closed by the next `>` or the end of the text, so a token a byte cut split is still markup; everything else is `Text`; then, per segment and in one pass, exactly `&lt;`, `&gt;` and `&amp;` are decoded and no segment is empty. `escape_text` is its inverse, so an escaped string parses to `Text` only and its segments join back to the original plain text, and `post_text_fits` holds a post's escaped form to `MAX_TEXT` (8 KiB).

7. **A frame write to a connection ends in an error rather than parking.** `reply::write_frame_to` fails after 10 s plus 1 s
   per MiB of blob (`write_frame_within` takes the bound), so a peer that stops draining cannot hold its writer; an
   over-cap envelope still fails as `EnvelopeTooLarge`, never as a timeout, so a caller can tell a refused frame from a
   dead peer. Limits: the bound is per frame, and a frame cut by the timeout is moot (the caller drops the connection);
   `reply::reject` writes one error frame with no deadline, as do the lease's malformed-line refusals that call it, so a
   peer that never reads can fill its buffer; the bounded writer is the daemon's control loop and its handlers'.
8. **The byte pipe ends with either side.** `pipe::pipe_bidirectional` copies both ways between a connection's halves
   and an upstream, half-closes the other side as each copy ends, and returns when either direction closes; it decodes
   no byte and owns no state past its call (it takes the halves by value and drops them at its return). It is here, not
   in the server, so the services that hand a connection off (pages' `proxy.connect`, rows' `lane.connect`) and the
   server take it from the wire without naming each other. Limit: it has no bound of its own (an idle pipe stays open
   until a side closes).
9. **A link names its channel in the payload of its own ops, so no reader misroutes it.** The window's link family
   (`ops/link.rs`; topology's `link` runs it): `link.hello {link}` -> `{link, build}` or `{error, code, link}`
   (`link_mismatch`, `unreachable`); `link.open {ch}` -> `{ch, ok}` or `{ch, error, code}` (`unreachable`,
   `too_many_channels`); `link.data {ch}` with a blob of at most `CHUNK` (64 KiB); `link.credit {ch, bytes}`;
   `link.close {ch, reason}` (`eof`, `refused`, `reset`). `LINK` (1) is the link's own number, raised only when the
   family changes; `WINDOW` (256 KiB) is the credit a channel's direction is granted at open and `MAX_CHANNELS` (128)
   the channels a link holds, both read by both ends. Mechanism: new op names with their own payloads; the envelope and
   every existing payload are unchanged, so guarantee 3 needs no raise. What an old reader does: the only reader that
   can be sent a link frame without knowing the family is a `sotd stdio-bridge` built before the link, which hands
   `link.hello` to its daemon; the daemon refuses a first frame that is not a hello (`unauthenticated`, the server's
   guarantee 4) and closes, and the window opens no channel before `link.hello` is answered, so no channel byte reaches
   an old reader. Limits: the family's previous-shape test is its own (`ops/link_tests.rs`); `v` on a link frame is the
   protocol integer as on every frame, and no link reader checks it (the link number does).
10. **A lost bus is told by one status evt, `bus.lost`.** `BusLostEvt { bus, skipped }` (`ops/bus.rs`): `bus` is the
    op the lost events travel as, `skipped` the count the daemon's receiver reports (defaulted 0). Mechanism: one new
    op in `op_table!` and a payload that grows only by defaulted fields; the envelope and every existing payload are
    unchanged, so guarantee 3 needs no raise. What an older window does: it reads an evt of an op it does not know as
    the generic `IncomingEvt::Event`, whose handler logs the op at debug and does nothing, so it keeps working against a
    newer daemon (`reply_tests::an_unknown_evt_op_reaches_the_generic_event_path`). Limit: a status only, no replay.
    Test `ops/bus_tests.rs`: `bus_lost_keeps_its_shape_and_a_missing_count_reads_as_zero`.

11. **One shared chat record, with a validated timestamp.** `chat::ChatRecord` (`chat.rs`): `workspace` and `channel`
    (part of a message's identity), `ts` and `thread_ts`, `author {id, name}`, `origin` (`member`, `drawer`, `row`,
    `bot`), `text` (Slack's markup as sent, read through the chat text grammar of guarantee 6), `reply_count`, `files` (`{id, name, mime, bytes, thumbnail}`, no URL), and the defaulted
    `truncated`, `latest_reply`, `limited`, `omitted` and `files_omitted`, left out of the JSON when false, zero or
    absent. `Ts` is the decimal string `<seconds>.<exactly six digits>`, never a float: `Ts::parse` and deserializing
    refuse any other number of fraction digits, because Slack answers such a bound with an empty page and no error.
    Mechanism: a new type and no new op, so every existing payload and `PROTOCOL_VERSION` (4) are unchanged. Bounds, enforced on decode since the chat family (guarantee 12): ids 64 bytes, text 8 KiB, author name 160, file name 256,
    MIME 128, eight files. `sot-chat` uses these constants. Limit: the 32 KiB total (`MAX_RECORD_BYTES`) is not checked on decode (the per-field
    bounds alone allow about 72 KiB); `sot-chat`'s normalization holds it, and a test pins that a page of 21 records of
    32 KiB and a cursor fit the envelope. What a cut field means (`truncated`) is `sot-chat`'s normalization; a record's unknown field is ignored (growth).
    Tests `chat_tests.rs`: `a_timestamp_has_exactly_six_fraction_digits`,
    `a_bad_timestamp_in_json_is_refused`, `the_record_wire_shape_is_stable`,
    `a_record_round_trips_and_an_older_shape_without_the_optional_fields_parses`, `a_record_is_bounded_at_decode`.

12. **Every chat payload is typed and bounded at decode, and every chat op is in the one table.** `ops/chat.rs`: requests
    `chat.resolve {row}`, `chat.watch {target}`, `chat.unwatch {target}`, `chat.history {target, thread?, after?, upper?, limit,
    cursor?}`, `chat.post {target, thread?, kind, name, text}` (`text` is plain text, what the person typed; chat's door will escape it, unbuilt until its own landing, so today the door sends the text as given), each with a reply, and the events `chat.changed` (puts and deletes in
    a watched channel) and `chat.status` (`service`, `gap`, and the channels that need a new snapshot). `target` is
    `{row, workspace, channel}`: the canonical row and the expected pair, rechecked by the daemon (resolve alone names the row
    only). A refusal is the standard error payload whose `code` is a `ChatCode` (`unavailable`, `unmapped`, `stale_row`,
    `stale_mapping`, `bad_request`, `too_many_watches`, `limited`, `busy`, `failed`; a later code reads as `other`), never
    Slack's or a library's text; a post's reply is `posted`, `refused`, `limited` or `unknown`. Bounds a decoder enforces:
    a channel id is 1 to 64 of letters, digits, `_`, `-`; a row 256 bytes, a workspace 64; a post's text 8 KiB and name 160; a
    cursor 1 KiB; `limit` 1 to 20; a page or a `chat.changed` list at most 21 and a status 32 channels (a list stops at its
    first excess item); `upper` is `before` or `until`, never both; every timestamp is a `Ts`. Every payload, a request
    included, ignores an unknown field, so guarantee 3's growth by defaulted fields holds both ways. Every enum a window reads
    reads a member added later safely: a `ChatCode` as `other`, a `service` as `other` (not running; resync every watch), a
    watch `state` as `other` (a snapshot is owed), a resolve `state` as `invalid`, a record's `origin` as `other` (never the
    app's own post, and one record never spoils its page), a post `outcome` as `unknown` (the post may exist; never resent).
    A request's `kind` is the other direction: an older daemon refuses a kind it does not know, with a `bad_request` code.
    A `limited` wait is clamped to an hour (`MAX_RETRY_SECS`). A `chat.post` reply with no `error` field that does not decode
    as `ChatPostRes` is read as `unknown`, never resent (an `error` reply is a refusal only with a refusal `ChatCode`, or as an older daemon's exact unknown-op answer, which fe-net reads as `Unsupported`; any other `error` reply, with another code or none, such as `handler_error` from a panic after the send began, is `unknown`, never refused and never resent):
    wire cannot hold that rule (a decode failure is the reader's), so it is fe-net's (`ops::chat::on_chat_post`, test `a_post_answered_by_a_handler_error_is_unknown_never_refused`). `ChatRefusal.code` is absent in an older daemon's unknown-op refusal (`{error}` alone). The seven names are in `op_table!`, so `op::ALL` and the
    admission fixture (refused `unauthenticated` and closed before hello) cover them. `bus.lost` stays the event bus's loss;
    `chat.status` is the service's and a watch's, not a second one. Mechanism: new ops and payloads only, so `PROTOCOL_VERSION`
    stays 4; an older daemon answers a chat op as any unknown op. Limits: the daemon serves all five requests (`chat/ops.rs`) and both events (`chat/watch.rs`); a `chat.watch` reply says `complete` when the owner already holds the channel's snapshot and `pending` otherwise, never `needed` (a channel that owes a snapshot is told by a `chat.status` entry, to which the window answers with `chat.watch`), and a window never resyncs on a reply; a duplicate JSON key in a frame's payload is not refused (the codec reads a
    `Value`, where the last wins); `chat.status` names the channels that need a snapshot, and nothing says a watch became
    complete: a window reads content by `chat.history` and `chat.changed`, so `complete` matters only to a second watch's reply; the watch count `MAX_WATCHED` is also `sot-chat`'s `watch::MAX_CHANNELS`, held equal by a test;
    a cursor is bounded here and bound to its query by `sot-chat`; `chat.fetch` and `chat.post_file` are B4's.
    Tests `ops/chat_tests.rs`: `the_chat_ops_are_declared_in_the_one_table`, `every_request_reply_and_event_keeps_its_shape`,
    `a_page_carries_records_that_round_trip`, `a_code_added_later_reads_as_other`,
    `an_older_daemons_unknown_op_refusal_reads_as_a_refusal_with_no_code`, `a_phase_or_outcome_added_later_reads_as_the_safe_one`,
    `a_wait_is_clamped_to_an_hour`, `a_state_added_later_reads_as_a_safe_one`, `a_record_with_an_origin_added_later_does_not_spoil_its_page`, `a_legal_page_fits_the_envelope`,
    `a_number_where_a_string_belongs_is_refused`, `a_timestamp_with_the_wrong_digits_is_refused_in_a_request`,
    `both_upper_bounds_cannot_be_asked_for`, `a_limit_is_one_to_twenty`, `a_cursor_a_text_and_a_name_stop_at_their_bounds`,
    `a_target_names_a_real_channel_and_bounded_ids`, `a_list_of_records_stops_at_a_page_and_an_event_at_its_bounds`.

13. **`selection.set` is one op in the table, its request bounded at decode, its digest defined once.** `ops/selection.rs`:
    `SelectionSetReq { workspace_id, selection, drawing }` -> `SelectionSetRes { state }` (`shared`, `held`; a later state reads as
    `other`, not shared), with `selection` a `SelectedText { node_id, start, end, len, digest }` or `null` to clear; the field
    is required, so a request that lost it is refused, never read as a clear. `TextPos { line, byte }` is a position in the shown file's text: a
    line and a UTF-8 byte offset, `end` exclusive. `whole_lines` is the one definition of a whole-line range, the form a
    rendered markdown preview shares (the window's source-line map calls it); in a file holding a lone CR, a range
    `whole_lines` can name answers `Some` and the daemon refuses it as `not_file_text`, while one the window's map
    numbers past the file's lines answers `None` and the share is cleared with no status line. `Some` is not an acceptance: the daemon still
    refuses a CR not before an LF, U+0085, U+2028 and U+2029 before the end, and reports a range ending on an empty line as
    `last - 1`. The text never travels. `selection_digest` (`sot_protocol::selection_digest`) is
    FNV-1a 64 over the UTF-8 bytes, lines joined by LF, the one definition the window and the daemon both call. The six
    refusal codes are the `selection_code` constants (`unknown_workspace`, `bad_node_id`, `not_file_text`, `no_agent`,
    `not_a_window`, `publish_failed`). Bounds a decoder enforces, `SelectedText` through `try_from` so no call shape skips
    them: `workspace_id` 256 bytes, `node_id` 4096, `start` before `end` by (line, byte), `len` 1 to 2 MiB
    (`MAX_SELECTION_LEN`), numbers typed (a string, a negative or a fractional number is refused). An unknown field is ignored
    (growth, as the chat family). Mechanism: one new op and new payloads, so `PROTOCOL_VERSION` stays 4; an older daemon
    answers it as any unknown op. Limits: a daemon built before the `drawing` field (one that has `selection.set`) ignores
    `drawing`, so it reads a drawing with `selection: null` as a clear and answers `shared`; the window must not send a
    drawing to a daemon that cannot take one. A daemon says it can in its hello reply: `HelloRes.takes_drawing`, defaulted and
    left out when false, so an older daemon reads as false (not the version string, guarantee 5; test
    `a_hello_reply_says_whether_the_daemon_takes_a_drawing_and_stays_additive`). The daemon's handler (`agents/selection.rs`) serves the op; a duplicate JSON
    key is not refused (the codec reads a `Value`, the last wins). The `drawing` field is defaulted (absent reads as none and
    is not written when none; a request without it decodes as before): a `Drawing { base, strokes }`, `base` `"canvas"` or
    `{"figure": {"node_id"}}` (4096 bytes), each point `[x, y]` as `u16` fractions of the base's width and height
    (`/ 65535`). Decode refuses a request with both a selection and a drawing, no stroke, an empty stroke, or more than
    `MAX_INK_POINTS` (16384) points in all; each stroke has a point, so that one bound also bounds the strokes, and the
    largest drawing is far under the envelope. Two more refusal codes, `not_an_image` and `too_large`, are fixed for the daemon's use. Tests `ops/selection_tests.rs`:
    `selection_set_round_trips_and_the_digest_is_fnv1a_64`, `a_missing_selection_is_refused_not_read_as_a_clear`,
    `a_drawing_rides_selection_set_within_its_bounds`, `ids_are_bounded_at_decode`, `a_range_starts_before_it_ends`, `len_is_one_to_two_mib`,
    `whole_lines_names_a_files_lines_as_the_daemon_reads_them`,
    `a_wrong_type_is_refused_and_an_added_field_is_ignored`, `the_refusal_codes_are_fixed`.

14. **A drawing has one raster.** `ink.rs`: `raster_dims` keeps the base's aspect with the long side at most 2000
    (`INK_RASTER_MAX`); `ink_radius` is `max(3, long side / 300) / 2` raster pixels; `paint_segment` gives each pixel the
    larger of what it holds and `r + 0.5` minus its center's distance to the segment, clamped to 0..1, as 0..255, visiting
    per row only the pixels within reach of the segment, so its cost follows the segment's own area (pinned by pixels
    visited, not time); `segments` yields every consecutive pair of every stroke, a one-point stroke as a dot, and `mask`
    paints them. Taking the maximum makes painting
    order-free, so the window painting segment by segment and the daemon painting the whole drawing get the same bytes within one release (a window and a daemon of different releases may draw the ink at different widths; the wire carries points only).
    `CANVAS_DIMS` (1600 x 1000) and `INK_RGB` are the canvas raster and the one ink color. Test `ink_tests.rs`:
    `one_raster_for_a_drawing_whatever_order_its_segments_are_painted`, `the_mask_is_the_distance_rule_on_every_pixel` (a brute-force reference), `a_segment_costs_its_own_area_not_its_bounding_box`. Measured on a 32-thread Linux workstation (release build, a 2000 x 2000 raster): a drawing at the wire's point bound takes about 5 s in `mask` as 16 random 1024-point strokes and about 13 s at worst, one 16,384-point stroke of full-diagonal segments, so the daemon paints it off the connection's loop. These were measured at the earlier 1/200 ink width; the 1/300 width visits about a quarter fewer pixels, so they are upper bounds.

## Owns
The frame, `Kind` and `PROTOCOL_VERSION` (`lib.rs`); the codec (`codec.rs`); child lines (`child_lines.rs`); the op
names, declared once in `op_table!` (`ops/mod.rs`), which also gives `op::ALL`, so a declared op joins the server's first-frame test with no list to keep; and every op's payload type (`ops/`, its page lists the families; the link family's constants `LINK`, `CHUNK`,
`WINDOW` and `MAX_CHANNELS` are in `ops/link.rs`); the chat record (`chat.rs`, with `TextSegment`) and the chat family (`ops/chat.rs`); the selection op, its payloads (the drawing too), `selection_digest` and `whole_lines` (`ops/selection.rs`); the drawing's raster (`ink.rs`); the tree and preview payloads and the binary
preview cap `PREVIEW_BINARY_CAP` (`ir.rs`); the version string and `is_release_build` (`version.rs`, `build.rs`); the
annotation, physical-scale, page-URL, video-suffix, page-reply and chat text grammars; the daemon's frame writes and byte pipe (`reply.rs`:
`HandlerOutput`, `write_frame_to`, `write_frame_within`, `reject`; `pipe.rs`: `pipe_bidirectional`), which services take from
here and never from the server. Owns no runtime state.

## Calls in
serde, serde_json, tokio (the async codec), tracing (the pipe's close log), and platform: `host_name` and `own_account_id` for a hello's identity.

## Calls out
`Frame`, `read_frame`, `read_envelope`, `write_frame` and their blocking forms, `ChildLines`, `blocking_child_lines`,
`PROTOCOL_VERSION`, `op::ALL`, `HelloReq`/`HelloRes` and every op payload (the link family's to topology's `link`),
`is_release_build`, `version_line`,
`annotation::{split_frontmatter, synced_against}`, `physical_scale::{PhysicalScale, parse_physical_scale}`,
`video_path::video_mime`, `loopback_port_from_url`, `with_loopback_port`, `page_reply::{MARK, MAX_TEXT, line, check_line}`, `chat::{TextSegment, text_segments, escape_text, post_text_fits}`; the shell twin `sot_hello_frame`
(`comm/lib/comm-lib-client.sh`) and `scripts/sot-lease.ps1` follow the lease and hello shapes and are owned by them.

## Does not
Decide policy from the version string (only `is_release_build`). Admit a connection (server's hello gate). Own a
connection's life (the pipe takes the halves it is given for the length of one call), a child or a socket. Interpret a payload's meaning beyond the grammars it owns (the op's handler does).

## Platforms
Same on all three; the blocking codec is the one the ssh bridge and shell wrappers use.

## Tests
| # | guarantee | test |
|---|---|---|
| 1 | envelope cap | `codec_tests.rs`: `an_envelope_at_the_cap_is_written_and_read_by_both_pairs`, `oversize_envelope_is_typed_and_writes_nothing`, `an_envelope_with_no_newline_is_refused_at_the_cap` |
| 2 | blob exact | `codec_tests.rs`: `file_chunk_blob_is_consumed_no_desync`, `a_blob_of_another_length_than_the_payload_declares_is_refused_before_any_byte_is_written`, `a_declared_blob_length_reserves_at_most_a_small_bound`, `a_truncated_frame_error_carries_no_frame_bytes` |
| 3 | defaulted growth, protocol integer | `ops/session_tests.rs`: `legacy_hello_req_defaults_to_preversioning`, `legacy_hello_res_defaults_to_preversioning`, `this_process_declares_the_protocol_the_version_and_the_os_account`; `lib_client_wire_tests.rs`: `the_shell_hello_is_this_protocols_hello`; `ops/lease_wire_tests.rs`: `fe_lease_res_defaults`; `ops/pages_tests.rs`: `docs_open_grows_by_a_defaulted_scope_and_old_payloads_grant_none`; the integer's refusal at hello is `rust/backend/src/server/hello_protocol_gate_tests.rs` and `rust/backend/tests/admission.rs` (server's) |
| 4 | lossy lines | `child_lines_tests.rs`: `every_byte_a_child_writes_reads_as_a_line` |
| 5 | release predicate | `version_app_version_tests.rs`: `the_bare_version_and_the_release_predicate_never_disagree`, `clean_on_tag_is_the_bare_version_only_from_ci` |
| 6 | grammars | `annotation_tests.rs`, `physical_scale_tests.rs`, `page_url_tests.rs`, `video_path_tests.rs`, `page_reply_tests.rs::hostile_text_is_refused_and_plain_text_is_marked`, `chat_tests.rs::slack_text_decodes_entities_once_delimited_and_escape_inverts_it` |
| 7 | a bounded frame write | `reply_tests.rs`: `write_frame_within_times_out_on_stuck_peer`, `write_timeout_is_not_confused_with_an_oversize_envelope`, `oversize_envelope_stays_downcastable_through_write_frame_within`, `write_frame_within_succeeds_on_healthy_peer`, `write_deadline_scales_with_blob_size` |
| 8 | the byte pipe | `pipe_tests.rs`: `bytes_cross_both_ways_and_the_pipe_ends_when_the_client_closes`, `the_pipe_ends_when_the_upstream_closes_though_the_client_stays_open` |
| 9 | the link family | `ops/link_tests.rs`: `link_payloads_keep_their_shape`; the window's reading of an old bridge's daemon refusal: topology's `link/tests.rs` `an_old_bridge_and_another_link_number_are_refusals_a_person_reads` |
| 11 | the chat record | `chat_tests.rs`: `a_timestamp_has_exactly_six_fraction_digits`, `a_bad_timestamp_in_json_is_refused`, `the_record_wire_shape_is_stable`, `a_record_round_trips_and_an_older_shape_without_the_optional_fields_parses`, `a_record_is_bounded_at_decode` |
| 12 | the chat family | `ops/chat_tests.rs` (the list under guarantee 12) |
| 13 | the selection op | `ops/selection_tests.rs` (the list under guarantee 13) |
| 14 | the drawing's raster | `ink_tests.rs` (the three tests under guarantee 14) |

## Files
- `Cargo.toml`: the crate manifest; depends on platform alone of ours
- `build.rs`: stamps the git-derived build inputs of the product version
- `src/annotation.rs`: split_frontmatter and synced_against, the shared annotation header grammar and its fixtures.
- `src/chat.rs`: `ChatRecord`, `ChatAuthor`, `ChatFile`, `Origin` and `Ts`, the one normalized chat record and its validated timestamp.
- `src/child_lines.rs`: `ChildLines` and `blocking_child_lines`, a child's output line by line, decoded lossily.
- `src/codec.rs`: the async and blocking frame readers and writers, and the envelope cap
- `src/ink.rs`: the drawing's one raster (guarantee 14): `raster_dims`, `ink_radius`, `paint_segment`, `segments`, `mask`.
- `src/ir.rs`: the wire's tree and preview payload types
- `src/lib.rs`: `Frame`, `Kind`, `PROTOCOL_VERSION` and the crate's re-exports
- `src/ops/`: the op names and payload types, one file per family; `ops/chat.rs` is the chat family (guarantee 12), `ops/selection.rs` the selection op (guarantee 13)
- `src/pipe.rs`: `pipe_bidirectional`, the raw byte pipe after a connect frame (the daemon's `proxy.connect` and `lane.connect`)
- `src/reply.rs`: `HandlerOutput`, the bounded frame write and the error frame for a refused connect
- `src/physical_scale.rs`: PhysicalScale, ScaleAxis and parse_physical_scale, shared by preview reads, writes and display.
- `src/page_reply.rs`: the page-reply mark, byte cap and one text grammar, shared by HTTP offers and confirmed
  delivery. `ops/pages.rs` holds `PageReplyReq { workspace_id, handle, line }` and `PageReplyRes { outcome, detail }`
  for the new `page.reply` op. Older daemons refuse the unknown op; older windows drop `page_reply` commands, so no
  protocol integer change is needed.
- `src/ops/pages.rs` also holds `ReplyScope`, tagged by `scope`: `none`, `row` with `workspace_id`, or `all`.
  `DocsOpenReq { path, direct }` defaults its scope to `none`; pages canonicalizes the requested row id or slug and
  attaches authority to one entry. HTTP `states.reply_scope` uses this same representation. The opener's field
  survives the frontend encoder; a missing field means no authority, including with an older frontend that drops it.
  `PageReplyReq.hold` is an additive field defaulting to false (the bar's request): with it, a busy row holds the
  line and the outcome is `sent`, `held`, `refused` or `unknown`.
- `src/page_url.rs`: the loopback page-URL grammar, one parser for the daemon's proxy allowlist and the window's page proxy, and the port rewrite the window's page proxy opens
- `src/video_path.rs`: video_mime, the five ASCII-insensitive dotted suffixes, and Rust consumer fixtures matching the executed Julia matches corpus.
- `src/version.rs`: the product version string and `is_release_build`

## Start here
`src/codec.rs` for how bytes become frames; `src/ops/mod.rs` for an op's name; `src/version.rs` before any
self-update decision.
