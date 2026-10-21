# rust/log/src/lane/wire: the three lanes' frames, bytes in and typed frames out (capsule)

Pure encode and decode for the capsule's three lanes (SOM0 management, SOA0 attach, SOSV supervisor): no I/O, no
clocks, no role machine. One outer frame (magic, body length, body) carries a lane-specific body behind a tag byte.
Part of capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: the outer frame, magics and limits, tags, `WireError`, the byte codec (`Reader`, `push_*`, `wrap`), `FrameSplitter`
- `mgmt.rs`: the SOM0 lane: `MgmtRequest`, `MgmtReply`, `Survival`, their encoders and `decode_mgmt_body`
- `attach.rs`: the SOA0 lane: `AttachClient`, `AttachServer`, refusal reasons, `negotiate`, checkpoint bounds, `encode_keepalive`, `decode_attach_body`
- `supervisor.rs`: the SOSV lane: `SupervisorRequest`, `SupervisorReply`, operation ids and states, their encoders and `decode_supervisor_body`
- `support_tests.rs`: `assert_golden`, `feed_ok`, `feed_err`, shared by the test files
- `golden_tests.rs`: exact bytes of every mgmt and attach frame, keepalive, pen and geometry
- `framing_tests.rs`: lane binding, caps, unknown tags, trailing bytes, negotiation, chunk arithmetic, the failed latch
- `bounds_tests.rs`: field bounds and edge completeness for the mgmt and attach lanes
- `supervisor_tests.rs`: goldens, bounds and splitter behaviour for the supervisor lane

## Start here
`mod.rs`'s module doc (the tag table), then the lane file of the frame you change and its golden test.

## Rules
- A connection's first frame binds its lane: `FrameSplitter::feed` refuses another magic (`WireError::LaneMismatch`) and an unknown one (`UnknownMagic`).
- A body length over `MAX_BODY_LEN` is refused when the 8-byte header arrives, before any body is buffered.
- A failed splitter stays failed: its buffer is freed and every later `feed` returns the same error; frames decoded earlier in the same call are still returned.
- Lanes are lockstep with no correlation ids; the caller enforces it, this folder only defines frames.
- Magics, tags and limits are read by other processes and releases: SOM0 is never versioned; SOA0 changes only through `ATTACH_PROTO_V*` and `negotiate`; SOSV refuses any version but `SUPERVISOR_PROTO_V1`.
- `operation_id` is validated at decode (`validate_operation_id`): the journal uses it as a file name.
- `MAX_CHECKPOINT_LEN` copies the vt100 fork's bound; `pinned_checkpoint_len_matches_the_fork` checks it on Windows.
