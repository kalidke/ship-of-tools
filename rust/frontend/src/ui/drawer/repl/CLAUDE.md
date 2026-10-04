# rust/frontend/src/ui/drawer/repl: the Julia REPL drawer (fe-ui)

The REPL tenant of the drawer: the log of submitted evals with the frames each returned, the submit path and Up/Down
history over it, and the projection of the log into display lines. Part of fe-ui; charter:
rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the three files below.
- `log.rs`: `ReplEntry`, `State::submit_repl_input` and the history walk (`State::history_step_back`, `State::history_step_forward`).
- `lines.rs`: `build_repl_lines` (log to display lines and image slots), `ReplImage`, `ReplImageSlot` and `pinned_repl_scroll`.
- `replies.rs`: repl.eval, repl.frame and repl.run_file replies

## Start here
`State::submit_repl_input` in log.rs for how an eval leaves; `build_repl_lines` in lines.rs for how the log is drawn.

## Rules
- An eval is tagged with the host and workspace that issued it (`State::submit_repl_input` fills `eval_id_workspace`).
- The log keeps at most 256 entries, trimmed from the front (`State::submit_repl_input`).
- A history walk skips in-flight entries (`history_step_back`).
- A scrolled view stays on its rows while output grows (`pinned_repl_scroll`).
