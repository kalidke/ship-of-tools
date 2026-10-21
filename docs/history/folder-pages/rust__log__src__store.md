# rust/log/src/store: the voyage store (capsule)

A voyage is an append-only, CRC-framed, seal-chained record of one terminal that is read forever. This folder holds its
record codec, envelope schema, segment files, open-time recovery and the reader-first rollout gate. It also holds the store
that opens a voyage for its one writer and publishes its blobs. Part of capsule;
charter: rust/log/CLAUDE.md (a forward reference: that page is not yet at this commit).

## Files
- `mod.rs`: declares the modules below and the test-only frame builders.
- `dedupe.rs`: the input-WAL dedupe index folded from the retained voyage at open (ADR 0041 decision 5), with its tests.
- `record.rs`: the record wrapper, an 18-byte CRC-checked prelude plus body, and `classify_tail`.
- `envelope.rs`: the frame envelope and class payloads, the normative schema with fail-closed enums.
- `segment.rs`: segment files (`.open`, `.recovering`, `.recovering-out`, `.sotseg`), their writer, reader and seal chain.
- `recovery.rs`: startup reconciliation and tear recovery, idempotent at every crash point.
- `rollout.rs`: the ADR 0041 reader-first rollout gate for a feature-bearing segment.
- `support_tests.rs`: frame builders shared by the voyage and dedupe tests.
- `verify/`: the `sot-log verify` checklist and the per-leg readers.
- `voyage.rs`: the store that opens a voyage for its one writer and publishes its blobs.
- `voyage_tests.rs`: the voyage store's tests: bootstrap, reopen, fence, lease, blob CAS, root pin, Windows arms.

## Start here
`segment.rs` `SegmentWriter::append` and `SegmentWriter::seal` for a format change; `recovery.rs` `reconcile` for what
open-time repair may do; `voyage.rs` `VoyageStore::open_prepared` for anything that opens or writes a voyage.

## Rules
- Written bytes are read forever: the format changes only through `codec_id` and `required_features` (`record.rs`,
  `HeaderBody` in `segment.rs`).
- Only a provably torn tail is discarded (`record::classify_tail`, `recovery::reconcile`); every other defect halts and
  nothing is deleted.
- A segment is published only by a no-clobber rename followed by a flush of its directory
  (`host::publish_noreplace`, called from `SegmentWriter::seal` and `recovery`).
- A feature-bearing run opens only when `rollout::gate` clears the feature against the installed rollback target's
  reader (called from the capsule's run open). The transaction that would write that evidence is unbuilt, as
  `rollout.rs`'s own doc says.
- One writer per voyage: `VoyageStore::open_prepared` pins the root (`host::PinnedDir`), takes `writer.lock` through the
  pin and checks the parent-death lease before it reads any history.
- The dedupe index is folded in that same walk and fails closed on history it cannot trust (`dedupe::walk_segment`).
  The fold and the verifier parse an `input_fact`'s `fact` object with the one `dedupe::FactObj`.
- The layout names are `voyage::SEG_DIR`, `BLOBS_DIR` and `WRITER_LOCK`; code joins these, and tests spell the literals
  because the layout is part of the format.
- A blob is published temp, fsync, no-clobber rename; a collision with different bytes is loud
  (`VoyageStore::publish_blob`).
