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

## Promises
- An envelope is at most `MAX_ENVELOPE_BYTES` (1 MiB). `write_frame` and `write_frame_blocking` fail with
  `EnvelopeTooLarge` before writing any byte.
- A frame whose payload has `blob.len` is followed by exactly that many bytes; `read_frame` reads them, and the
  blocking pair never reads a blob tail.
- Facts of the code today, not promises: the async `read_frame` reads the whole line before its 1 MiB check, and it
  allocates the blob length the envelope declares with no cap. `read_frame_blocking` caps while it reads.
- A payload grows only by `#[serde(default)]` fields; any other change raises `PROTOCOL_VERSION`, and hello requires
  both sides to agree.
- The version string is bare `X.Y.Z` only for a CI build on its clean release tag. `is_release_build` is the only
  question policy may ask; the string is for people.

## Connections
- Called in by the window's control transport and the daemon's connection loop (`read_frame`, `write_frame`, the op
  payloads), by the daemon's topology and bridge code, and by every binary's `--version` (`version_line`).
- `loopback_port_from_url` is called by the window's page proxy (`ui/page_proxy.rs`, to arm a listener) and by the
  daemon's REPL supervisor (`sidecars/repl/supervisor.rs`, to record a `browser` frame's port for the proxy allowlist).
- It calls only sot-log (state directory derivation) at run time.

## Folders
- `src/ops/`: the op payloads (wire); its page lists the families.
- `src/topology/`: the topology subsystem, with its own charter; endpoints, ssh bridge, lane client and relay units.

## Files
- `Cargo.toml`: the crate manifest
- `build.rs`: stamps the git-derived build inputs of the product version
- `src/codec.rs`: the async and blocking frame readers and writers, and the envelope cap
- `src/ir.rs`: the wire's tree and preview payload types
- `src/lib.rs`: `Frame`, `Kind`, `PROTOCOL_VERSION` and the crate's re-exports
- `src/ops/`: the op names and payload types, one file per family
- `src/page_url.rs`: the loopback page-URL grammar, one parser for the daemon's proxy allowlist and the window's page proxy
- `src/topology/`: the topology, endpoint, ssh bridge and lane client modules
- `src/version.rs`: the product version string and `is_release_build`

## Start here
`src/codec.rs` for how bytes become frames; `src/ops/mod.rs` for an op's name; `src/version.rs` before any
self-update decision.
