# rust/backend/src/lifecycle: lifecycle (contract)

The last window on a computer decides, through its lease, whether that computer's sessions end, and every end of the
daemon is bounded. Lifecycle stays inside the `sotd` binary (it needs rows and the server's accept loop); the window's
half is `rust/frontend/src/lease.rs` and `lease_exit.rs`. Process containment is not lifecycle's: it is the crate
`rust/containment` (its contract is `rust/containment/CLAUDE.md`), which lifecycle fires and never reaches into.
Lifecycle decides when anything ends and asks rows to end rows (`rows::end`); it never ends a row itself.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on and the limit it has. The Tests section holds the test
of each.

1. **A lease is granted only to a peer the OS named.** `Leases::grant` accepts a claim whose pid, creation time and boot
   equal what the OS reported at accept (`lease::claim`); a peer whose process the OS did not report, or a daemon whose
   own boot is unknown, is `Undetermined`; a different claim is `Foreign`; a phase other than open is `Closing`. A
   grant whose record cannot be written is undone and refused `Undetermined`: a grant never outruns its record. Limit:
   on Windows an empty boot on either side matches (the pipe client's pid and creation time are the proof there).
2. **`held.json` is the one record, and a restart reads it alone.** It exists iff some field is non-empty or true, so
   one window's `Keep` or `Close` never deletes another holder's entry; it is written durably
   (`sot_platform::host::write_durable`, `remove_durable`); its name and shape are `sot_protocol::ops::lease::
   HELD_RECORD_FILE` and `HeldRecord` (version 1; another version plans Cleanup). Every deadline in it is wall-clock unix
   milliseconds, so a persisted handover survives a restart. Limit: a clock set backwards extends a persisted deadline
   by the step.
3. **Only the last lease's departure decides, and the latest intent wins.** A lease ends by `fe.leaving` or by its
   connection's end (an end with no intent means `Close`). While another lease is held nothing is decided. The last one
   out: `Close` ends the sessions (`Leases::gone` fires and the daemon shuts down), `Keep` leaves them up with no bound,
   `Handover` leaves them up for `HANDOVER_BOUND` (the ticker turns an expired handover with no lease into a shutdown).
   A later `fe.leaving` on a lease that already departed replaces its earlier intent. A lease's generation fences its own
   removal (an unknown or already-removed one decides nothing). A malformed lease line and a lane connection's end
   depart nothing. The lease ops and bounds are `sot_protocol::ops::lease`'s.
4. **A start's plan is a function of the record and this boot alone.** `startup_plan` is Resume, Pending or Cleanup and
   never looks a process up, so a reused pid cannot resume or hold a row: an unreadable record, a closing record, a
   record of another boot (two non-empty, different boot ids) or an expired handover plans Cleanup; a handover not yet
   expired or recorded holders plan Pending; otherwise Resume. Cleanup ends every row and resumes none
   (`startup::cleanup`, through rows' end API), and claims the slugs of the rows it lists before any listener binds and
   holds them until it ends (`Workspaces::claim_create`), so a same-label create is refused meanwhile and the Cleanup
   never ends a row a window created. The leases are built from the record, and a listed row's `forget` dropped, before
   any connection is accepted.
5. **The close stops accepting before it touches a row, and ends within `SHUTDOWN_BOUND`.** The accept loop breaks on
   `Leases::gone` and drops the listener before `shutdown::run`; the close asks rows to end every row without resuming
   any (`rows::end::end_every`) by one deadline, counts each row not confirmed ended, writes the final record
   (`not_ended` stays recorded until a window acknowledges it), and a backstop thread exits 1 at `SHUTDOWN_BOUND` through
   the terminal. A close that finishes exits 0. The update restart exits 75 only while no shutdown has begun:
   `Leases::commit_update` moves the phase to `Updating` under the lease lock, so no close begins afterwards, and a close
   that began first refuses the update. Limit: a child creation stalled in the OS can delay the fire (containment's limit
   in guarantee 2), never the exit.
6. **Every controlled end of the serving daemon takes one terminal, `shutdown::exit`.** The first caller claims the exit
   and its code stands (`Signal::claim_exit`; a second waits for the process to end under it); it fires the child
   signal on a thread of its own and waits at most `FIRE_WAIT` (2 s) for the answer, logs a failed or unanswered fire,
   removes its Julia children's temporary folders waiting at most `FOLDERS_WAIT` (2 s) (`sidecars::child::remove_own`),
   then makes the process's one raw exit; when no thread can be started for the fire it exits without it, logging that.
   Codes: 0 (a finished close, an Ok main result), 1 (an error, a boot refusal, the backstop), 2 (a bad `agent-exec`
   recipe), 75 (the update restart), 78 (no config directory), 101 (a panic of the main future, turned into its code
   while the runtime still exists, `complete_main`); INT and TERM take the same fire and folders and then end the daemon
   by the signal itself with its default action (`shutdown::exit_by_signal`), so a service manager counts the stop as
   clean and a shell reads 130 and 143. Limit: a death by an uncatchable signal, an abort or a raw exit elsewhere is the
   guard's and the OS's, not the terminal's; a request is not a death (on Linux the guard ends what is left; on macOS
   only the controlled ends end children).
7. **INT and TERM reach the terminal whatever the main runtime does.** They are caught on a thread of their own with a
   runtime of its own, unblocked whatever mask the daemon inherited and checked to be deliverable; a failed installation
   refuses the boot (`signal_exit::install`). Unix only.
8. **The window never gives up an unanswered lease request while its sessions are meant to stay.** A `Keep` or a
   `Handover` still waiting on an unanswered request when its ack wait ends, or at a Ctrl+Q during that wait, stays open
   (`LeaveStep::Stay`; the frontend's `Leases::stay` decides under the lock `install` takes, so a grant that lands first
   gets the leave and its own ack wait), and the Windows supervisor keeps an unanswered request open (`Open-SotLease`).
   A window started with `--ephemeral`, `--capture` or `--no-lease` never leases (`lease_exempt`). The window's forced
   `deliver_queued` wait covers the writes already queued to all holders by one `Instant` deadline and does not wait for
   daemon acknowledgements.
9. **A lease outcome is named truthfully.** `Foreign` names a refused identity claim, not an absent backend or a proved
   different account; notice precedence is Undetermined, Unsupported, Foreign, then Unreached after granted, pending and
   exempt suppression (a host whose lease request is still out is pending); when the standings name no cause the notice
   names a leave that stayed, until the next leave (`lease_notice`).

## Owns
The window leases and `<state>/held.json` (`lease.rs`: `Leases`, `HeldRecord`, `read_record`, `write_or_delete`, the
ticker); the lease ops `fe.lease`, `fe.leaving`, `fe.notice_seen` (`lease::hold`); the start plan and its acting
(`startup.rs`, `lease::startup_plan`); the close and its backstop (`shutdown::run`); the daemon's terminal and its
bounds (`shutdown::exit`, `exit_by_signal`, `terminal`, `FIRE_WAIT`, `FOLDERS_WAIT`); `signal_exit::install`; the held
points of the daemon-lifetime harness (`test_gates.rs`, feature `daemon-lifetime-faults`); the bounds and exit codes in
`sot_protocol::ops::lease` (the window and the launcher share them: `launcher_bounds_match_ops`); the window's lease
half (`rust/frontend/src/lease.rs`, `lease_exit.rs`) and `scripts/sot-lease.ps1`. Test seams: `SOT_TEST_SHUTDOWN_BOUND_MS`
and `SOT_TEST_HANDOVER_BOUND_MS` (read once per process, unset in every deployment); `SOT_TEST_MAIN_OUTCOME`,
`SOT_TEST_GATES` and their kin are read only by a build with `daemon-lifetime-faults` (an installed binary reads none).

## Calls in
`sot_containment` (`process()`, `Signal::fire`, `claim_exit`, `reset_child_signal`, the guard's `require_one_thread`,
`install`, `guard_pid`), rows (`rows::end::{end_every, forget}`, `rows::resume_all`, `Workspaces::{claim_create,
close_gate_and_settle, list}`), sidecars (`child::remove_own`), the server (the accept loop's break on `Leases::gone`,
`handle_connection` handing a lease connection to `lease::hold`), `sot_platform` (`sot_state_dir`, `boot_identity`,
`write_durable`, `remove_durable`), `sot_protocol::ops::lease`, tokio, tracing, libc (the signal thread).

## Calls out
`startup::begin`, `lease::ticker`, `Leases::{gone, grant, depart, leave_again, begin_close, commit_update,
finish_shutdown, finish_cleanup, notice_seen}`, `lease::hold`, `shutdown::{run, exit, exit_by_signal}`,
`signal_exit::install`. The update restart reaches the lease only through `Leases::commit_update`.

## Does not
End a row (rows does), start, contain or kill a child (containment does), hold the daemon's single-instance lock or its
socket (the server and platform do), decide an update (distribution), or read a process table (a start's plan reads the
record and the boot id). The window's half does not decide for the daemon: it sends intents and reads outcomes.

## Platforms
Same on all three except: INT and TERM (Unix only); the lease's peer proof (Unix: peer credentials and pidfd; Windows:
the pipe client's pid and creation time, with a lenient empty boot); the Linux guard's mirror of the daemon's end (see
containment); `scripts/sot-lease.ps1` (Windows supervisor).

## Tests
One per guarantee. Paths are under `rust/backend/` (`src/lifecycle/` unless they start `src/` or `tests/`) or
`rust/frontend/src/` for the window's half. Integration tests need the built `sotd` and run in test containers.

| # | guarantee | test |
|---|---|---|
| 1 | grant only to the OS's peer | `lease_tests.rs`: `lease_claim_table`, `boots_match_table`, `grant_refused_when_record_cannot_be_written` |
| 2 | one record | `lease_tests.rs`: `record_roundtrip_and_empty_file_is_deleted`, `record_write_syncs_its_directory`, `record_survives_other_windows_keep`, `pending_deadline_is_persisted_once`; `tests/window_lease/lease.rs`: `closing_flag_spans_shutdown` |
| 3 | the last departure decides | `lease_tests.rs`: `departure_table`, `handover_table`, `latest_intent_wins_table`, `lease_generation_fences_removal`, `pending_expiry_shuts_down`; `tests/window_lease/lease.rs`: `last_one_out_two_windows`, `keep_is_open_ended`, `relaunch_handover_expires_then_shuts_down`, `lease_end_any_way_departs`, `malformed_lease_line_does_not_depart`, `lane_connection_close_does_not_shut_down`, `close_after_keep_on_one_lease_shuts_down` |
| 4 | the start plan | `lease_tests.rs`: `startup_plan_table`, `startup_plan_boot_row_compares_only_two_nonempty_boots`, `lease_during_startup_cleanup_keeps_the_record`; `startup_tests.rs`: `failed_forget_removal_is_kept_for_the_next_start`; `src/rows/ops/create_tests.rs` (the Cleanup's slug claim) |
| 5 | the close and its bound | `lease_tests.rs`: `not_ended_survives_restart_until_acked`, `shutdown_keeps_an_unacked_loaded_count`, `close_during_startup_cleanup_writes_closing`; `src/update_tests.rs`: `a_committed_update_is_not_undone_by_a_later_close`; `tests/window_lease/lease.rs`: `create_during_shutdown_refused`, `shutdown_bound_is_end_to_end`, `never_run_rows_count_as_ended`, `shutdown_ends_a_child_that_left_the_agents_process_group`; `tests/daemon_lifetime/outcomes.rs`: `a_close_that_outlasts_its_bound_ends_with_1_and_takes_the_ephemeral_trees` |
| 6 | one terminal | `shutdown_tests.rs`: `the_first_controlled_exit_decides_the_code`, `the_terminal_does_not_wait_for_a_stalled_creation`, `a_terminal_with_no_thread_to_fire_on_exits_without_the_fire`, `the_terminal_removes_the_folders_within_their_wait_before_it_exits`; `tests/daemon_lifetime/outcomes.rs`: `a_main_outcome_becomes_the_launched_status`, `early_commands_and_boot_refusals_keep_their_statuses`, `a_service_manager_counts_int_and_term_as_a_clean_stop` |
| 7 | INT and TERM | `signal_exit.rs`: `an_installation_failure_is_a_concrete_failure_exit`; `tests/daemon_lifetime/outcomes.rs`: `int_and_term_end_a_stalled_daemon_whatever_mask_it_inherited` |
| 8 | the window keeps an unanswered request | `lease_leave_tests.rs`: `leave_failure_table`, `second_close_during_handover_exits_zero_on_every_path`, `forced_exit_delivers_the_queued_close`; `lease_grant_tests.rs`: `an_unanswered_lease_is_named_and_held_until_its_answer`, `harness_never_leases`; `lease_delivery_tests.rs`: `frozen_worker_and_multiple_holders_share_one_deadline`, `held_book_does_not_extend_forced_delivery` |
| 9 | the notice | `lease_grant_tests.rs`: `no_lease_status_line`; `lease_leave_tests.rs`: `mixed_outcomes_show_the_sum`, `not_ended_status_line` |
