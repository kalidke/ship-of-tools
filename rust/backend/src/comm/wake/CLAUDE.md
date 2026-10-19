# rust/backend/src/comm/wake: the wake of idle sessions, and the dialog tick (backend)

The daemon's tick that types a one-line wake into a session's free prompt when its handle has unread mail. The tick
decides when, `unread.rs` counts the mail, `screen.rs` reads the screen and `attempt.rs` does the one attach. Beside it,
the dialog tick reads each Claude row's screen with the same `screen.rs` and stamps the row's `dialog` fact. Part of the
backend; charter: comm/CLAUDE.md.

## Files
- `attempt.rs`: one wake's attach, hold, type and Enter, and its outcome
- `dialog.rs`: the dialog tick: a Claude row whose screen shows a dialog that waits on a person gets `dialog`, and `resumed` once it has gone
- `dialog_tests.rs`: the tests of the dialog tick, through the real `comm-status.sh`
- `mod.rs`: the tick: when a row is woken, and the account of the whole wake
- `screen.rs`: whether a captured screen is a free prompt, the rows the hold compares, and whether it shows a dialog (`dialog_of`)
- `screen_tests.rs`: the tests of the screen reading, over captured frames
- `unread.rs`: how much mail is unread: the inbox against the cursor

## Start here
`mod.rs` `check_row` for when a row is woken; `screen.rs` `prompt_of` for what counts as a free prompt.

## Rules
- The wake only reads `inbox/<h>.jsonl` and `read/<h>.cursor`.
- "Last woken" lives in the tick's memory, so after a restart each row with unread mail is woken once.
- Every TICK (2 s), `run` checks each capsule row with a declared handle; a handle is declared on one row only
  (`set_agent_handle`, `scan_disk`).
- `check_row` wakes only a Ready row with fresh mail, or mail unread past REPEAT_AFTER (600 s) (`decide`); a row owed an
  Enter (`Woken::enter_owed`) is instead sent Enter alone, fresh mail or not, until REPEAT_AFTER (`Decision::Complete`). It skips a row
  whose registry `stop_at` mark is under STOP_HOOK_BOUND (60 s) old (`stop_hook_running`).
- `cursor_offset` ports comm-lib-inbox.sh's `sot_cursor_offset`, which is the spec; `agrees_with_the_shell` runs both (Linux).
- `counts` is the one unread rule: a JSON object whose `to` is a string equal to the handle and whose `from` is not it.
  `sot_unread` in comm-lib-inbox.sh is its shell twin, which the Stop hook counts with; `unread_agrees_with_the_shell` runs both (Linux) (they agree on every line a writer emits; PROTOCOL.md names the malformed lines on which only the shell counts).
- `prompt_glyphs` knows claude only, so a Codex row is never typed into.
- `wake_if_free` makes one attach. A free first frame must hold still through the box's lower rule (`held_rows`) for
  STILL_FOR (1.5 s), and the live screen must read free again.
- After typing, `wake_if_free` waits up to OP_BUDGET (3 s) for `typed_refusal` to see the line alone in main's input
  box (`type_then_enter`); Enter goes only then.
- A typed line counts as the wake and, within one daemon run, is never typed again for its batch (`enter_owed` lives in
  the tick's memory, so a restart can type it once more); one left without Enter is sent, Enter alone,
  by a later tick that finds it alone in main's input box (`decide`'s Complete, `Woken::enter_owed`). An Enter the leg
  refused as stale wrote nothing and leaves the line owed the same way (`WakeOutcome::EnterRefused`).
- The dialog tick (`dialog.rs`, every DIALOG_EVERY, 5 s, Unix only) reads Ready capsule rows whose agent is `claude`, with
  a declared handle and an entry that reads `working` or carries a `dialog` fact (`due`), one watcher attach each (never
  the pen, a resize or a record frame). A sweep is awaited before the next tick and missed ticks are skipped.
- `dialog_of` says a screen shows a dialog only when no main input box (`main_box_shown`: a glyph and the NBSP between
  two borders) is on screen and one of the last DIALOG_ROWS (30) rows is a dialog footer (`is_dialog_footer`: a line that
  starts with "Enter to select" or "Esc to cancel" and a middle dot). "Do you want to" only names the summary.
- The open dialog is keyed by its row, never its text (a menu's tabs and auto mode's countdown change it): a row that
  holds the fact gets nothing new while a dialog still shows.
- It writes only where screen and registry disagree (`stamp_for`): `dialog` when a dialog shows and the row has no fact,
  `resumed` when none shows and it has one. It keeps nothing between ticks, and writes through `comm-status.sh` with
  `SOT_COMM_ROW_HANDLE`, never with Rust's own reduction. A Codex row is never read.
