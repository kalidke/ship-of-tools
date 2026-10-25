# rust/backend/src/server: server (contract, drafted)

The daemon's one listener, and the op table behind it. A connection is admitted twice, each in one place: by the OS
peer read at accept (`admit_peer`) and by its first frame, which must be a hello the daemon accepts (`admit_hello`).
Until then nothing is served. The hello's role then says what the connection becomes: a `handoff` role leaves for a byte
pipe or a lease after its reply (`hand_off`), every other role is a control session at once and its loop its one
writer (`serve_control`). The server stays inside the `sotd` binary: it is the binary's one table of ops and the only
caller of every service's handlers. The services do not name its modules: a handler takes what it may use as arguments the
server builds from the connection's `Shared` (`dispatch.rs`), and the frame helpers they need (`HandlerOutput`, the
frame writes, the refusal frame, the byte pipe) are the wire's (`sot_protocol::reply`, `sot_protocol::pipe`).
Part of the server's subsystem, beside this folder: `main.rs` (sotd's entry), `clients*.rs` (the roster), `session.rs`
(the revision ring), `daemon_log/` and `paths.rs`.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on and the limit it has. The Tests section holds the test
of each.

1. **One daemon per state root, while `run` is running.** `take_daemon_lock` takes `<state>/daemon.lock`
   (`sot_platform::host::try_lock_daemon`) at the top of `run` and holds it until `run` returns or the process ends (a
   kill included), so a successor never reads the registry while a predecessor still serves. A busy lock with a daemon
   answering on the socket refuses the start at once; with nobody answering it waits up to `daemon_lock_wait` for the
   predecessor, then refuses (`lock_daemon`). On Unix "answering" means the listener returns a whole frame to a probe
   frame before one absolute deadline, `REPLY_BOUND` (`socket_answers`, `replies`): a listener that only completes the
   connect, as a copy of the old one held by a child between its fork and its exec does, is waited on, not read as a
   live daemon (queue item 21). Limits: the lock wait is checked between probes, so a turn can overrun it by the
   probe's own bounds (`CONNECT_BOUND` plus `REPLY_BOUND`); on Windows "answering" is a successful open of the pipe
   name, which proves no reply and can fail on a busy pipe; a daemon too slow to reply is waited on and the lock, not
   the probe, then ends the wait; a machine with no state root runs unfenced, with a warning; when `run` returns an
   error the lock drops before the terminal runs (`complete_main`, `lifecycle::shutdown::exit`), so a successor can
   take it while the terminal's cleanup still runs (a close that finishes exits without returning from `run`).
2. **A socket a live daemon answers on is not unlinked by a start.** `main.rs` calls `refuse_live_socket` before any
   start side effect, and `run_local` calls it before it removes a file at the socket path. The probe is `connect_own`
   within `CONNECT_BOUND`, then one frame and a reply (`replies`): a listener that replies refuses the start, another
   account's listener and a listener whose backlog is full (the connect gives up at the bound) refuse it too, and only a
   missing file, `ConnectionRefused`, or a listener that completes the connect and never replies (a dead predecessor's
   leftover, item 21) lets it start. A live daemon that is only slow to reply is fenced by its state lock, which a start
   takes before `run_local`. Limits: the fence is per state root, so two starts on different state roots and one socket
   path can both finish their probe before either unlinks, and one then unlinks the other's new socket; a start with no
   state root has no lock, and a live daemon too slow to reply loses its socket to it; macOS refuses a full backlog with
   `ConnectionRefused`, so there it reads as stale; on Windows a pipe leaves no file and the open of the name is the
   check.
3. **At accept, a connection of another account is dropped before a byte is read.** On Unix `admit_peer` (called from
   `run_local` through `dispatch_admitted`) drops a connection whose recorded effective uid is not this process's, or
   cannot be read (`same_account`). The uid is the account recorded at the connect, not the holder's current euid:
   credential transitions and descriptor transfers leave it unchanged. On Windows the pipe's owner-only descriptor
   decides (`bind_session`: protected, no inheritance on the descriptor's ACEs; the listener's handles are not
   inheritable either, interprocess's default). The account is the one fact the OS authenticates: the hello's `host`,
   `os_user` and `client_id` are the client's own declarations, checked for presence and for conflict (guarantee 4),
   never against the OS. The peer's process (pid and creation time) is read apart from
   the account and only a lease uses it: a peer of this account whose process the daemon cannot read (an
   administrator's ssh session on Windows; one that has ended on Linux) is served and cannot hold a lease.
4. **A connection is served only after a hello the daemon accepted.** A first frame that is not a hello request that
   parses and declares no blob, or that fails `admit_hello` (the protocol integer, the declared host and OS account
   present, the host's account record), gets one reply (`unauthenticated`, `protocol_mismatch`, `identity_missing` or
   `os_user_conflict`) and the close; nothing else is sent. A first frame that declares a blob is refused before a byte
   of the blob is read (`read_envelope`, `parse_first_frame`). Only `admit_hello` makes an `Admitted`, and
   `serve_control`, `hand_off` and `register_hello` take one, so no path from accept serves a hello that was not
   admitted. A host that has said hello as two OS accounts is refused for either until the daemon restarts
   (`Clients::admit_account`); connections already open are left alone. The accepted hello's reply (`handle_hello`)
   carries the session id and revision, `snapshot_pending` and any replay: a client with no session id gets no replay;
   one whose session id matches gets every ring entry newer than its `last_seen_revision`; a mismatched session id (a
   daemon restart) or a revision older than the ring's low watermark gets `snapshot_pending`, and refetches. Limits:
   the declared host and account are not authenticated (guarantee 3); a connection that sends no first frame within
   10 s, or a `handoff` connection no next frame within 10 s, is closed with nothing sent (`ADMISSION_READ_BOUND`);
   the ring is bounded, so a replay is a best effort that `snapshot_pending` backs.
5. **A connection has one writer.** A control session's loop owns its stream; an off-loop job has no reference to it and
   hands its reply to the loop over `OutTx`, which the loop writes with `write_reply`. An event is written by the same
   loop from its bus. Holds by construction (the stream is a `&mut` the loop alone holds; `spawn_job` takes only
   `OutTx`), so it has no test of its own.
6. **A control session's second hello closes it unanswered; a `handoff` connection serves one frame.** A `handoff`
   connection's next frame is `proxy.connect`, `lane.connect` or `fe.lease`, else `bad_request` and the close
   (`hand_off`); it is never in the roster and never serves a control op. Limit: only a request frame is read as a
   second hello (`serve_control` ignores other kinds).
7. **A handler's failure is one frame, and the connection stays, for the ops that reach `finish_dispatch`.** A handler
   `Err` is one `handler_error` frame (`finish_dispatch`); a job whose handler panics is answered `handler_error`
   naming the panic (`spawn_job`); an over-cap reply degrades to an `envelope_too_large` error frame for that request
   (`write_reply`). Only a failed write ends the connection, or a download's read error after its chunks are on the
   wire (`stream_file_download`). Exceptions: `file.download`, `pty.open` and `workspace.reauth` return before
   `finish_dispatch` (`dispatch.rs`): a reauth handler's error propagates and ends the connection, and a
   `file.download` error reply that outgrows the envelope (its message echoes the path) is written by `write_frame_to`
   directly (`files/transfer.rs:59`), so the typed size error ends the connection instead of becoming
   `envelope_too_large`.
8. **A peer that cannot drain a frame is dropped.** A write that makes no progress for 10 s plus 1 s per MiB of blob
   (`sot_protocol::reply::write_frame_to`, used by the loop's replies, events and the downloads' chunks) ends the
   connection. No end of a control session owes its peer bytes, nor does a lease whose reply could not be written, nor a
   `handoff` connection whose next frame failed or did not come: each closes without waiting for its peer to read
   (`Abandon`), so on Windows no such peer holds the closes behind it in interprocess's one linger thread. A refusal, a
   `bad_request`, a lease's last answer and a byte pipe's tail still wait until the peer has read them (Windows holds
   the pipe open until then). Limits: the deadline is per frame, not per reply: a download of many chunks to a slow
   peer that drains each one runs to its end; `sot_protocol::reply::reject` has no deadline, and the lease's
   malformed-line refusals use it (`lifecycle/lease.rs:592`, `:604`), so a lease peer that never reads can fill its
   buffer and block there.
9. **A ping is answered whatever the connection's role, and a silent pinger is reaped.** `handle_ping` answers every
   control connection's `ping` from the loop. The first `ping` arms a read deadline of 90 s that any later frame
   resets, and an armed connection silent for it is closed: `select_once` checks the deadline at the top of every turn,
   so a peer that sends nothing while events stream to it is reaped too (`ping_read_deadline`). Limits: a peer that
   never pings (too old to) is never reaped by this path; the check runs in the loop, so neither it nor the ping runs
   while an inline op holds the loop (guarantee 10); a `handoff` connection has no loop and no ping.
10. **An op runs off the connection's loop unless `INLINE` names it.** `place` puts every op in `INLINE` (each with its
    reason), in `POOLED` or, by default, a job with no permit. An inline op waits only on memory, the local disk, a
    capsule, registry, REPL or kernel step with its own bound (5 s, 1 s, 5 s, 10 s), `workspace.reauth` (its caller is a row's own client by convention: dispatch gives the handler no caller identity) on the row's start lock, or on its own writes (`file.download`'s chunks, each within guarantee
    8); the loop answers other requests after it. Limit: a local disk that stalls (a network filesystem) or a download
    to a slow peer holds the loop for that long, and with it the ping and the reaper. The three `POOLED` ops hold one
    of 4 permits per connection; a job that waits 10 s for one is answered with a timeout and never runs
    (`spawn_job`); a pooled job's workspace hint is resolved to the canonical id before it queues, or answered
    `unknown_workspace` with no job run (`canonicalize_workspace_id`). Job replies come back in the order the jobs
    finish, each under its own request id. A job runs to its end after its connection closes (`Jobs` detaches, never
    aborts), and only its reply is lost.
11. **An accept error is survived by one policy.** `survive_accept_error` follows `sot_platform::transport::accept_policy`, the
    policy the row socket's acceptors use too: a transient error accepts again, a resource error (EMFILE and kin)
    waits `ACCEPT_SLEEP` first, and only a dead listener ends the daemon with an error. Accepting stops at the
    deciding lease departure, before any row is touched (`run_local`'s loop breaks on `Leases::gone` and drops the
    listener before `shutdown::run`): that is lifecycle's guarantee 5, held there. Limit: the accept `select!` is not
    biased toward the departure, so a connection ready at the same moment can be accepted just after the decision.
12. **A control session's hello reply names its window's REPL runs still running.** `HelloRes::repl_running`, read from
    `CallerNumbers::running` after the connection's bus subscriptions, so the `done` of each run it names is written to
    the connection. Limits: the bus is lossy: a `done` that lag drops leaves the run open until the window's next drop;
    a `handoff` connection's reply names none; a window's runs are kept to 256 (`REMEMBERED`, `numbers.rs:71`), the
    oldest ended one forgotten first and then any other, so a run still running can be evicted and its `done` is then
    written under the daemon's number, not the window's; a window that numbers a run with a number it used before
    forgets the earlier run.
13. **A command with no window named goes to the window in use, with exceptions.** A preview-pane command goes to
    every window; `relaunch` goes to the active frontend alone, which needs a person's input within `ACTIVE_WINDOW`, else
    it goes nowhere; any other `fe.command`, a browser open included, goes by serial to the window in use
    (`ClientsSnapshot::window_for`, `in_use`); a `browser` REPL frame with `open` true goes as it came to the window in
    use and with `open: false` and no `fe` to every other connection, chosen once per frame by the first connection that
    writes it (`repl_frame_for`, `ReplFrameMsg::opener`). Limit: only `fe`-role connections with a name are windows;
    "in use" is the latest `fe.presence` stamp among them, and with none stamped the one connected last; with no
    named frontend attached a browser open goes nowhere.
    A page's command instead goes to the window that opened its site: `Clients::window_connection(owner)` selects
    that owner's newest live named `fe` connection, and `PageVerbs` publishes with its name and serial. The bus writer
    writes only to that serial, so another window and an older redial receive nothing. No live connection answers
    503 `window_gone` at the page server. A disconnect after selection remains the bus's best-effort delivery limit.
14. **A handler is given what it may use as arguments; the compiler enforces part of that.** The server's
    implementation modules (`conn`, `dispatch`, `events`, `hello`, `listen`, `reply`) are private to it, so no other
    module can name them. What remains reachable crate-wide is the entry's (`server::run`, `server::refuse_live_socket`)
    and the server-owned modules at the crate root (`clients*`, `session`, `daemon_log`, `paths`): the services take
    `Session` (the revision ring's handle) as an argument, and no longer take the roster: pages is given the window's key
    and the away windows as a closure, and `PageVerbs` with the states, handle and window lookups and fe-command bus;
    the updater the count of attached windows. That no service reaches the roster or
    the entry's items through the crate is a convention until the crates are cut, held by review, not by the compiler.
15. **The log is one file, bounded, and normally not copied to a regular-file stdout.** `sotd.log` follows
    `XDG_STATE_HOME` and rolls after a daemon write leaves it over 64 MiB: its last 64 MiB go to `sotd.log.1`,
    replacing an older one, and the log is truncated in place (`daemon_log`); stdout gets a copy unless it is a regular
    file, and always when the private log cannot be opened (`copies_to_stdout`). Limit: a failed roll is printed once
    and not retried until the log has grown by the cap again.
16. **An event reaches a connection as one frame, or is dropped when its bus lags.** Each bus item is one `evt` frame
    written by the connection's loop (`events.rs`); a connection that falls behind a bus loses what the bus dropped
    (the receiver's lag is logged and nothing is written for it), and a closed bus writes nothing. Nothing resends a
    dropped event: a window refetches on its next change or reconnect (guarantee 4's replay covers only the revision
    ring's events).

## Owns
- `<state>/daemon.lock` (`take_daemon_lock`, `lock_daemon`); the session socket or pipe (`run_local`: its private
  folder, `refuse_live_socket`, `bind_session` with the Windows owner-only descriptor and an inbound buffer of two
  envelopes, accept, the unlink at the end). On Windows the inbound buffer is a hint of two envelopes (`PIPE_INBOUND_BYTES`; the
  OS may adjust or clip it, so it is no strict ceiling): it lets a client that writes its hello and its request before it
  reads finish writing while a refusal waits for it, at the price of nonpaged pool charged while data waits in it.
- The seven buses `run` creates: repl frames, preview changes, workspace events, topology writes, agent messages (UI
  notifications), frontend commands and the monitor tick; their payloads belong to their owners. `run` also creates the
  hub's `Links` (messaging's), which a `link`-role connection attaches to.
- Each control session's task and state (`serve_control`): its roster guard, declared host, link end, active
  workspace, monitor flag, read deadline and its off-loop `JoinSet` and semaphore.
- The op table: `place` (the `INLINE` and `POOLED` lists), `dispatch` (the ops that use the loop's state or writer),
  `serve` (every other op, to its owner) and the unknown-op reply.
- sotd's entry (`main.rs`: argv, umask 077, boot refusals), the log tee (`daemon_log`), the client roster (`clients*.rs`)
  and the revision ring (`session.rs`).

## Calls in
`sot_protocol` (`Frame`, `codec`, `reply::{HandlerOutput, write_frame_to, write_frame_within, reject}`,
`pipe::pipe_bidirectional`, the op names and payloads, `PROTOCOL_VERSION`), `sot_platform` (`try_lock_daemon`,
`sot_state_dir`, `host_name`, `owner_protected_pipe_descriptor`, the peer challenge), `sot_platform::transport`
(`connect_own`, `accept_policy`), lifecycle (`startup::begin`, `lease::ticker`, `Leases::gone`, `lease::hold`,
`shutdown::run`), and every service's handler named in `serve` and `dispatch` of `dispatch.rs`, with
`handle_proxy_connect` and `handle_lane_connect` for the handoffs.

## Calls out
`run`, and `refuse_live_socket` for the early check in `main.rs`. Services reach nothing here; the roster (`Clients`,
`ClientGuard`) is handed to a handler that needs a window fact as a value or a closure (`handle_docs_open` takes the
window's key and `away_sites`, `spawn_periodic` the count of attached windows).
`run` builds one `PageVerbs` after the fe-command bus and roster exist, then calls `start_page_servers` before accepting
control connections. All page listeners share its separate body-reader, polling and action budgets. Its blocking closures use the checked fresh
comm registry read and call rows' `work_state::states` and
`handle::row_of_handle`; its window closure is `Clients::window_connection`.

## Does not
Implement an op beyond `hello`, `ping` and the handoffs (every other op is its owner's handler). Decide when anything
ends (lifecycle). Serve a page, a file or a preview (pages, files, sidecars). Hold a service's state. Start a child
(containment).

## Platforms
Same on all three except: the accept-time check (Unix: the recorded peer uid; Windows: the pipe's owner-only
descriptor, the process read apart), the pipe's inbound buffer and the drop-wait (`Abandon`) on Windows, the live
probe's reply (Unix only), and macOS's full backlog reading as stale (guarantee 2).

## Tests
One per guarantee. Paths are under `rust/backend/` (`src/server/` unless they start `src/` or `tests/`). Integration
tests need the built `sotd` and run in test containers.

| # | guarantee | test |
|---|---|---|
| 1 | one daemon per state root | `listen_tests.rs`: `the_lock_probe_reads_a_listener_that_never_replies_as_not_answering`, `the_start_refusal_reads_a_listener_that_never_replies_as_no_daemon`, `the_lock_probe_reads_a_replying_listener_as_answering`; `tests/window_lease/lock.rs`: `second_daemon_refuses_live`, `second_daemon_waits_for_lock`, `daemon_lock_timeout_exits_1` |
| 2 | a live socket is not unlinked by a start | `tests/live_socket.rs`: `a_start_against_a_listener_that_never_replies_binds_the_socket_and_answers`; `listen_tests.rs`: `the_start_probes_tell_missing_and_stale_from_live` (a replying listener refuses), `a_start_probe_of_a_full_backlog_returns_within_the_bound_and_refuses`; `tests/live_socket.rs`: `a_second_daemon_on_a_live_socket_refuses_and_the_first_still_answers` |
| 3 | accept-time admission | `listen_tests.rs`: `same_account_table`, `admit_peer_admits_this_process_with_its_pid_and_creation_time`, `a_connection_of_this_account_whose_process_has_ended_is_admitted_without_it`, `a_process_of_this_account_this_daemon_may_not_open_is_admitted_without_it`, `a_refused_os_peer_reaches_no_connection_handler`; Windows: `session_pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags`, `session_pipe_denies_a_token_without_the_owner_grant` |
| 4 | served only after an accepted hello | `hello_protocol_gate_tests.rs`: `accepts_matching_protocol`, `rejects_mismatched_protocol`; `tests/admission.rs`: `every_op_as_a_first_frame_is_refused`, `every_connection_kind_needs_an_accepted_hello`, `a_second_os_account_on_one_host_is_refused_loudly`, `alternating_accounts_on_one_computer_are_caught`, `a_hello_without_host_or_os_user_is_refused`, `a_refusal_names_no_account`, `a_refused_hello_closes_the_connection`, `a_first_frame_that_declares_a_blob_is_refused_before_its_blob_is_read`, `a_connection_that_says_nothing_is_closed`; Windows: `scripts/tests/test-local-daemon-pipe.ps1` |
| 5 | one writer | by construction |
| 6 | second hello, handoff | `tests/admission.rs`: `every_connection_kind_needs_an_accepted_hello` (its cases c and d) |
| 7 | a failure is one frame | `reply_job_panic_tests.rs`: `a_panicking_job_is_answered`; `tests/control_session.rs`: `control_session_replies_are_pinned`; `rust/protocol/src/reply_tests.rs`: `oversize_envelope_stays_downcastable_through_write_frame_within` |
| 8 | a peer that cannot drain | `rust/protocol/src/reply_tests.rs`: `write_frame_within_times_out_on_stuck_peer`, `write_timeout_is_not_confused_with_an_oversize_envelope`, `write_frame_within_succeeds_on_healthy_peer`, `write_deadline_scales_with_blob_size`; `tests/active_frontend.rs`: `a_stalled_download_is_closed_by_the_write_deadline` |
| 9 | ping and the reaper | `conn_select_once_wire_tests.rs`: `an_expired_read_deadline_reaps_a_connection_whose_event_arms_are_always_ready`; `tests/ping_reaper.rs`: `never_pinged_fe_connection_stays_connected_past_the_deadline`, `fe_connection_that_pings_once_then_goes_silent_is_reaped_within_the_deadline`, `pinging_fe_connection_stays_in_the_roster`, `a_cli_connection_that_pings_is_answered_and_reaped_when_silent`, `a_link_connection_that_pings_once_then_goes_silent_is_reaped`, `clean_exit_still_reaps_immediately` |
| 10 | off the loop | `dispatch_place_tests.rs`: `the_lists_are_disjoint_and_reasoned`; `reply_tests.rs`: `a_pooled_job_that_waits_ten_seconds_for_a_permit_is_answered_and_never_runs`; `tests/switch_latency/main.rs`: `slow_concept_read_does_not_delay_a_later_cheap_reply_on_the_same_connection`, `an_instant_op_is_answered_while_kernel_previews_hold_the_pool`, `a_ping_is_answered_while_a_quarto_render_is_held`, `a_held_render_runs_to_its_end_after_its_connection_closes`; `src/rows/unknown_workspace_tests.rs` (the pooled hint) |
| 11 | accept errors | `listen_tests.rs`: `the_session_loop_survives_every_transient_and_resource_accept_error`; the stop at departure is lifecycle's (`tests/window_lease/lease.rs`: `last_one_out_two_windows`) |
| 12 | the hello names running REPL runs | `tests/repl_windows.rs` (Linux) |
| 13 | a command with no window named; a page's own window | `events_tests.rs`: `a_page_for_every_frontend_opens_in_the_window_in_use`; `src/clients_show_tests.rs`: `a_preview_goes_to_every_window_and_every_other_command_to_one`, `the_window_in_use_is_the_one_with_the_latest_input_the_one_marked_active`; `tests/active_frontend.rs`: `a_preview_reaches_every_window`, `a_browser_open_reaches_only_the_window_in_use`, `an_announced_page_opens_in_one_window_the_one_in_use`; `tests/page_channel.rs`: `a_go_to_reaches_only_the_window_that_opened_the_page`, `a_page_whose_window_is_away_is_answered_503`; `src/clients_window_tests.rs`: `a_page_owner_resolves_only_to_its_newest_named_frontend` |
| 14 | a handler is given its arguments | the private submodules (compiler); the rest by convention, no test |
| 16 | an event is one frame or dropped | `conn_select_once_wire_tests.rs`: `one_item_is_one_evt_frame`, `a_lagged_bus_writes_nothing`, `a_closed_bus_writes_nothing` (per bus) |
| 15 | the log | `src/daemon_log/log_tests.rs`: `a_log_over_the_cap_rolls_after_the_first_write_and_a_held_writer_keeps_the_base`, `a_log_over_the_cap_rolls_within_a_run_and_every_writer_keeps_the_base`, `a_failing_roll_prints_once_and_is_not_retried_before_the_log_grows_by_the_cap`; `tests/daemon_stdout.rs`: `a_daemon_whose_stdout_is_a_file_writes_its_log_once` |

## Server queue
Seen at pass 5c, handled at the server's next pass; none is a defect seen in use.
- Windows' lock probe proves no reply (`listen.rs`, `socket_answers`): a successful pipe open shows neither a responsive
  daemon nor a protocol reply, and a busy pipe can make the open fail. There is no Unix fork-to-exec twin of item 21 there
  (interprocess's listener handles are not inheritable); the state lock and the first pipe instance flag still prevent
  a second holder. The inbound buffer size is a hint, and `scripts/tests/test-local-daemon-pipe.ps1` measures eight
  mostly idle connections only.
- A terminal-time fence: an error return from `run` drops the daemon lock before `complete_main` runs the terminal
  (`server/mod.rs` `_daemon_lock`, `main.rs` `complete_main`, `lifecycle/shutdown.rs`), so a successor can start while
  cleanup runs; holding the lock through the terminal is lifecycle's and the server's together.
- Two starts on different state roots and one socket path can race probe and unlink (`listen.rs`, `refuse_live_socket`
  then `run_local`'s `remove_file`); no test.
- `file.download`'s pre-stream error and `workspace.reauth`'s errors bypass `finish_dispatch` (`dispatch.rs`,
  `files/transfer.rs:59`, `rows/reauth/mod.rs:564`): an oversize error closes the connection. No test of the
  replacement frame and a following request.
- The lease's malformed-line refusals use the undeadlined `reject` in a loop that continues (`lifecycle/lease.rs:592`,
  `:604`): a lease peer that never reads can fill its buffer.
- The accept loop's select can take a ready connection when the departure is ready too (`listen.rs`, the `select!` in
  `run_local`): a connection can be accepted just after the decision.
- Trickling probe replies: only the unit tests of the helper cover the absolute deadline's edge; no test sends a byte
  at a time.
- `handle_connection`, `serve_control` and `run_local` carry the daemon's buses and handles as 17 to 19 positional
  parameters (`conn.rs:107`, `conn.rs:254`, `listen.rs:250`), copied by name three times in `mod.rs` and `listen.rs`; one
  context value built in `run` would delete the copies and become `Shared`'s base.
- `hand_off`'s `bad_request` and every `reject` write no deadline (`sot_protocol::reply::reject`, `conn.rs:248`); one
  small frame fits the buffer, so none was seen to park.
- A lease or lane connection that has left the loop is not in the roster, so nothing reaps one that never pings; the
  lease has its own ticker and the lane its pipe's end (`conn.rs:195`).
- The accept loop's stop before any row is touched (lifecycle's guarantee 5) has no test that connects after the
  departure; only the order of `run_local`'s tail holds it (`listen.rs`, the break, then `drop(listener)`).
- `INLINE` still holds ops that wait on a local disk (`file.write`, `file.delete`, `dir.create`, `concept.write`): on
  a network filesystem they hold the loop, and with it the ping (guarantee 10's limit).
