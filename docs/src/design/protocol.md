# Line Protocol

!!! note "Status: partly built"
    The envelope, the blobs and the REPL frames below are built, as corrected here. The op set has grown since this
    page was written, and the ops are defined in `rust/protocol/src/ops/`, not in this page.

Ship of Tools talks over sockets and stdio between the window, the backend daemon and the daemon's Julia children
(a kernel and a REPL per row). Every seam carries newline-delimited JSON for control; the
window-daemon seam also carries length-prefixed binary blobs for payloads. The format is deliberately
hand-debuggable: you can `tail -f` any JSON seam and read what is happening.

This page describes the *application* protocol. The separate session-to-session
messaging used by Claude instances (sot-comm) is a different transport
entirely — see [The comm protocol](../ref/comm.md). For where these bytes flow
(Unix sockets, the ssh stdio bridge, reconnect) see [Backend & Sessions](backend.md).

## Control frames — NDJSON

Each control message is one UTF-8 JSON object terminated by `\n`. The envelope
is fixed; the variable part is `op` plus its `payload`:

```json
{"v":3,"id":42,"kind":"req","op":"preview.get","payload":{"node_id":"files:README.md"}}
```

| Field | Meaning |
|-------|---------|
| `v` | protocol version (`PROTOCOL_VERSION`, 3 between the window and the daemon); the daemon refuses a hello that speaks another. The kernel seam carries its own number (`KERNEL_PROTOCOL_VERSION`, 1) |
| `id` | client-chosen `u64`; responses echo it, events use `0` |
| `kind` | `"req"` request, `"res"` response, `"evt"` event |
| `op` | the verb (see Ops below) |
| `payload` | op-specific object; may declare a binary blob |
| `rev` | optional; the session revision the frame represents, set on replayed events |

Requests carry a client-chosen `id`; the matching response echoes that `id`.
Events are unsolicited (a file changed, a REPL frame arrived) and use `id:0`.

Shared Rust types live in `rust/protocol/`, consumed by both the frontend and the backend. The envelope is a
`Frame` whose payload stays a JSON value until a receiver routes on `op` and reads it into the op's `serde` struct.
The kernel side uses `JSON3.jl`. Every connection opens with a `hello` that carries `PROTOCOL_VERSION`, and the two
sides must agree on it.

### Binary blobs

On the window-daemon seam, control frames stay small and printable and bytes ride alongside them. A payload
that carries binary content declares it inline:

```json
{"blob":{"len":18324,"mime":"image/png"}}
```

The next `len` bytes on the stream *are* the blob; after them the stream resumes
with the next JSON line. This avoids base64 — roughly a third of bandwidth and
CPU on every PNG — while keeping the control channel greppable. The kernel and REPL seams have no blobs: the kernel's
`file.preview` reply carries the bytes as base64 in `blob_base64`, and the REPL's `image` frame carries `data_base64`.

### Ops

The op names are constants in `rust/protocol/src/ops/mod.rs`, and each family's payload types sit beside them in
`rust/protocol/src/ops/` (browse, repl, pty, workspace, agent, pages, monitor, update, lane, lease and session).
`docs/integration.md` says which subsystem serves them. Requests, responses and events are told apart by `kind`;
events (`preview.changed`, `repl.frame`, `workspace.changed`, `monitor.tick`, ...) use `id:0`.

## REPL streaming frames

The REPL produces multi-modal output — stdout, stderr, return values with one or
more MIME representations, images (CairoMakie figures), and structured errors.
The REPL shim in `julia/repl/src/ShipToolsRepl.jl` emits **newline-delimited
JSON frames**; the backend wraps each into a `repl.frame` event on the main stream.

The shape borrows IJulia's `display_data`, flattened: no separate metadata
channel, every field at the top level of the frame.

| Frame | Fields |
|-------|--------|
| `stdout` | `{text}` |
| `stderr` | `{text}` |
| `value` | `{mime, text}` |
| `image` | `{mime, data_base64, bytes}` — a showable binary result such as a CairoMakie figure |
| `browser` | `{url, open}` — a live page the eval serves (a WGLMakie figure) |
| `started` | `{run_id, origin, display}` — a run began (sent by the daemon) |
| `lifecycle` | `{state}` — the REPL child is `starting`, `ready` or `dead` (sent by the daemon) |
| `error` | `{message, stacktrace:[{file, line, fn}, …]}` |
| `done` | `{eval_id, elapsed_ms}` |

Two properties fall out of this design:

- **Ordering is preserved** because the REPL process has a single stdout writer;
  frames arrive in the order they were produced.
- **`done` is the completion signal.** The backend correlates it by `eval_id` to
  clear the UI spinner for that evaluation. CairoMakie figures route through
  `show(io, "image/png", x)` automatically, so plots arrive as `image` frames
  and render inline through the [preview layer](rendering.md) — never through a
  terminal graphics protocol.

The `mime` field keeps the frame schema open: adding HTML, LaTeX, or a custom
representation is a frontend-side render change, not a protocol change. Errors
are structured rather than dumped to stderr, so the window can print
each stack frame as `at fn (file:line)`.

## Why NDJSON

- **Debuggable by hand.** Any seam is readable with `tail -f`; this pays for
  itself the first time something hangs.
- **Cross-platform and low-ceremony.** One object per line, UTF-8, works
  identically on Windows and Linux.
- **Versioned.** The `v` field and the hello's version agreement make an incompatible change explicit.
- **Cheap where it matters.** Length-prefixed blobs keep binary payloads off the
  JSON path; parse overhead on the control channel is negligible at
  human-interactive rates.
