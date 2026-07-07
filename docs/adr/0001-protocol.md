# ADR 0001: Line protocol

**Status:** Accepted
**Date:** 2026-05-07

## Context

Three Rust↔Julia seams (frontend↔backend, backend↔kernel, backend↔REPL) need IPC. Requirements: cross-platform (Windows + Linux), debuggable by hand, supports binary blobs (PNG previews, plot images), low ceremony.

## Decision

Newline-delimited JSON (NDJSON) for control + length-prefixed binary frames for blobs.

Each control message is one JSON object, UTF-8, `\n` terminated:

```json
{"v":1,"id":<u64>,"kind":"req"|"res"|"evt","op":"<verb>","payload":{...}}
```

Requests carry a client-chosen `id`; responses echo it; events use `id:0`.

Payloads with binary content carry `{"blob":{"len":N,"mime":"…"}}`. The next `N` bytes on the stream are the blob; then the next line resumes JSON.

Phase 1 ops: `mode.list`, `mode.tree.root`, `mode.tree.children`, `mode.preview`, `repl.eval`, `repl.interrupt`, `fs.read`, `fs.write`, `concept.read`, `concept.write`, `chat.send`, `chat.cancel`. Events: `tree.invalidate`, `repl.frame`, `chat.delta`, `file.changed`.

Shared types live in `rust/protocol/`, consumed by both `rust/frontend/` and `rust/backend/`. `serde` with `#[serde(tag="op")]`. Kernel side uses `JSON3.jl`.

## Consequences

- Debuggable with `tail -f` against any seam — pays for itself the first time something hangs.
- Slight parse overhead vs binary-only protocols; negligible at human-interactive rates.
- Versioned via `v` field; format bump is a one-line change.
- Length-prefixed blobs avoid base64 cost (~33% bandwidth + CPU on every PNG).
