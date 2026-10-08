# rust/protocol: the wire contract and how a daemon is reached (wire, charter)

Every byte two Ship of Tools programs exchange is defined here, once, and so is the way a process names a daemon's
endpoint and starts an ssh login. The crate is shared by the frontend and the backend; its only dependency of ours is
sot-log. It holds no state, threads or files of its own.

## Idea
A frame is one `\n`-terminated JSON envelope, and may be followed by raw blob bytes. Payload types are plain serde
structs, so a client in another language needs only the op name and the JSON shape. The hosts.toml topology and the
ssh recipes live beside the wire because they are how a frame gets to the other end.

## Owns
- The frame: `Frame`, `Kind` and `PROTOCOL_VERSION` (lib.rs); the codec and its 1 MiB envelope cap (codec.rs).
- The op names and every op's payload type (`ops/`).
- The wire's tree and preview payload types, `TreeNode`, `PreviewPayload` and `BlobDescriptor` (ir.rs). The kernel
  builds the JSON these deserialize; nothing in the Julia core serializes its own types to them.
- The product version string and the release predicate (version.rs); build.rs stamps their inputs.
- The loopback page-URL grammar, owned by pages: `loopback_port_from_url` (page_url.rs).
- Reaching a daemon: the topology grammar, endpoints, ssh recipe and lane client (`topology/`).
- Annotation frontmatter and synced_against (src/annotation.rs), shared by the window and daemon.
- PhysicalScale's shared JSON validity and parse (src/physical_scale.rs).
- Rust video suffix classification and its MIME result (src/video_path.rs); the unchanged Julia plugin is exercised against the same suffix corpus by its real matches tests.

## Promises
- An envelope is at most `MAX_ENVELOPE_BYTES` (1 MiB), its newline not counted, in every writer and reader.
  `write_frame` and `write_frame_blocking` fail with `EnvelopeTooLarge` before writing any byte.
- A frame whose payload has `blob.len` is followed by exactly that many bytes; `read_frame` reads them, and the
  blocking pair never reads a blob tail.
- `read_frame` refuses an envelope one byte past the cap, never reading a line with no newline to its end, and grows
  a blob's buffer only with bytes that arrived, never by the length the envelope declares; `read_envelope` reads the
  envelope alone, for a reader that must not read a blob it has not admitted. `read_frame_blocking` caps while it reads.
- A payload grows only by `#[serde(default)]` fields; any other change raises `PROTOCOL_VERSION`, and hello requires
  both sides to agree.
- The version string is bare `X.Y.Z` only for a CI build on its clean release tag. `is_release_build` is the only
  question policy may ask; the string is for people.
- An annotation header has two complete trimmed --- fence lines; hash parsing handles CRLF and indentation and trims one matching quote pair. An unclosed header has no hash and splitting preserves body bytes.
- A physical scale has a string unit and a nonempty axes array of string names and finite, positive nm_per_px; empty string names/units retain their existing acceptance.

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `Frame`, `codec::read_frame`,
`codec::read_envelope`, `codec::write_frame`, `hello`, `PROTOCOL_VERSION`, `rust/protocol/src/ops/mod.rs`, `rust/protocol/src/ops/`,
`sot_hello_frame`, `comm/lib/comm-lib-client.sh`, `fe.lease`, `fe.leaving`, `scripts/sot-lease.ps1`,
`launcher_bounds_match_ops`, `scripts/tests/installer-state.sh`, `is_release_build`, `rust/backend/src/update.rs`,
`rust/frontend/src/selfupdate.rs`, `version_line`, `--version`, sot_protocol::annotation::split_frontmatter, sot_protocol::annotation::synced_against, sot_protocol::physical_scale::PhysicalScale, sot_protocol::physical_scale::parse_physical_scale, sot_protocol::video_path::video_mime. Uses: `sot_state_dir`, `sot_config_dir`, `host_name`,
`state_dir_hash`.

## Folders
- `src/ops/`: the op payloads (wire); its page lists the families.
- `src/topology/`: the topology subsystem, with its own charter; endpoints, ssh bridge, lane client and relay units.

## Files
- `Cargo.toml`: the crate manifest
- `build.rs`: stamps the git-derived build inputs of the product version
- `src/annotation.rs`: split_frontmatter and synced_against, the shared annotation header grammar and its fixtures.
- `src/codec.rs`: the async and blocking frame readers and writers, and the envelope cap
- `src/ir.rs`: the wire's tree and preview payload types
- `src/lib.rs`: `Frame`, `Kind`, `PROTOCOL_VERSION` and the crate's re-exports
- `src/ops/`: the op names and payload types, one file per family
- `src/physical_scale.rs`: PhysicalScale, ScaleAxis and parse_physical_scale, shared by preview reads, writes and display.
- `src/page_url.rs`: the loopback page-URL grammar, one parser for the daemon's proxy allowlist and the window's page proxy
- `src/video_path.rs`: video_mime, the five ASCII-insensitive dotted suffixes, and Rust consumer fixtures matching the executed Julia matches corpus.
- `src/topology/`: the topology, endpoint, ssh bridge and lane client modules
- `src/version.rs`: the product version string and `is_release_build`

## Start here
`src/codec.rs` for how bytes become frames; `src/ops/mod.rs` for an op's name; `src/version.rs` before any
self-update decision.
