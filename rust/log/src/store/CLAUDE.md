# rust/log/src/store: the voyage store (capsule)

A voyage is an append-only, CRC-framed, seal-chained record of one terminal that is read forever. This folder holds its
record codec, envelope schema, segment files, open-time recovery and the reader-first rollout gate. Part of capsule;
charter: rust/log/CLAUDE.md (a forward reference: that page is not yet at this commit).

## Files
- `mod.rs`: declares the five modules below.
- `record.rs`: the record wrapper, an 18-byte CRC-checked prelude plus body, and `classify_tail`.
- `envelope.rs`: the frame envelope and class payloads, the normative schema with fail-closed enums.
- `segment.rs`: segment files (`.open`, `.recovering`, `.recovering-out`, `.sotseg`), their writer, reader and seal chain.
- `recovery.rs`: startup reconciliation and tear recovery, idempotent at every crash point.
- `rollout.rs`: the ADR 0041 reader-first rollout gate for a feature-bearing segment.

## Start here
`segment.rs` `SegmentWriter::append` and `SegmentWriter::seal` for a format change; `recovery.rs` `reconcile` for what
open-time repair may do.

## Rules
- Written bytes are read forever: the format changes only through `codec_id` and `required_features` (`record.rs`,
  `HeaderBody` in `segment.rs`).
- Only a provably torn tail is discarded (`record::classify_tail`, `recovery::reconcile`); every other defect halts and
  nothing is deleted.
- A segment is published only by a no-clobber rename followed by a flush of its directory
  (`fsutil::publish_noreplace`, called from `SegmentWriter::seal` and `recovery`).
- A feature-bearing run opens only when `rollout::gate` clears the feature against the installed rollback target's
  reader (called from the capsule's run open). The transaction that would write that evidence is unbuilt, as
  `rollout.rs`'s own doc says.
