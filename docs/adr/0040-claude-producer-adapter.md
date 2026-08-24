# ADR 0040: The Claude producer adapter (Ship's Log P2)

**Status:** Accepted (2026-08-24). The contract for recording Claude agent
sessions as ADR 0039 voyages. Implemented by the `adapters/claude-sdk-helper`
Node package plus a `claude` producer adapter in the capsule; gated by the
conformance rig in §8 before anything consumes these voyages.
**Date:** 2026-08-24

> How this was designed: four adversarial review rounds against a working
> draft, with mandatory deletion pressure. The first draft drove the CLI's
> JSON stream directly and was overturned — the stream is an undocumented,
> unversioned subprocess protocol, and the producer-surface homework itself
> said never to parse it. The supported surface is the Agent SDK. Roughly
> fifteen mechanisms were deleted across the rounds; what remains satisfies
> the ADR 0037 invariants without inventing certainty the producer does not
> offer.

## Decision, in three sentences

A per-capsule **Node helper** embeds the pinned Claude Agent SDK, owns one
logical producer run, and speaks a thin versioned NDJSON protocol to the
capsule; **the helper is the epoch's logical producer** — the Claude
subprocesses its queries spawn are internal query executions. Each operator
turn is **one SDK `query()`** (resumed from the session's transcript from
the second turn on), so every turn has a per-input result barrier by
construction. The capsule records the helper's stream as semantic-JSON
producer frames under two registered required features, fences the helper in
a **nested cgroup inside a per-capsule systemd user unit**, and closes turns
honestly — including after crashes, by successor closure.

## The helper protocol (v1)

First line out: `hello {protocol: 1, sdk_version}`. Then:
`msg {body}` (one per SDK message, projected — §Codec), `turn_end
{query_id}` ("iterator exhausted normally", nothing more), `interrupted
{id, ok, sdk_return, note: "adapter-derived"}`, `fatal {reason, detail}`.
In: `user_turn {query_id, text}`, `interrupt {id}`, `shutdown`. Unknown or
malformed traffic in either direction is TERMINAL — protocol corruption may
have replaced a close or a permission event, so continuing to accept work is
forbidden. `producer_ready` derives from `hello` and means exactly "the
helper imported and validated its pinned SDK and can accept a query".

## One query per turn

`resume` restores the transcript; it does NOT restore in-memory MCP/tool
state, hooks, background workers, or prompt cache — the adapter repeats
every authority-bearing option on every query and records that this is the
continuity model. A turn is well-formed iff its query yields exactly one
top-level result message with the expected session id, then exhausts. The
turn's prompt is delivered as a ONE-MESSAGE streaming-input iterable (not a
plain prompt string): the SDK supports `interrupt()` only in streaming-input
mode, and one message per query preserves the per-input result barrier —
found during helper implementation, resolved without weakening the model.
Anything else is terminal. The streaming-input single-query mode is an
optimization seam, adoptable only after a pinned-SDK fixture proves
per-input result barriers empirically.

## Codec

Declared native representation: the **semantic JSON value** of each helper
line (byte-verbatim is explicitly NOT claimed; ADR 0039 permits an
adapter-declared representation). The helper's recursive projection is part
of the versioned codec: `toJSON` once per node; enumerable own properties;
`undefined`/functions absent in objects and `null` in arrays; `bigint`,
non-finite numbers, cycles, and exotic object classes are terminal
serializer failures. Producer-payload numbers are finite IEEE-754 binary64
in ordinary JSON spelling under the required feature
**`sot.producer.json-f64-v1`** (control and envelope fields remain u53/i53).
Bounds: 8 MiB per line, 64 MiB spooled per turn — exceeding either is a
terminal bound, never a truncation. Capture-on malformed bytes ride
`payload_ref {encoding: "bytes"}` — the stated exception to semantic JSON.

## Turn attribution — ordered rules

Option set pinned: no partial-message events, no hooks, no permission
callback; the observability subtype list is frozen from the pinned SDK at
helper build time. The adapter indexes `tool_use.id → turn`; a collision is
terminal. Rules in order: (1) the current query's top-level result closes
its turn; (2) correlation ids (`tool_result.tool_use_id`,
`parent_tool_use_id`; present-but-null = absent) must all resolve to ONE
turn — disagreement terminal, unresolved turn-free with a warning; (3)
session-scoped types are turn-free; (4) operator-echo user messages are
turn-free, redacted, retained-transformed; (5) known mainline messages with
no ids belong to the current query's turn — sound because each query is
single-shot, and GATED: the adapter cannot ship against a pinned SDK until
the no-replay resume fixture passes; (6) unknown types are recorded
turn-free (whole-frame redacted under capture-off), then the adapter stops
admitting turns, drains, and terminates.

Turn frames: `turn_open` commits durably BEFORE the forward syscall
(`input` → `forward_intent` → `turn_open` with `responds_to` → input →
take-fence recheck → write → `forwarded`); a failed recheck or write leaves
the WAL at delivery-unknown (never `refused_stale_epoch` after intent) and
closes the turn `failed`. Interrupt-vs-result races fold by log order with
`duplicate_of`. The producer_observed fact is not emitted in v1 — no stable
echo key exists; WAL chains legally end at `forwarded`.

## Kill domain

Each claude-adapter capsule is the main process of its own transient
systemd user unit (`Delegate=`); the helper lives in a **nested cgroup**
(moved in before any helper code runs), capsule outside it. Capsule death ⇒
systemd kills the unit — the kernel-enforced lease; the helper's stdin-EOF
exit is only a graceful fast path. The nested-cgroup locator is
authority-bearing (successors kill by it) and rides the required feature
**`sot.capsule.cgroup-fence-v1`**. Termination: refuse input →
`cgroup.kill` → wait `cgroup.events populated=0` → reap the helper → remove
the cgroup → only then `producer_dead` + synthesized closes → seal. The
fence contains inherited descendants; it cannot contain work handed to an
external supervisor — that surface is denied to v1's toolset and stated as
outside the guarantee. Fails closed without cgroup delegation.

## Closure verification

Two verifier predicates (ADR 0039 §Verifier, amended): **complete** — every
`turn_open` has a winning close; the default, and the only CERTIFYING mode,
required for archival, recovery acceptance, CI, and offline verification.
**`--allow-open-tip`** — a non-certifying diagnostic tolerating one
unmatched open in the currently open tip's epoch; only the owning capsule
(or a monitor consuming its committed watermark) may treat it as live
health. Successor recovery closes unmatched prior-epoch opens
(synthesized_death, fsynced, under the lock, before any input) — which is
what makes complete mode reachable after any crash.

## Redaction ("claude-sdk-default") and forensics

Operator-echo user messages: every typed content block (text, image
sources, documents, filenames/metadata) redacted with concrete
`transformed.ops`, frames retained. Config: allowlisted flag names; free
text values recorded as `"redacted"` (no hashes — equality oracles).
Permission model: `bypassPermissions`, no callback — nothing interactive
exists to hang. Interrupts are three frames: request (fsynced before the
op), response (`responds_to`, SDK return preserved, `ok` labeled
adapter-derived), outcome (scope+target, once disposition is known).
Capture-off: unclassifiable lines are whole-frame redacted (presence +
length + reason); raw bytes go only to a fixed, append-only **forensic
sidecar in the producer home** — fsynced before the presence frame, located
by `{store_version, file_id, offset, length}` with a random opaque file id —
and are honestly OUTSIDE the checksummed voyage: full forensic fidelity
requires capture-on. Producer home: `<voyage>/producer/claude/`, 0700,
no-follow creation, parent fsync before spawn; `CLAUDE_CONFIG_DIR` points
into it; subagent transcripts land there too (pinned-SDK gate asserts it).

## Gates

Actual-helper tests against a fake claude executable (the SDK's executable
path override) — serializer, permission mode, EOF/EPIPE, ordering; a
pinned-SDK resume fixture carrying the no-replay premise and the per-turn
latency/cost benchmark (`total_cost_usd` is cumulative, never differenced);
capture-mode × crash-boundary canary cross-product scanning the voyage
byte-tree (all transport encodings) with process-tree quiescence asserted
after every terminal fixture; the P1 kill sweep rerun under this adapter;
SDK package + bundled executable pinned by hash. The credentialed live
smoke stays a smoke, not the oracle.

## Consequences

- Claude sessions become durable, verifiable, replayable records with real
  turns — the first rich producer, and the template for Codex and Julia
  REPL adapters (their own ADRs will be shorter: the helper pattern, turn
  tables, and kill domain generalize).
- Two new required features enter the ADR 0039 registry; pre-feature
  readers fail closed on these segments, exactly as designed.
- The design history (four review rounds, the deletion log) lives in the
  private working notes; this ADR stands alone.
