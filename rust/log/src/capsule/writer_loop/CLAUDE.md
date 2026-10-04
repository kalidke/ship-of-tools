# rust/log/src/capsule/writer_loop: the leg's writer loop (capsule)

`run` drives one producer on a pseudoterminal and records its terminal into the voyage. Output is published only
after it is fsynced, and the voyage's lanes are served through AttachProto until teardown closes them. Part of
capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: `run`, the leg's writer loop, and the state and guards it owns.
- `start.rs`: Starting a leg: open the voyage, bind the transport, write the control preamble, spawn the producer and its reader, and hand `run` the loop state.

## Start here
`run` in mod.rs, read top to bottom.

## Rules
- Output reaches an attach subscriber only after the commit that covers it is fsynced (`flush_output!`).
- `Leg`'s last six fields drop in declaration order: the budget is cancelled first and the writer lock released last, after the transport shuts down (`Leg` in mod.rs; test `shutdown_guard_runs_while_the_writer_lock_is_held`).
