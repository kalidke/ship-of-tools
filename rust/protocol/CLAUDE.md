# rust/protocol: wire (contract)

Every byte two Ship of Tools programs exchange is defined here, once: the frame, its codec and size cap, every op's
payload, the protocol integer and the product version. The crate (`sot-protocol`) is shared by the window and the
daemon and owns no runtime state. Until pass 5b moves it out, `src/topology/` (hosts.toml, endpoints, the ssh recipe
and the lane client) lives beside the wire; it is topology's contract (`src/topology/CLAUDE.md`), not this page's, and
it is the only part of this crate that depends on sot-log. The wire's own modules depend on platform only, for the
host name and the OS account a hello declares.

## Guarantees
1. **An envelope is at most 1 MiB in every reader and writer.** `MAX_ENVELOPE_BYTES`, its newline not counted;
   `write_frame` and `write_frame_blocking` fail with `EnvelopeTooLarge` before writing a byte; `read_frame` refuses an
   envelope one byte past the cap, never reading a line with no newline to its end; `read_frame_blocking` caps while
   it reads. Rests on: one constant and one codec, no second reader.
2. **A blob is exactly its declared length.** A frame whose payload has `blob.len` is followed by exactly that many
   bytes; `read_frame` reads them and grows its buffer only with bytes that arrived, never by the length the
   envelope declares; `read_envelope` reads the envelope alone, for a reader that must not read a blob it has not
   admitted; the blocking pair never reads a blob tail.
3. **A payload grows only by defaulted fields; any other change raises `PROTOCOL_VERSION`, and hello refuses the
   mismatch.** Payload types are plain serde structs, so a client in another language needs only the op name and the
   JSON shape. The integer (3) is declared by every hello (`HelloReq::this_process`), by the shell client's literal
   (held to it by a test that runs it), and gated by the daemon's hello (server's guarantee). Rests on: `#[serde(default)]`
   on every field added since protocol 3, and the tests that parse the previous shape.
4. **A child's output is read by lines, lossily, so no byte ends a reader.** A line is the bytes up to `\n`, less a
   `\r` before it, decoded lossily, through `ChildLines` or `blocking_child_lines` only; `rust/clippy.toml`'s
   child-output group refuses tokio's and std's strict line readers. Each user keeps its own proof (the sidecars'
   children, the window's ssh stderr, the hub link, the dial, the lane client).
5. **The version string is for people; policy asks `is_release_build`.** The string is bare `X.Y.Z` only for a CI
   build on its clean release tag; every other build carries `+src`, `+<sha>` or `+<sha>-dirty`; the predicate and
   the string never disagree. Rests on: `build.rs` stamping the inputs, one definition of each.
6. **The shared grammars have one parser.** The annotation header (two complete trimmed `---` fence lines, CRLF and
   indentation tolerated, an unclosed header has no hash, the body bytes kept); the physical scale (a string unit and
   a nonempty axes array of string names with finite positive `nm_per_px`); the loopback page URL; the video suffix
   classification (the Julia twin is exercised against the same corpus).

## Owns
The frame, `Kind` and `PROTOCOL_VERSION` (`lib.rs`); the codec (`codec.rs`); child lines (`child_lines.rs`); the op
names and every op's payload type (`ops/`, its page lists the families); the tree and preview payloads and the binary
preview cap `PREVIEW_BINARY_CAP` (`ir.rs`); the version string and `is_release_build` (`version.rs`, `build.rs`); the
annotation, physical-scale, page-URL and video-suffix grammars. Owns no runtime state.

## Calls in
serde, serde_json, tokio (the async codec), and platform: `host_name` and `own_account_id` for a hello's identity.

## Calls out
`Frame`, `read_frame`, `read_envelope`, `write_frame` and their blocking forms, `ChildLines`, `blocking_child_lines`,
`PROTOCOL_VERSION`, `HelloReq`/`HelloRes` and every op payload, `is_release_build`, `version_line`,
`annotation::{split_frontmatter, synced_against}`, `physical_scale::{PhysicalScale, parse_physical_scale}`,
`video_path::video_mime`, `loopback_port_from_url`, `with_loopback_port`; the shell twin `sot_hello_frame`
(`comm/lib/comm-lib-client.sh`) and `scripts/sot-lease.ps1` follow the lease and hello shapes and are owned by them.

## Does not
Decide policy from the version string (only `is_release_build`). Admit a connection (server's hello gate). Hold a
connection, a child or a socket. Interpret a payload's meaning (the op's handler does).

## Platforms
Same on all three; the blocking codec is the one the ssh bridge and shell wrappers use.

## Tests
| # | guarantee | test |
|---|---|---|
| 1 | envelope cap | `codec_tests.rs`: `an_envelope_at_the_cap_is_written_and_read_by_both_pairs`, `oversize_envelope_is_typed_and_writes_nothing`, `an_envelope_with_no_newline_is_refused_at_the_cap` |
| 2 | blob exact | `codec_tests.rs`: `file_chunk_blob_is_consumed_no_desync`, `a_declared_blob_length_reserves_nothing`, `a_truncated_frame_error_carries_no_frame_bytes` |
| 3 | defaulted growth, protocol integer | `ops/session_tests.rs`: `legacy_hello_req_defaults_to_preversioning`, `legacy_hello_res_defaults_to_preversioning`, `this_process_declares_the_protocol_the_version_and_the_os_account`; `lib_client_wire_tests.rs`: `the_shell_hello_is_this_protocols_hello`; `ops/lease_wire_tests.rs`: `fe_lease_res_defaults`; the hello gate itself is server's |
| 4 | lossy lines | `child_lines_tests.rs`: `every_byte_a_child_writes_reads_as_a_line` |
| 5 | release predicate | `version_app_version_tests.rs`: `the_bare_version_and_the_release_predicate_never_disagree`, `clean_on_tag_is_the_bare_version_only_from_ci` |
| 6 | grammars | `annotation_tests.rs`, `physical_scale_tests.rs`, `page_url_tests.rs`, `video_path_tests.rs` |

## Files
- `Cargo.toml`: the crate manifest; `test-handshake-bound` is enabled only by test-fixture features (the backend's dev-dependencies and the frontend's `test-pane-timing`), for the lane client's handshake bound and the ssh recipe's fixture command
- `build.rs`: stamps the git-derived build inputs of the product version
- `src/annotation.rs`: split_frontmatter and synced_against, the shared annotation header grammar and its fixtures.
- `src/child_lines.rs`: `ChildLines` and `blocking_child_lines`, a child's output line by line, decoded lossily.
- `src/codec.rs`: the async and blocking frame readers and writers, and the envelope cap
- `src/ir.rs`: the wire's tree and preview payload types
- `src/lib.rs`: `Frame`, `Kind`, `PROTOCOL_VERSION` and the crate's re-exports
- `src/ops/`: the op names and payload types, one file per family
- `src/physical_scale.rs`: PhysicalScale, ScaleAxis and parse_physical_scale, shared by preview reads, writes and display.
- `src/page_url.rs`: the loopback page-URL grammar, one parser for the daemon's proxy allowlist and the window's page proxy, and the port rewrite the window's page proxy opens
- `src/video_path.rs`: video_mime, the five ASCII-insensitive dotted suffixes, and Rust consumer fixtures matching the executed Julia matches corpus.
- `src/topology/`: the topology, endpoint, ssh bridge and lane client modules
- `src/version.rs`: the product version string and `is_release_build`

## Start here
`src/codec.rs` for how bytes become frames; `src/ops/mod.rs` for an op's name; `src/version.rs` before any
self-update decision.
