# Line Protocol

Ship of Tools is three processes that talk over sockets and stdio: the frontend, the
backend daemon, and the Julia kernel (plus a supervised REPL). Every seam
between them carries the same wire format — newline-delimited JSON for control,
length-prefixed binary frames for payloads. The format is deliberately
hand-debuggable: you can `tail -f` any seam and read what is happening.

This page describes the *application* protocol. The separate session-to-session
messaging used by Claude instances (sot-comm) is a different transport
entirely — see [The comm protocol](../ref/comm.md). For where these bytes flow
(Unix sockets, SSH forwarding, reconnect) see [Backend & Sessions](backend.md).

## Control frames — NDJSON

Each control message is one UTF-8 JSON object terminated by `\n`. The envelope
is fixed; the variable part is `op` plus its `payload`:

```json
{"v":1,"id":42,"kind":"req","op":"mode.preview","payload":{}}
```

| Field | Meaning |
|-------|---------|
| `v` | protocol version; a format bump is a one-line change |
| `id` | client-chosen `u64`; responses echo it, events use `0` |
| `kind` | `"req"` request, `"res"` response, `"evt"` event |
| `op` | the verb (see the op table) |
| `payload` | op-specific object; may declare a binary blob |

Requests carry a client-chosen `id`; the matching response echoes that `id`.
Events are unsolicited (file changed, a REPL frame arrived) and use `id:0`.

Shared Rust types live in `rust/protocol/` and are consumed by both the frontend
and the backend via `serde` (tagged on `op`). The kernel side uses `JSON3.jl`.
Because the envelope is versioned, an old client and a new backend can negotiate
on `v`.

### Binary blobs

Control frames stay small and printable; bytes ride alongside them. A payload
that carries binary content declares it inline:

```json
{"blob":{"len":18324,"mime":"image/png"}}
```

The next `len` bytes on the stream *are* the blob; after them the stream resumes
with the next JSON line. This avoids base64 — roughly a third of bandwidth and
CPU on every PNG — while keeping the control channel greppable.

### Phase 1 ops

The verbs the protocol defines in phase 1 (several later gained an additive
`workspace_id` field):

| Op | Direction | Purpose |
|----|-----------|---------|
| `mode.list` | req/res | enumerate available modes |
| `mode.tree.root` | req/res | root node of a mode's tree |
| `mode.tree.children` | req/res | children of a tree node |
| `mode.preview` | req/res | rendered preview for a node (may blob) |
| `repl.eval` | req/res | evaluate source in the REPL |
| `repl.interrupt` | req | interrupt a running eval |
| `fs.read` / `fs.write` | req/res | read/write a file |
| `concept.read` / `concept.write` | req/res | read/write a `.concept/` annotation |
| `chat.send` / `chat.cancel` | req | drive the orchestrator session |

Events: `tree.invalidate`, `repl.frame`, `chat.delta`, `file.changed`.

## REPL streaming frames

The REPL produces multi-modal output — stdout, stderr, return values with one or
more MIME representations, images (CairoMakie figures), and structured errors.
The display shim in `repl/src/DisplayShim.jl` emits **length-prefixed JSON
frames**; the backend wraps each into a `repl.frame` event on the main stream.

The shape borrows IJulia's `display_data`, flattened: no separate metadata
channel, every field at the top level of the frame.

| Frame | Fields |
|-------|--------|
| `stdout` | `{text}` |
| `stderr` | `{text}` |
| `value` | `{mime, text}` for textual MIMEs; `{mime:"image/png", blob}` for binary |
| `image` | `{mime:"image/png", blob}` — convenience for CairoMakie etc. |
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
are structured rather than dumped to stderr, so the frontend can render
stacktraces with `file:line` links.

## Why NDJSON

- **Debuggable by hand.** Any seam is readable with `tail -f`; this pays for
  itself the first time something hangs.
- **Cross-platform and low-ceremony.** One object per line, UTF-8, works
  identically on Windows and Linux.
- **Versioned.** The `v` field makes a format change explicit and one-line.
- **Cheap where it matters.** Length-prefixed blobs keep binary payloads off the
  JSON path; parse overhead on the control channel is negligible at
  human-interactive rates.
