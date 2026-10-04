# rust/log/src/store/verify: the voyage verifier (capsule)

The `sot-log verify` checklist of ADR 0039: wrapper and seal validity, identity and index continuity, epoch and take
ordering, the cross-field matrix, the input-fact lattice, stream chains and blob presence, run over a voyage's segments.
It also holds the two per-leg readers the supervisor uses to decide a respawn and to judge stability. Part of capsule;
charter: rust/log/CLAUDE.md (a forward reference: that page is not yet at this commit).

## Files
- `mod.rs`: the checklist's types (`VerifyMode`, `REGISTERED_FEATURES`, the frame objects it reads), `verify_voyage` and the payload and blob helpers.
- `pass.rs`: `verify_voyage_mode`, the pass over a voyage's segments, and the segment-level rules (listing, quiescence, header, turn closure).
- `lifecycle.rs`: the lifecycle frame rules (kind fields, kill-domain locator, take order, input_fact lattice)
- `leg.rs`: `leg_carries_run_end_marker` and `leg_producer_uptime_ms`, the per-leg readers, with their tests.
- `support_tests.rs`: the `store` test helper shared by the test files.
- `features_tests.rs`: tests of feature opt-ins, spilled frames, turn closure, f64 gates, rotation and attached_to.
- `matrix_tests.rs`: tests of the cross-field matrix, input-fact lattice, stream chains, take epochs and blob refs.

## Start here
`pass.rs` `verify_voyage_mode` for the walk and the segment rules; `lifecycle.rs` for a lifecycle frame rule, though the frame loop still sits in `pass.rs`; `leg.rs` for the questions the supervisor asks of one leg.

## Rules
- `VerifyMode::Complete` is the only certifying mode (`verify_voyage` uses it); `AllowOpenTip` is for a live writer's own use.
- A segment declaring a feature not in `REGISTERED_FEATURES` is refused whole (`check_segment_header`; test
  `unknown_feature_name_refuses_the_whole_segment`).
- The leg readers err loud on a mismatched or malformed segment and never answer "no marker" for one
  (`leg_carries_run_end_marker`).
