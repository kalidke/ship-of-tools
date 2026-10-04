# rust/log/src/capsule/writer_loop: the leg's writer loop (capsule)

`run` drives one producer on a pseudoterminal and records its terminal into the voyage. Output is published only
after it is fsynced, and the voyage's lanes are served through AttachProto until teardown closes them. Part of
capsule; charter: rust/log/CLAUDE.md.

## Files
- `mod.rs`: `run`, the leg's writer loop, and the state and guards it owns.

## Start here
`run` in mod.rs, read top to bottom.

## Rules
- Output reaches an attach subscriber only after the commit that covers it is fsynced (`flush_output!`).
