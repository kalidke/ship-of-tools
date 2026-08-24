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

One SDK `query()` per `user_turn`, in **streaming-input mode**: `prompt` is a
one-message `AsyncIterable<SDKUserMessage>` (`oneShotPrompt()` in
`src/main.ts`), not a plain string. The generator yields exactly one message
and returns — one input in, one result out, same turn boundary a
single-prompt string would have given, but over the streaming-input
transport. The message is the minimal `SDKUserMessage` shape from the
installed SDK's `sdk.d.ts` (only `type`, `message`, and `parent_tool_use_id`
are required; every other field is optional and omitted):

```ts
{
  type: "user",
  message: { role: "user", content: op.text },
  parent_tool_use_id: null,
}
```

**Why streaming-input instead of a plain string:** the SDK's own doc
comment on `Query#interrupt()` (and its sibling control-request methods)
says they are "only supported when streaming input/output is used." A
single-prompt string query is documented to exit the underlying process
once the turn completes, and control requests like `interrupt()` are not
guaranteed to be honored against it. Since this adapter's protocol makes
`interrupt` a first-class op, the query mode has to actually support it —
so every turn goes through streaming-input mode, one message at a time,
which keeps the exactly-one-result-then-exhaustion contract identical to
what single-prompt mode would have given while making `interrupt()` a
genuinely supported call rather than an unspecified one. (This resolves
what was flagged as an unresolved spec-vs-SDK tension in an earlier
revision of this package — see git history for that revision's language.)

The first turn is a fresh query; every later turn adds
`resume: <session_id>`, captured from the first turn's `system`/`init`
message. Every `query()` call repeats the same authority-bearing options:
`model` (from `HELPER_MODEL` if set), `permissionMode: "bypassPermissions"`,
`includePartialMessages: false`, no `hooks`, no `canUseTool`.

A well-formed turn is exactly **one** top-level `result` message followed by
normal iterator exhaustion. Zero results, more than one result, a
`session_id` that drifts from the pinned/resumed session, or the iterator
itself rejecting, are each a distinct fatal (`no_result` / `multi_result` /
`session_drift` / `query_error`). A second `user_turn` while one is still in
flight is `busy` — the capsule is expected to enforce one-in-flight itself;
this is the helper's own double-check.

**Open question, fixture-decides:** the `session_drift` check compares every
message's `session_id` field against the pinned/resumed session as a flat
equality check across the whole turn. This assumes every message in the
turn — including any subagent-originated messages (`SDKUserMessage` carries
optional `subagent_type`/`task_description` fields for exactly this case) —
reports the *main* session's `session_id`, not a subagent-local one. If the
pinned-SDK conformance fixtures (golden Rust-writes/Julia-reads fixtures per
ADR 0039) show subagent messages carrying their own distinct session ids,
this check needs to learn to scope the comparison to top-level/main-thread
messages only (e.g. by `parent_tool_use_id === null`) rather than flat
equality across every message. Not resolved here because it depends on
observed SDK behavior this package cannot fabricate a fixture for.

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

## Design choices beyond the literal wire spec (flagged, not papered over)

An earlier revision of this package used single-prompt-string `query()`
calls and flagged, as an open tension, that the SDK's own `sdk.d.ts`
documents `interrupt()` as "only supported when streaming input/output is
used" — i.e. that the mandated non-streaming turn model might not actually
support the mandated `interrupt` op. That flag was reviewed and resolved by
switching the turn model to streaming-input mode with a one-message
iterable (see "Turn model" above): every turn now uses the mode `interrupt()`
is documented to support, while preserving the same one-result-per-turn
contract. It is no longer an open tension; kept here as the record of why
the turn model looks the way it does.

Three more spec points needed a concrete choice where the wire spec was silent, reviewed and accepted:

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
- **Uniform drain-await on every outgoing line**, not only around message
  emission — the spec's literal backpressure requirement is scoped to
  "before reading the next SDK message," but applying the same await
  uniformly to `hello`/`turn_end`/`interrupted`/`fatal` lines too is a strict
  superset of that guarantee and keeps the write path in one code path
  (`emit()`) instead of two.
