# rust/backend/src/rows/reauth: workspace.reauth (rows)

`workspace.reauth` moves a live capsule row to another account and resumes its conversation there. The row's own
session is the thing replaced, so the order of steps is the design: validate, record, write the accept, then restart.
Part of the daemon's rows subsystem; charter: rust/backend/src/rows/CLAUDE.md.

## Files
- `mod.rs`: the accept half: `check`, `handle_workspace_reauth`, `write_accept_then`, `answer_workspace_reauth` (the connection's arm), `ReauthRestart` and its rollback
- `restart.rs`: the restart runner: `restart_blocking`, `LiveSupervisor`, `RestartEffects`, the voyage mint and the settle wait
- `restart_tests.rs`: the runner's effect order, revival and mint-licence cases, with the fake supervisor they drive
- `support_tests.rs`: shared fixtures: the env guard and the seeded homes and rows
- `check_tests.rs`: the `check` refusals and the accept that passes them
- `accept_tests.rs`: the handler's payloads and the accept-frame ordering cases, with the fake peer they write to

## Start here
`mod.rs::handle_workspace_reauth` for the ops a reauth runs in order, then `restart.rs::restart_blocking` for what
happens to the old leg after the accept frame is written.

## Rules
- Every check runs before anything changes: `check` precedes `set_account` in `handle_workspace_reauth`, so a refusal
  leaves the record and the leg as they were.
- The new account is recorded before the old leg is touched, and the accept frame is written before the restart is
  handed its plan: `write_accept_then` hands the plan on only after the write returned.
- An accept that cannot be written rolls the record back: `ReauthRestart::rollback`.
- A replacement voyage is minted only when a new authority rests at `ended_no_respawn`: `ready_to_mint`.
