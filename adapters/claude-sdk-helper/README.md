# claude-sdk-helper

Ship's Log **P2** adapter (see `docs/adr/0037-ships-log-substrate.md`, "P2: the
first rich writer: Claude through its SDK"). This is a per-capsule Node
process, spawned by the Rust capsule with stdio pipes. It owns **one Claude
Agent SDK session** across many turns and speaks a small NDJSON protocol
(helper-protocol **1**) on stdin/stdout — one JSON object per `\n`-terminated
line, in each direction.

The capsule itself, and how its output becomes voyage frames on disk, is out
of scope here; that's `docs/adr/0039-voyage-frame-codec-and-segment-format.md`.
This package's only job is: turn stdin ops into SDK `query()` calls, and turn
SDK messages into stdout lines, faithfully and losslessly.

## How the capsule invokes it

```
HELPER_MODEL=<model-id-or-unset> node dist/src/main.js
```

stdin/stdout/stderr are pipes the capsule owns. The helper never reads
argv and never writes anything to stdout other than protocol lines (no logs,
no banners). `HELPER_MODEL`, if set, is forwarded as `model` on every
`query()` call; if unset, `model` is omitted and the CLI's own default model
applies.

## Protocol (helper-protocol 1)

NDJSON both directions, one JSON object per line.

### Out (helper → capsule)

| Line | When |
|---|---|
| `{"ev":"hello","protocol":1,"sdk_version":"<pinned>"}` | Always first, before anything else. |
| `{"ev":"msg","body":<projected SDK message>}` | Once per SDK message received during a turn. |
| `{"ev":"turn_end","query_id":<id>}` | The turn's query iterator exhausted normally with exactly one `result` message. |
| `{"ev":"interrupted","id":<id>,"ok":<bool>,"sdk_return":<value\|null>,"note":"adapter-derived"}` | After an `interrupt` op. `ok` only reflects whether `interrupt()` resolved without throwing — never SDK confirmation that anything was actually cancelled. |
| `{"ev":"fatal","reason":"<reason>","detail":<string>}` then `exit(1)` | Any protocol violation. `detail` is always a short, fixed, non-user-derived string (or omitted, for `reason:"protocol"`, matching the spec's own example) — never raw input bytes. |

Fatal `reason` values: `protocol`, `no_result`, `multi_result`,
`session_drift`, `query_error`, `busy`, `serializer`, `line_too_large`.

### In (capsule → helper)

| Line | Effect |
|---|---|
| `{"op":"user_turn","query_id":<id>,"text":"<prompt>"}` | Runs one SDK `query()` for this turn. |
| `{"op":"interrupt","id":<id>}` | Calls `interrupt()` on the in-flight query. |
| `{"op":"shutdown"}` | Clean `exit(0)`. |

An unknown `op` or a malformed input line ⇒ `{"ev":"fatal","reason":"protocol"}`
then `exit(1)`. stdin EOF (parent death) ⇒ abort any in-flight query and exit
within one event-loop turn, with no fatal line — this is the fast path, not
an error.

## Turn model

One SDK `query()` per `user_turn`, **not** streaming-input mode — every turn
is a single prompt string. The first turn is a fresh query; every later turn
adds `resume: <session_id>`, captured from the first turn's
`system`/`init` message. Every `query()` call repeats the same
authority-bearing options: `model` (from `HELPER_MODEL` if set),
`permissionMode: "bypassPermissions"`, `includePartialMessages: false`, no
`hooks`, no `canUseTool`.

A well-formed turn is exactly **one** top-level `result` message followed by
normal iterator exhaustion. Zero results, more than one result, a
`session_id` that drifts from the pinned/resumed session, or the iterator
itself rejecting, are each a distinct fatal (`no_result` / `multi_result` /
`session_drift` / `query_error`). A second `user_turn` while one is still in
flight is `busy` — the capsule is expected to enforce one-in-flight itself;
this is the helper's own double-check.

## Codec (protocol 1 projection)

Every value riding in `"body"` or `"sdk_return"` passes through
`project()` (`src/codec.ts`):

- `toJSON()` is applied once per node, if present.
- Only enumerable own properties are walked.
- `undefined`/function values are **omitted** from objects, become `null` in
  arrays; sparse array holes become `null` too.
- `bigint`, a non-finite number, a reference cycle, or anything that is
  still neither a JSON primitive, a plain object, nor an array after the
  `toJSON` step, throws — surfaced as `fatal reason:"serializer"`.
- Finite fractional numbers pass through as ordinary JSON numbers.
- Every outgoing line is capped at 8 MiB **after** `JSON.stringify` — over
  that, `fatal reason:"line_too_large"` instead of the line that triggered it.
- After writing each line, the helper awaits the stdout stream's backpressure
  (`drain`) before doing more work — this holds uniformly for every line
  emitted, a superset of the spec's minimum ("before reading the next SDK
  message"), applied for simplicity.

## Exact SDK version

`@anthropic-ai/claude-agent-sdk` is pinned **exact** (no `^`) at
**`0.3.241`** — the latest published version as of this package's creation.
`sdk_version` in the `hello` line is read from this package's own
`package.json` `dependencies` entry at runtime (not the installed SDK
package's own `package.json`, whose `exports` map does not expose that
subpath).

## Layout

```
src/codec.ts     pure projection (project(), no SDK import)
src/protocol.ts  pure NDJSON wire layer: op parsing, line encoding/decoding,
                 the line-size cap, stdin line buffering (no SDK import)
src/main.ts      wires the real SDK behind a tiny SdkLike interface and runs
                 the protocol loop; the only file that imports the SDK
test/*.test.ts   node:test suites — codec/protocol are exercised directly;
                 main.ts is exercised through run() with a fake SdkLike and
                 in-memory stdin/stdout streams, so no credentials are needed
```

## Regenerating

```
npm install   # resolves the pinned SDK + devDependencies; hits the network
npm run build # tsc -p tsconfig.json -> dist/
npm test      # build, then node --test over dist/test/*.test.js
npm start     # run the real helper against real stdio (HELPER_MODEL optional)
```

## Known spec-vs-SDK tension (flagged, not papered over)

The installed SDK's own `sdk.d.ts` documents `Query#interrupt()` (and its
sibling control-request methods) as "only supported when streaming
input/output is used." This package's turn model is mandated non-streaming
(a single prompt string per `query()` call, per the P2 adapter spec). The
helper still calls `.interrupt()` on the in-flight query exactly as
specified — that's the given contract — but whether the real CLI honors an
interrupt against a single-prompt query in practice is a live open question
this package does not resolve; it is inherited from the spec, not
introduced here.

Two more spec points needed a concrete choice where the wire spec was silent:

- **`allowDangerouslySkipPermissions: true`** is set alongside
  `permissionMode: "bypassPermissions"` on every `query()` call. The spec
  lists only `permissionMode`; the real SDK requires this companion flag for
  `bypassPermissions` to take effect at all (undocumented in the spec, but
  without it the mandated permission mode would not actually apply).
- **`interrupt` with no turn in flight** answers `{"ok":false,"sdk_return":null}`
  rather than being treated as a protocol violation — the spec describes
  `interrupt` acting "on the in-flight query" but doesn't say what happens
  when there is none. Since `ok` is defined as "whether `interrupt()`
  resolved without throwing," and nothing was invoked in this case, `false`
  is the most literal reading.
