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
   the hello and version query (`ops/session_tests.rs`, where the last release's `DaemonVersion` is frozen) and the lease (`ops/lease_wire_tests.rs`). The families with
   none are agent, browse, lane, monitor, pages, pty, repl, update, workspace and the preview payloads (`ir.rs`).
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
   with the one `MARK`, `[sot-page] `. `check_line` accepts exactly a canonical marked line, without rewriting it.

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

## Owns
The frame, `Kind` and `PROTOCOL_VERSION` (`lib.rs`); the codec (`codec.rs`); child lines (`child_lines.rs`); the op
names and every op's payload type (`ops/`, its page lists the families); the tree and preview payloads and the binary
preview cap `PREVIEW_BINARY_CAP` (`ir.rs`); the version string and `is_release_build` (`version.rs`, `build.rs`); the
annotation, physical-scale, page-URL, video-suffix and page-reply grammars; the daemon's frame writes and byte pipe (`reply.rs`:
`HandlerOutput`, `write_frame_to`, `write_frame_within`, `reject`; `pipe.rs`: `pipe_bidirectional`), which services take from
here and never from the server. Owns no runtime state.

## Calls in
serde, serde_json, tokio (the async codec), tracing (the pipe's close log), and platform: `host_name` and `own_account_id` for a hello's identity.

## Calls out
`Frame`, `read_frame`, `read_envelope`, `write_frame` and their blocking forms, `ChildLines`, `blocking_child_lines`,
`PROTOCOL_VERSION`, `HelloReq`/`HelloRes` and every op payload, `is_release_build`, `version_line`,
`annotation::{split_frontmatter, synced_against}`, `physical_scale::{PhysicalScale, parse_physical_scale}`,
`video_path::video_mime`, `loopback_port_from_url`, `with_loopback_port`, `page_reply::{MARK, MAX_TEXT, line, check_line}`; the shell twin `sot_hello_frame`
(`comm/lib/comm-lib-client.sh`) and `scripts/sot-lease.ps1` follow the lease and hello shapes and are owned by them.

## Does not
Decide policy from the version string (only `is_release_build`). Admit a connection (server's hello gate). Own a
connection's life (the pipe takes the halves it is given for the length of one call), a child or a socket. Interpret a payload's meaning (the op's handler does).

## Platforms
Same on all three; the blocking codec is the one the ssh bridge and shell wrappers use.

## Tests
| # | guarantee | test |
|---|---|---|
| 1 | envelope cap | `codec_tests.rs`: `an_envelope_at_the_cap_is_written_and_read_by_both_pairs`, `oversize_envelope_is_typed_and_writes_nothing`, `an_envelope_with_no_newline_is_refused_at_the_cap` |
| 2 | blob exact | `codec_tests.rs`: `file_chunk_blob_is_consumed_no_desync`, `a_blob_of_another_length_than_the_payload_declares_is_refused_before_any_byte_is_written`, `a_declared_blob_length_reserves_at_most_a_small_bound`, `a_truncated_frame_error_carries_no_frame_bytes` |
| 3 | defaulted growth, protocol integer | `ops/session_tests.rs`: `legacy_hello_req_defaults_to_preversioning`, `legacy_hello_res_defaults_to_preversioning`, `this_process_declares_the_protocol_the_version_and_the_os_account`; `lib_client_wire_tests.rs`: `the_shell_hello_is_this_protocols_hello`; `ops/lease_wire_tests.rs`: `fe_lease_res_defaults`; the integer's refusal at hello is `rust/backend/src/server/hello_protocol_gate_tests.rs` and `rust/backend/tests/admission.rs` (server's) |
| 4 | lossy lines | `child_lines_tests.rs`: `every_byte_a_child_writes_reads_as_a_line` |
| 5 | release predicate | `version_app_version_tests.rs`: `the_bare_version_and_the_release_predicate_never_disagree`, `clean_on_tag_is_the_bare_version_only_from_ci` |
| 6 | grammars | `annotation_tests.rs`, `physical_scale_tests.rs`, `page_url_tests.rs`, `video_path_tests.rs`, `page_reply_tests.rs::hostile_text_is_refused_and_plain_text_is_marked` |
| 7 | a bounded frame write | `reply_tests.rs`: `write_frame_within_times_out_on_stuck_peer`, `write_timeout_is_not_confused_with_an_oversize_envelope`, `oversize_envelope_stays_downcastable_through_write_frame_within`, `write_frame_within_succeeds_on_healthy_peer`, `write_deadline_scales_with_blob_size` |
| 8 | the byte pipe | `pipe_tests.rs`: `bytes_cross_both_ways_and_the_pipe_ends_when_the_client_closes`, `the_pipe_ends_when_the_upstream_closes_though_the_client_stays_open` |

## Files
- `Cargo.toml`: the crate manifest; depends on platform alone of ours
- `build.rs`: stamps the git-derived build inputs of the product version
- `src/annotation.rs`: split_frontmatter and synced_against, the shared annotation header grammar and its fixtures.
- `src/child_lines.rs`: `ChildLines` and `blocking_child_lines`, a child's output line by line, decoded lossily.
- `src/codec.rs`: the async and blocking frame readers and writers, and the envelope cap
- `src/ir.rs`: the wire's tree and preview payload types
- `src/lib.rs`: `Frame`, `Kind`, `PROTOCOL_VERSION` and the crate's re-exports
- `src/ops/`: the op names and payload types, one file per family
- `src/pipe.rs`: `pipe_bidirectional`, the raw byte pipe after a connect frame (the daemon's `proxy.connect` and `lane.connect`)
- `src/reply.rs`: `HandlerOutput`, the bounded frame write and the error frame for a refused connect
- `src/physical_scale.rs`: PhysicalScale, ScaleAxis and parse_physical_scale, shared by preview reads, writes and display.
- `src/page_reply.rs`: the page-reply mark, byte cap and one text grammar, shared by HTTP offers and confirmed
  delivery. `ops/pages.rs` holds `PageReplyReq { workspace_id, handle, line }` and `PageReplyRes { outcome, detail }`
  for the new `page.reply` op. Older daemons refuse the unknown op; older windows drop `page_reply` commands, so no
  protocol integer change is needed.
- `src/page_url.rs`: the loopback page-URL grammar, one parser for the daemon's proxy allowlist and the window's page proxy, and the port rewrite the window's page proxy opens
- `src/video_path.rs`: video_mime, the five ASCII-insensitive dotted suffixes, and Rust consumer fixtures matching the executed Julia matches corpus.
- `src/version.rs`: the product version string and `is_release_build`

## Start here
`src/codec.rs` for how bytes become frames; `src/ops/mod.rs` for an op's name; `src/version.rs` before any
self-update decision.
