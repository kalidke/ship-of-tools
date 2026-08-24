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
| `{"ev":"turn_end","query_id":<id>,"results":<count>}` | The turn's query iterator exhausted normally. `results` is how many `result` messages were seen — normally 1; see "Turn model" for the 0-after-interrupt case. |
| `{"ev":"interrupted","id":<id>,"ok":<bool>,"sdk_return":<value\|null>,"note":"adapter-derived"}` | After an `interrupt` op. `ok` only reflects whether `interrupt()` resolved without throwing — never SDK confirmation that anything was actually cancelled. |
| `{"ev":"fatal","reason":"<reason>","detail":<string>}` then `exit(1)` | Any protocol violation. `detail` is always a short, fixed, non-user-derived string (or omitted, for `reason:"protocol"`, matching the spec's own example) — never raw input bytes. |

Fatal `reason` values: `protocol`, `no_result`, `multi_result`,
`session_drift`, `query_error`, `busy`, `serializer`, `line_too_large`,
`turn_too_large`.

### In (capsule → helper)

| Line | Effect |
|---|---|
| `{"op":"user_turn","query_id":<id>,"text":"<prompt>"}` | Runs one SDK `query()` for this turn. |
| `{"op":"interrupt","id":<id>}` | Calls `interrupt()` on the in-flight query. |
| `{"op":"shutdown"}` | Clean `exit(0)`. |

An unknown `op` or a malformed input line ⇒ `{"ev":"fatal","reason":"protocol"}`
then `exit(1)`. So does a peer that sends 8 MiB of bytes without ever
completing a line — the input side is capped at the same 8 MiB as an output
line (`LineReader` in `src/protocol.ts`), so unbounded, newline-less input
can never grow the helper's memory without bound; reason `"protocol"` was
picked over `"line_too_large"` to keep that reason scoped to *our own*
output lines being too large, distinct from a misbehaving peer's input.
stdin EOF (parent death) ⇒ abort any in-flight query and exit within one
event-loop turn, with no fatal line — this is the fast path, not an error.

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
normal iterator exhaustion. More than one result, a `session_id` that
drifts from the pinned/resumed session, or the iterator itself rejecting,
are each a distinct fatal (`multi_result` / `session_drift` / `query_error`).
A second `user_turn` while one is still in flight is `busy` — the capsule is
expected to enforce one-in-flight itself; this is the helper's own
double-check.

**Zero results** is fatal `no_result` — *unless* `interrupt` was invoked for
this turn's query at any point before the iterator exhausted, in which case
zero results is the expected outcome of a successful interrupt, not a
violation: the helper emits `turn_end` with `results: 0` normally, no fatal.
Without an interrupt, zero results stays fatal `no_result` exactly as
before. The `interrupted` flag is per-turn (set the moment `interrupt()` is
invoked for the current in-flight query, regardless of whether that call
itself later resolves `ok:true` or `ok:false`) and is read once, right
before the turn's `inFlight` record is cleared.

**Per-turn spool bound.** The sum of every emitted `"msg"` line's wire bytes
during one turn is capped at 64 MiB (`TURN_SPOOL_CAP_BYTES` in
`src/main.ts`, reset to 0 at the start of every turn) — the spool bound
from ADR 0040's Codec section. Crossing it is a terminal fatal
(`turn_too_large`), **never a truncation**: the `"msg"` line that would
cross the bound is never written at all, so what's on the wire before the
fatal is always a set of complete, individually-valid lines, never a cut-off
one. This is independent of, and much larger than, the existing 8 MiB
**per-line** cap (`line_too_large`) — a turn can legally emit many lines
each well under 8 MiB and still trip the 64 MiB per-turn total.
`HELPER_TEST_TURN_CAP` overrides the 64 MiB default; it is read once at
module load and exists purely so tests can exercise this path with a
handful of small messages instead of actually constructing 64 MiB of
scripted output — the real capsule never sets it, and it plays no other
role.

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
- `bigint`, a non-finite number, a reference cycle, a depth past 256, or
  anything that is still neither a JSON primitive, a plain object, nor an
  array after the `toJSON` step, throws — surfaced as `fatal
  reason:"serializer"`.
- Finite fractional numbers pass through as ordinary JSON numbers.
- Every outgoing line is capped at 8 MiB **after** `JSON.stringify`,
  **including the trailing newline** (the cap covers the wire bytes actually
  written, not `JSON.stringify()`'s output alone — a line whose JSON is
  exactly 8 MiB is one byte over once the newline lands) — over that, `fatal
  reason:"line_too_large"` instead of the line that triggered it.
- After writing each line, the helper awaits the underlying `write()` call's
  own completion **callback**, not its boolean return value or the stream's
  `drain` event — the boolean only reports whether the internal buffer is
  under the highWaterMark, a weaker signal that data has actually left the
  process; on a platform where a pipe write can complete asynchronously even
  when `write()` returns `true`, resolving on that instead could let
  `process.exit()` (e.g. right after the final fatal line) run before the
  bytes were actually flushed. This holds uniformly for every line emitted,
  a superset of the spec's minimum ("before reading the next SDK message").

**Cycle detection keys on pre-`toJSON` identity**, not the value `toJSON()`
returns. A `toJSON` that manufactures a fresh wrapper object every call
(`a.toJSON = () => ({ self: a })`) would otherwise never repeat an object
identity in the post-`toJSON` visited-set, and the walk would recurse until
the real JS call stack overflowed instead of failing cleanly with
`SerializerError`. Keying on the original reference (the value as received,
before any `toJSON` call) catches the cycle on the second visit to `a`,
before `a.toJSON()` is even invoked again. The depth bound (256) is a
second, independent backstop against any other unbounded-recursion shape,
including deeply nested structures that never repeat an identity at all.

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

Four more spec points needed a concrete choice where the wire spec was silent, reviewed and accepted:

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
- **Uniform write-completion-await on every outgoing line**, not only around
  message emission — the spec's literal backpressure requirement is scoped
  to "before reading the next SDK message," but applying the same await
  uniformly to `hello`/`turn_end`/`interrupted`/`fatal` lines too is a strict
  superset of that guarantee and keeps the write path in one code path
  (`emit()`/`writeLine()`) instead of two. (A peer-review round hardened
  *what* is awaited here — see "Codec" above: the write's own completion
  callback, not `write()`'s boolean return or the `drain` event.)
- **Fatal `"protocol"`, not `"line_too_large"`, for oversized unterminated
  input** — see "Protocol" above. Both were viable per the review finding
  that asked for this cap; `"protocol"` was picked to keep `"line_too_large"`
  scoped to our own output lines being too large, distinct from a
  misbehaving peer's input.
