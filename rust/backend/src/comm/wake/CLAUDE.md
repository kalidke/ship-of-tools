# rust/backend/src/comm/wake: the wake of idle sessions (backend)

The daemon's tick that types a one-line wake into a session's free prompt when its handle has unread mail. The tick
decides when, `unread.rs` counts the mail, `screen.rs` reads the screen and `attempt.rs` does the one attach. Part of the
backend; charter: comm/CLAUDE.md.

## Files
- `attempt.rs`: one wake's attach, hold, type and Enter, and its outcome
- `mod.rs`: the tick: when a row is woken, and the account of the whole wake
- `screen.rs`: whether a captured screen is a free prompt, and the rows the hold compares
- `screen_tests.rs`: the tests of the screen reading, over captured frames
- `unread.rs`: how much mail is unread: the inbox against the cursor

## Start here
`mod.rs` `check_row` for when a row is woken; `screen.rs` `refused_on` for what counts as a free prompt.

## Rules
- The wake only reads `inbox/<h>.jsonl` and `read/<h>.cursor`.
- "Last woken" lives in the tick's memory, so after a restart each row with unread mail is woken once.
- Every TICK (2 s), `run` checks each capsule row with a declared handle; two rows declaring one handle: neither is woken.
- `check_row` wakes only a Ready row with fresh mail, or mail unread past REPEAT_AFTER (600 s) (`decide`). It skips a row
  whose registry `stop_at` mark is under STOP_HOOK_BOUND (60 s) old (`stop_hook_running`).
- `cursor_offset` ports comm-lib-inbox.sh's `sot_cursor_offset`, which is the spec; `agrees_with_the_shell` runs both (Linux).
- `prompt_glyphs` knows claude only, so a Codex row is never typed into.
- `wake_if_free` makes one attach. A free first frame must hold still through the box's lower rule (`held_rows`) for
  STILL_FOR (1.5 s), and the live screen must read free again.
- Enter goes only when `typed_refusal` sees the line alone in main's input box.
- A typed line counts as the wake whether or not Enter followed (`step_of`), so a batch is never typed twice.
