# rust/capsule: capsule (contract)

A row's terminal lives in a capsule: one process per row, the supervisor (`sot-capsule supervise`), runs the agent on a
real terminal, keeps its latest screen in a file and serves the row socket, and every viewer attaches through one client
over that socket. No append-only record of the terminal is kept (decision D2).
The crate (`sot-capsule`) depends on `sot-platform` (`../platform/CLAUDE.md`: locks, durable publication, the peer
challenge's OS steps, the local clients) and `sot-protocol` (wire: the frames), and on nothing else of ours; only
rows (`rust/backend/src/rows/`), topology's lane client (`rust/topology/src/lane_client.rs`) and the window's agent pane
(`rust/frontend/src/ui/agent_pane/`) name it, and the graph guard (`tests/dependents.rs`) holds the crate-level half of
that. Messaging reads one fact of it, the supervisor's argv (guarantee 13). It holds no state outside a row's state
folder (`supervisor.lock`, `screen`, `ended`) and its row socket.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on, its limit, and the test that holds it (the Tests table
lists where each lives). A test of a Windows or macOS arm runs on the hosted runner for its OS; the suites that start a
real supervisor (`tests/supervisor/`, `tests/row_management.rs`) run on Linux only and are cross-checked elsewhere.

1. **One authority per state folder.** The supervisor takes `<state_dir>/supervisor.lock` (platform's kernel lock,
   `fence::lock_supervisor`) before it binds the row socket or starts an agent, and holds it for its whole life. A held
   fence ends the contender with exit 70, nothing bound, and the first supervisor untouched. On Unix a supervisor born
   holding a claim adopts it instead (`birth_claim`: rows' durable parent took the same lock and passed the descriptor
   with `--claim-fd`/`--takeover-fd`; the fence is never free between claim and first act, and the supervisor answers
   the parent with its pid and start identity). One authority means one run: the supervisor runs one agent at a time on
   its main thread. Its first act is to make its state folder (on Windows nothing before it does). A run's start calls
   platform's `preflight_volume` (platform's guarantee 8) and a refusal fails the run and ends the supervisor terminal.
   Limits: a supervisor that fails to take over an inherited claim (a wrong or unlocked descriptor) also exits 70, which
   does not prove another live authority; the lock is the file system's, so the state root must be local (rows
   refuses a network root before a start); the capsule has no test of its own that makes the volume proof fail.
2. **One process per row, and the agent dies with it.** The agent is the supervisor's own child, started from its main
   thread. Linux: the kernel's parent-death signal (`PR_SET_PDEATHSIG`, armed first in the child, the parent then
   verified) fires when that thread ends, so a SIGKILL of the supervisor ends the agent even when it ignores SIGHUP.
   Unix: the supervisor holds the terminal's master, so its end also hangs up the agent's session. Windows: the ConPTY
   agent runs in an anonymous kill-on-close job with no breakaway (platform's guarantee 12), so the last handle's close
   ends its tree. Limits: macOS has no parent-death signal, so an agent that ignores SIGHUP can outlive a SIGKILLed
   supervisor (queue item MACOS-AGENT-WATCH, a kqueue watcher; the ordinary case is proved on macOS, the
   SIGHUP-ignoring case on Linux and Windows only); a descendant that leaves the session (Unix) or the job (Windows) is
   not the supervisor's to end.
3. **A row outlives its viewers and the daemon.** The supervisor serves its row socket for as long as it lives, across
   every run, and no connection's end, the daemon's end or a window's close ends it; its agent is its child, so it
   outlives them too. That the supervisor is outside the daemon's kill domain is rows' mechanism (the durable parent on
   Unix, breakaway on Windows). Limit: nothing in the capsule restarts a supervisor that died; rows' watchdog and boot
   resume do.
4. **One socket per row, this account only, nothing before an admitted hello, the row number the one gate.** The socket
   is `<runtime dir>/row-<h>.sock` (`h` is platform's `state_dir_hash`; a pipe `\\.\pipe\sot-row-<h>` on Windows). Unix:
   the runtime folder must be private, the socket is mode 0600, a peer of another uid is dropped at accept before a byte
   is read, and a bind replaces a socket file a dead supervisor left (the fence is held, so the file is its own) and
   refuses anything that is not a socket. Windows: every pipe instance carries platform's owner-only descriptor. A
   connection is served nothing before its `row.hello` is admitted, within one absolute deadline (5 s, Unix) with
   wire's envelope cap on the whole line. A client trusts a reply only after platform's challenge (`Proven`: the OS's
   record of the peer and the hello's `(pid, created)` agree); one that cannot prove the peer says `Unproven`, apart
   from a deadline that expired (`Timeout`). The hello's `row` number (`ROW_PROTOCOL`, now 1) is the one gate: no reader
   judges a frame's `v`, so a daemon update never strands a live row; the number rises only when a row op changes, and
   a client refuses another number by name, never by a hang. The reply carries the build id as a diagnostic that is never
   compared. Limits: a window attaching through the daemon's bridge binds the OS half to the daemon's report
   (`PeerAuthenticated`, steps 1 to 3 of topology's bridge); a Windows pipe has no read timeout, so a peer that never
   says hello holds two threads and a slot until it closes or the row ends; on Linux the challenge refuses a peer that
   started in the clock tick the client connects in, so a supervisor dialed within 10 ms of its own start can be
   `Unproven` (a later connection is proven).
5. **Attach shows the screen at once, then every byte in order, and a viewer behind is resynced.** `row.attach`
   is answered by the screen, the vt100 fork's checkpoint of the run's live parser (both grids, modes, a ring of at most
   200 rows, `CAPSULE_SCROLLBACK_ROWS`), cut only at a parser ground state (`RowRun::tick` and `output` serve waiting
   attaches, so a viewer joins after every byte the parser has seen was published and never inside an escape
   sequence), in blobs of at most 1 MiB with the last flagged; the output then follows in order, in blobs of at most 64
   KiB. No disk read and no new output is needed. A driver's attach applies its own size to the terminal first, so the
   snapshot is cut at that size; a failed OS resize is a refused attach, never a success. A viewer that falls behind is
   resynced, never cut for output: when 256 output frames wait for it they are all dropped, it leaves the live set, and
   the next ground state sends it a fresh screen (one that it has not begun to receive is replaced, so a viewer held
   back through many resyncs keeps one screen queued); its answers (a reply, a demotion) are never dropped. A viewer
   whose answers alone fill 64 frames, or that makes no progress writing for 5 s, is cut (Unix `shutdown(2)`, Windows
   `CancelIoEx`). The one client every viewer uses, `FeAttachClient` (the window as driver; the wake and the dialog
   tick as headless watchers), is one worker over `RowClient`: it waits until the row is viewed and its endpoint's link
   is up, dials once, restores the screen into its parser and follows the output, restoring any resync screen; a lost
   connection is redialed on platform's `Redial` (250 ms doubling to 4 s, started over only after a connection that
   lasted `STABLE`); it stops reading once 4 MiB of output sit unconsumed, so a viewer that does not pump is resynced by
   the row. A login (an ssh one has topology's 30 s) and the row's hello (10 s from the stream up) have budgets of their
   own. An episode ends only for a failure that cannot get better (another account's socket, a row number the client
   does not speak, a refusal by the daemon, access denied, a socket path that can never fit) or when the row's socket
   has been absent, though the daemon answers, for 120 s; anything else is retried. Limit: scrollback in a screen is
   at most 200 rows (a client accumulates up to 5,000 while it stays attached); the ConPTY fidelity of a mid-stream cut
   is proved on the hosted Windows run only.
6. **One driver, and eight real connections.** Input and resize are taken only from the connection attached as driver:
   a watcher's is refused by name and writes nothing; a second driver (an attach as driver, or `row.take`) demotes the
   first with `row.demoted`; a reattach as a watcher gives up the pen; a resize is accepted from 2x2 to 512x256. At
   most `MAX_CONNECTIONS` (8) connections are served and the ninth gets no slot; a connection has a reader and a writer
   thread, its slot returns only when both have ended, and `RowServer::shutdown` stops the acceptor, ends every
   connection's stream and joins every connection thread. The daemon's own calls share the eight with the viewers.
   Limits: two windows on one row trade the pen by typing; a Windows pipe stays open until its last owner lets go, so
   the dropped-watch slot test runs on Linux only.
7. **Input is written at most once, into the run it names, or reported.** `row.input` answers `written` once the bytes
   reached the terminal's master (ConPTY's input pipe), `busy` when 64 requests already wait or 16 typed writes wait
   behind the one input thread, `delivery unknown` when the loop did not answer in 2 s (it may still write; the client
   never sends it again), and `run ended` for input that names another run than the current one, input still queued
   when the run changes, and input that arrives once the run's loop has been left (a request arriving after the end is
   answered at once, until the next run starts). Every write to the terminal, typed input and the host's DA1 reply, goes
   through that one thread (`row/input.rs`), so the main loop never blocks on a terminal whose agent reads nothing; the
   run retires it within 2 s of its end. The loop drains at most 16 requests a pass and keeps at most one wake pending,
   so output, ticks and exit checks still run under a flood. The client tags each input with the run it was typed into
   and answers one left over when the connection comes back to another run `run ended`, counted, never written; input
   typed before the attach is held (8 KiB at most) and sent after it. A headless client takes the pen once per run: its
   Enter never takes it in a replacement run, and in the same run after another driver took it back it does not fight
   for it. The window says unknown delivery in its status line. Limits: written is not read, the agent may never read
   it; a thread that does not end within its bound is noted and left, holding the cloned terminal until the write
   fails.
8. **A restart shows the last screen.** `<state_dir>/screen` holds the vt100 checkpoint (format 2), handed to one writer
   thread at most every 5 s (`SCREEN_PACE`) while the screen changed, and at the agent's end; the writer writes it
   through platform's `write_durable`, and the slot between them keeps only the newest snapshot. A run starts its parser
   from the file, with a `[restarted <UTC time>]` line written below the retained content (on Windows both are scrolled
   into the history, because the ConPTY host repaints the visible screen); a file that does not restore (torn, another
   format version) is removed and the screen starts empty. Limits: the file can be 5 s old and a failing disk can leave
   it old for good; the end waits for the writer at most 2 s, and a writer that timed out at a run's end can still
   publish a stale snapshot over a later run's file (queue item SCREEN-WRITER-SPAN).
9. **No disk write stops output.** Output never waits on the screen file: a stalled disk blocks only the writer thread
   and a failing one is noted once per failing streak and tried again at the next pace.
10. **Status, end, reset and stop are bounded replies fenced by the run, and an end answered `ended` was recorded
    first.** The daemon's four calls are ops of the row socket, each answered in the calling connection's thread through
    the supervisor's `Shared`, every outcome a reply payload (an `error` reply is a protocol fault: an unknown op, a bad
    payload).
    - Phases: `starting`, `ready` (derived: the run's loop found the agent alive `READY_AFTER`, 2 s, after it started,
      so an agent that writes its error and exits sooner was never ready), `ending`, `ended_no_respawn` (the agent is
      gone because an end was asked for; only a reset or a stop remains) and `terminal` (sticky; no run is admitted).
      A run id is a UUIDv7 minted at each agent start; a resting supervisor reports the run it rests on, and one
      restarted with `--resume` over a requested end names no run.
    - `row.status` answers `{phase, run?}` at once. `row.end_run {run, reason}` names the run it ends: another run's
      id is `stale_run` and changes nothing; it is otherwise admitted only while that run is live and not already
      ending, and answers `ended` once the run has ended, `failed` (no run is live, an end is in progress, the
      supervisor is terminal, or the end could not be recorded) or `ending` after 25 s. `row.reset` is fenced by the
      phase alone: admitted only from `ended_no_respawn`, answering `reset` once the end record is replaced, `failed`
      otherwise or after 25 s. `row.stop` ends the agent and answers `stopping` as its connection's last reply; the
      process exits (0, or 69 when terminal) once that connection has ended or after 3 s, and its shutdown then ends
      every connection (a closed connection stays registered until both its threads have ended, so a peer that does
      not read cannot hold the exit).
    - `<state_dir>/ended` is the one record of how the last run ended (`requested`, the agent's uptime and exit). A
      requested end is written to it, durably, before `ended` is answered, even when the agent ended first: admission
      closes under the lock that reads whether an end was admitted (`Shared::close_run`), so an end admitted before is
      recorded and one after is answered `failed`. A requested end that cannot be recorded is not an end: the caller is
      told `failed`, the supervisor is terminal and starts no other agent. Each run's start clears `ended`; a reset
      publishes by a durable write of a record that asks for no end (a removal is not flushed on Windows), so a
      supervisor killed after the answer resumes into an agent on every system, and a publication that fails makes the
      supervisor terminal. A stop leaves no request, so the next start runs the agent again. A `--resume` over a
      requested end rests in `ended_no_respawn`; over no record or one that asks for none it starts an agent; a record
      that cannot be read is not a record of no end, so the supervisor is terminal before any run. A supervisor that goes
      terminal sends its running agent `Stop` first, with no request recorded.
    - No call carries an id and nothing is kept for a resubmit; a completion answer is kept under its own admission, so
      another run's admission never clears it. The daemon's client (`attach_client/supervisor_client.rs`) is bounded
      from the request (status 5 s, `end_run`, `reset` and `stop` 30 s; a dial adds up to 2 s before it and a recovery
      up to 10 s after). A client that lost a reply, or got `ending`, settles it by reading status from the same
      supervisor (the dial's `(pid, created)` must match): for `end_run` the run gone means ended (with the supervisor
      terminal, failed) and the run still there means unknown; for `reset` a run other than the one it rested on, or
      none, means done and the same run with the supervisor resting or terminal means not; nothing is asked a second
      time, and a replacement run's status never stands for the original. The dial checks the hello's row number alone,
      so a row of another number is `VersionSkew`, which rows reads as `foreign`.
    - Limit: a ninth dial gets no slot, which the daemon reads as one failed observation and never as an end.
11. **A watch tells every change in order, and the exit code before a controlled exit.** `row.watch` holds a
    connection as the daemon's view of the row and answers the status last told; each change of phase or run after it
    is one `row.phase` event carrying the new status, in the order the changes happened and never twice for one status.
    Every change is recorded with its value under `Shared`'s lock (a send down an unbounded channel, which never
    blocks) and one announcing thread tells the watchers from that record, so a change the run has already moved past
    is still told. `ready` is a clock, so the run's loop asks the management each tick to note it, only while something
    watches. A watch takes no pen, needs no attach and is never closed as idle. A controlled exit (0, 69, 75 or 76,
    whichever the supervisor returns) is told: the record is closed and delivered, the code committed, and each watcher
    is sent `row.exiting {code}` as its last frame and then the end of the stream; a watch admitted after the commit is
    answered its status and then told the same. The supervisor waits at most 1 s for the watchers' connections to be
    released before it drops the row socket, whose shutdown aborts what is still queued. A stream that ends with no
    `row.exiting` is a kill, a lost listener, a cut watcher or an exit before the bind and carries no code. The client
    side: `Watch::end` closes the connection and then waits for the supervisor's process, at most its bound, and says
    whether it ended, so a reader that acts on a told exit does not decide while the exiting supervisor still answers; a
    cancel handle gates every watch's publication (a cancel that lands during the dial leaves no connection behind) and a
    dropped watch withdraws its own publication and no newer one. Limit, the slow-watcher policy: reports and the exit
    are answers, never dropped for output's sake, but a watcher with 64 answers queued that it has not read is cut, and
    the cut aborts everything queued for it, the exit frame too; it reads the end with no code, and a redial's first
    reply is the status then.
12. **The exit code says how the supervisor ended: 0, 69, 70, 75 or 76.** 0 a stop; 69 (`EXIT_TERMINAL`) it is terminal
    (a failure other than a held fence, an end or reset record it could not write, an end record it could not read) or
    was terminal when stopped, or nobody stopped it within the 2 s grace; 70 (`EXIT_CONTENDED`) the fence was already
    held; 75 (`EXIT_AGENT_NOT_STARTED`) the agent could not be spawned, or the row socket could not be bound, before any
    agent started, or the listener was lost; 76 (`EXIT_AGENT_ENDED`) the agent started and ended by itself, with no stop
    or end asked for. A supervisor that is killed has no code. The supervisor never starts the agent a second time on
    its own: it rests only after a requested end, so rows' watchdog owns the one restart budget and decides when a row
    that exits 75 or 76 ends terminal. A row listener that dies (a permanent accept failure) does not leave an agent
    nobody can reach: the supervisor's watch, blocked on an event the acceptor sends as it dies (no clock), binds it once
    more; a second death or a failed bind ends the live run unrequested (`Command::Stop`, no request recorded) and the
    supervisor exits 75; a listener lost while it rests ends it the same way, freeing its fence, and no run is admitted
    once the listener is lost. Limits: on Unix an executable that passes preflight but fails at exec can read as an
    aborted child, so 76 for a program that never started; a viewer that dials in the gap between a listener's death and
    its rebind sees a refused connection and retries.
13. **The supervisor names its row in its argv; first-run-only tokens are kept on the first run only.** The command line
    is `sot-capsule supervise <state_dir> <--start|--resume> [--cols <n>] [--rows <n>] [--first-run-only <token>]...
    [--claim-fd <n> --takeover-fd <n>] -- <agent argv>`; messaging's ancestry walk binds a session to its row by a
    `sot-capsule` ancestor with an argument before `--` that ends in `/workspaces/<row>` (the state folder). A flag the
    usage does not name, the retired spellings included, is a usage error (exit 2) before the fence is taken. Tokens
    named by `--first-run-only` (the daemon passes a reauth's `--resume <id>`) stay in the agent's argv until an agent has
    started in this process and are stripped from every run after, so a first run that failed before running anything is
    retried with them; the supervisor knows nothing of the agent or of `--resume`. Limit: the daemon and `sot-capsule`
    ship in one pack and the daemon starts the binary of its own install, so no release pairs a daemon with a
    capsule that parses other words.

## Owns
The supervisor (`src/supervisor/`: the fence, the birth claim, `Shared` and the main thread's loop); a run (`src/capsule/`:
the terminal and its producers, the vt100 parser, the screen file `<state_dir>/screen`, the end record
`<state_dir>/ended`, the bounded join, the output budget); the row socket (`src/row/`: `RowServer`, `RowRun`,
`RowClient`, the input thread, the hello's exchange and payloads, the four management ops and the watch); the attach
client every viewer uses and the daemon's calls on a row's supervisor (`src/attach_client/`); the endpoint seam
(`src/endpoint.rs`: `Endpoint` and the two endpoints that resolve a row's socket path and hand back platform's
client). The state folder holds nothing else of the capsule's. The crate's `Error` is `Io`, `Json`, `Schema`, `State`
(a held lock and other refusals the caller acts on), `VersionSkew`, `Transport` and, on Windows, the ConPTY and job
failures; `storage_exhaustion` reads it through platform's native storage table.

## Calls in
The OS (pty and process groups, `prctl`, ConPTY and job objects, sockets and named pipes, the kernel's peer records); the
vt100 fork (`../vt100/`: the parser and its checkpoint); platform: `host::{lock_supervisor, lock_supervisor_for_handover,
HandoverLock, write_durable, remove_durable, preflight_volume, owner_protected_pipe_descriptor, harden_own_stdio, Redial,
native_storage_code}`, `host::state_dir::{state_dir_hash, runtime_dir, current_uid}`, `host::fd::{openpty_cloexec,
fork_guard, inherit_none}`, `host::winjob::AnonymousJob`, `host::process_tree::Launch` (the native birth launcher),
`identity::{challenge::*, exchange::IdentityExchange, deadline::run_with_deadline}` and the per-OS challenge steps,
`transport::{Client, TransportError, connect_own}` and the two clients; wire: `Frame`, `Kind` and the blocking codec with
its envelope cap; serde_json, uuid, tracing, libc (Unix), interprocess (Windows).

## Calls out
Rows: `supervisor::{supervise, SuperviseConfig, StartMode, EXIT_*}` and the `sot-capsule supervise` command line,
`supervisor::fence::{lock_supervisor, supervisor_lock_path}`, `supervisor::birth_claim::{BirthClaim, read_takeover}`,
`capsule::{read_ended, mark_end_requested, Ended}`, `attach_client::supervisor_client::{query_status, end_run, reset,
stop, watch, CancelHandle, StatusReport}`, `SupervisorPhase`, `storage_exhaustion`, `row::row_socket_path`. The window's
agent pane and rows' headless typing (`rows/run/headless.rs`, for the wake and the dialog tick):
`attach_client::client::{FeAttachClient, InputOutcome, TAKE_QUEUE_CAP}`, `endpoint::{Endpoint, PlatformEndpoint}`.
Topology's lane client implements `endpoint::Endpoint` (`DaemonLaneEndpoint`) so a remote attach rides one login.

## Does not
Know the daemon's registry of rows, mail or windows. Decide when a row starts, restarts or ends (rows' watchdog and end
API ask; the supervisor only answers). Read the agent's words (`--resume` is an opaque token). Keep any record of the
terminal beyond the latest screen and the end record. Write the launch recipe (agents') or place the supervisor
outside the daemon's kill domain (rows'). Judge a frame's version: the row number is the one gate.

## Platforms
Linux, macOS and Windows build; no fourth. Unix: a pty (`openpty`, a new session and process group, descriptors born
close-on-exec) and a Unix socket; Linux adds the parent-death signal and the per-thread parent it follows; macOS adds
the kernel's `pidversion` as a process's start identity and has no parent-death signal. Windows: ConPTY in an anonymous
kill-on-close job and a named pipe with the owner-only descriptor on every instance; a state folder is made by the
supervisor itself. The helper binaries `sot-pty-helper` and `sot-conpty-helper` are test fixtures and never ship.

## Tests
One row per guarantee. Paths are under `rust/capsule/` (`src/` for unit tests, `tests/` for integration) unless they
start `rust/` or name the backend; the integration binaries are named by their file or folder.

| # | guarantee | test |
|---|---|---|
| 1 | one authority | `src/supervisor/fence_tests.rs` (5: the file name pinned, a second holder refused, the same-process and cross-process races grant one); `src/supervisor/birth_claim_tests.rs::a_takeover_record_round_trips_and_a_torn_or_foreign_one_is_refused`; `tests/supervisor/guards.rs::a_second_supervisor_on_a_held_fence_exits_70_and_leaves_the_first_serving_its_socket` (a guard); `tests/supervisor/spawn.rs::a_supervisor_creates_its_own_state_folder`; backend `capsule_workspaces::spawn::capsule_supervisor_spawn_survives_fence_contention_without_marking_terminal` |
| 2 | the agent dies with it | `tests/supervisor/one_process.rs::{one_process_per_row, the_agent_dies_with_its_supervisor}`; `tests/macos_kernel_facts/kqueue.rs`; `tests/macos_kernel_facts/pty_hangup.rs`; `src/capsule/producer/pty/drop_and_domain_tests.rs::drop_kills_surviving_descendants_when_the_leader_already_exited`; `tests/capsule/windows_only.rs::the_agent_job_refuses_a_breakaway`; `tests/conpty.rs::containment_kills_child_and_grandchild` |
| 3 | outlives viewers and daemon | `tests/supervisor/guards.rs::a_supervisor_and_its_agent_outlive_every_connection` (a guard); backend `capsule_workspaces::platform::capsule_supervisor_survives_a_real_user_service_stop` |
| 4 | one socket, this account, hello, row number | `tests/row.rs`: `a_peer_of_another_uid_is_dropped_at_accept`, `a_peer_that_trickles_bytes_before_hello_is_dropped_at_the_deadline`, `a_row_that_declares_an_older_wire_integer_and_the_current_row_number_still_attaches`, `a_row_with_a_newer_row_number_is_refused_by_name`, `a_peer_that_cannot_be_proven_is_not_reported_as_a_timeout`, `a_row_socket_path_past_sun_path_is_refused_by_name`; `tests/row_limits.rs::a_bind_replaces_a_socket_file_a_dead_supervisor_left_and_nothing_else` (a guard); `tests/row_pipe.rs::every_pipe_instance_is_owner_only` (hosted Windows); `src/row/proto_tests.rs` (13); `tests/row_management.rs::a_daemon_dial_checks_the_row_number_not_the_wire_integer`; the challenge against a real `RowServer`: `tests/challenge_unix.rs`, `tests/challenge_macos.rs`, `tests/pipe_win/challenge.rs` |
| 5 | attach shows the screen | `tests/row.rs::attach_shows_the_screen_at_once`; `tests/fe_attach.rs::attach_shows_the_screen_at_once`; `src/row/run_tests.rs::{a_viewer_joins_only_at_a_ground_state_and_is_then_sent_the_output_in_order, a_viewer_that_falls_behind_is_resynced_with_one_screen_not_cut}` (the first a guard); `tests/row_flood.rs::a_driver_that_pumps_slowly_under_a_flood_keeps_its_connection_and_gets_every_reply`; `tests/row.rs::{a_drivers_attach_gets_its_own_geometry_in_the_snapshot, a_failed_os_resize_is_a_failure}`; `tests/fe_attach.rs`: `a_redial_waits_on_the_doubling_pace`, `a_login_that_takes_twelve_seconds_still_attaches`, `a_row_socket_path_too_long_ends_the_attach_with_a_named_error`; `src/attach_client/client_tests.rs` (5); `tests/fe_client/headless.rs::headless_attach_adopts_capsule_geometry_and_types_without_resizing`; `tests/capsule/windows_only.rs::attach_mid_stream_snapshot_reproduces_reference_screen` (hosted Windows) |
| 6 | one driver, eight connections | `tests/row.rs`: `a_watcher_cannot_type`, `a_new_driver_demotes_the_old`, `reattaching_as_a_watcher_gives_up_the_driver`; `tests/capsule/row_run.rs::a_drivers_input_reaches_the_terminal_and_a_watchers_is_refused`; `tests/row_limits.rs`: `a_ninth_connection_is_refused_while_eight_are_held`, `an_evicted_viewer_gives_its_slot_back_only_when_its_threads_have_ended`, `after_shutdown_no_connection_thread_is_left`; `src/row/run_tests.rs::a_closed_connection_stays_reachable_until_its_threads_have_ended`; `tests/fe_attach.rs::{a_demoted_window_types_again_after_a_wake, a_demotion_during_attach_leaves_a_watcher_whose_next_key_takes_the_pen}` |
| 7 | input once or unknown | `tests/row.rs`: `input_that_times_out_is_delivery_unknown`, `a_request_left_over_when_the_run_changes_is_answered_run_ended`; `tests/row_management.rs::a_row_refuses_input_meant_for_another_run`; `tests/fe_attach.rs`: `input_queued_for_an_earlier_run_is_never_written_into_the_next`, `a_headless_client_takes_the_pen_once_per_run` (a guard); `src/row/run_tests.rs::{a_full_request_queue_refuses_input_as_busy, a_wake_is_coalesced_and_the_drain_is_under_a_quota}`; `src/row/input_tests.rs` (4); `tests/row_switch.rs::a_paste_the_agent_has_not_read_does_not_stop_its_output`; `tests/capsule/row_run.rs::input_sent_while_a_requested_end_is_in_progress_is_answered_run_ended`; `tests/fe_client/headless.rs::headless_write_while_a_client_is_driving_demotes_it_without_duplicating_input` |
| 8 | a restart shows the last screen | `src/capsule/screen_tests.rs` (3); `tests/capsule/main.rs::e2e_runs_and_leaves_its_ending_and_its_screen`; `tests/capsule/screen_io.rs::a_run_over_a_saved_screen_shows_it_above_a_restart_line` (a guard) |
| 9 | no write stops output | `tests/capsule/screen_io.rs::{a_stalled_screen_write_never_stalls_output, a_full_volume_never_stops_output}` |
| 10 | status, end, reset, stop | `tests/row_management.rs` (the daemon's calls on the row socket, over a real supervisor and a stand-in that drops replies: `an_end_run_naming_another_run_is_refused_with_no_change`, `a_reset_after_end_run_is_admitted`, `a_resting_supervisor_reports_the_run_it_rests_on`, `a_stop_is_answered_before_the_supervisor_exits`, `an_end_run_whose_reply_is_lost_after_the_run_ended_reports_ended`, `a_lost_end_reply_is_settled_by_the_run_a_rest_or_a_terminal_status_names`, `a_reset_whose_reply_is_lost_is_settled_by_the_run_status_names`, `a_peer_that_never_reads_its_stop_reply_cannot_hold_the_shutdown`); `tests/supervisor/authority.rs` (5: `reset_is_refused_while_a_run_is_live`, `reset_from_ended_no_respawn_starts_an_agent_in_a_new_run`, `a_reset_right_after_a_reset_is_refused_while_its_run_is_live`, `a_reset_answered_before_a_kill_is_not_undone_by_the_resume`, `a_reset_whose_publication_fails_leaves_the_supervisor_terminal_and_starts_no_run`); `tests/supervisor/lifecycle.rs` (2); `tests/supervisor/spawn.rs`: `start_reaches_ready_promptly`, `a_watch_reports_ready_only_after_the_agent_lived_two_seconds`, `an_agent_that_exits_before_two_seconds_is_never_reported_ready`, `a_resume_with_nothing_to_resume_starts_the_agent`, `a_run_asked_to_end_rests_after_a_supervisor_restart`, `an_unreadable_end_record_starts_no_agent_on_resume`, `a_stop_leaves_no_end_request_so_the_next_start_runs_the_agent_again`, `an_end_run_the_agent_beat_is_still_a_requested_end_after_a_stop_and_a_resume`, `a_requested_end_that_cannot_be_recorded_is_not_answered_ended`, `an_end_run_asked_after_the_agent_ended_by_itself_is_never_answered_ended_unrecorded`; `src/supervisor/authority_tests.rs` (9); `src/capsule/ended_tests.rs` (2); `tests/row_pipe.rs::a_peer_that_never_reads_its_stop_reply_cannot_hold_the_shutdown` (hosted Windows) |
| 11 | the watch | `src/row/run_tests.rs`: `a_watch_is_told_each_change_once_and_in_order`, `a_tick_asks_nothing_when_nothing_watches`, `exiting_is_sent_before_the_shutdown_and_waits_for_the_watchers`, `exiting_gives_up_on_a_watcher_that_does_not_drain`, `a_watch_admitted_during_the_exit_is_told_the_code`; `tests/row_management.rs`: `a_watch_reports_each_phase_change_in_order`, `a_watch_reports_exiting_with_the_code_before_a_controlled_end`, `a_watch_ends_by_waiting_for_the_supervisors_process`, `a_watch_end_withdraws_its_publication_before_waiting`, `a_cancelled_watch_never_publishes_a_connection`, `a_transition_is_told_though_the_run_ends_before_its_announcement`, `a_dropped_watch_gives_its_slot_back`; `src/supervisor/authority_tests.rs::{every_change_is_recorded_in_order_with_its_value, a_poll_records_a_clock_change_once}` |
| 12 | the exit code | `tests/supervisor/spawn.rs`: `an_agent_that_ends_by_itself_ends_the_supervisor_with_76`, `an_agent_that_cannot_be_started_ends_the_supervisor_with_75`; `tests/supervisor/lifecycle.rs::full_lifecycle_status_end_run_and_clean_exit` (0); `tests/supervisor/guards.rs` (70); `tests/supervisor/authority.rs::a_reset_whose_publication_fails_leaves_the_supervisor_terminal_and_starts_no_run` (69); `tests/row_switch.rs::a_row_socket_that_cannot_be_bound_exits_75_before_the_agent_starts`; `tests/row_supervisor.rs`: `a_supervisor_serves_its_row_socket_and_the_agent_hears_a_driver`, `a_dead_row_listener_is_bound_again_once`, `a_row_listener_that_keeps_dying_ends_the_supervisor_with_75`, `a_listener_lost_while_the_supervisor_rests_ends_it_with_75`, `the_row_listeners_watch_waits_on_an_event`; `src/supervisor/authority_tests.rs`: `a_terminal_supervisor_ends_its_running_agent`, `a_lost_row_listener_wakes_a_resting_supervisor`, `a_run_is_not_admitted_once_the_row_listener_is_lost`, `an_end_admitted_after_the_run_is_closed_is_refused_and_one_before_is_reported`; backend `capsule_workspaces::spawn::a_row_socket_that_cannot_be_bound_ends_the_row_terminal_through_the_budget` |
| 13 | argv and first-run-only | `tests/supervisor/argv.rs::a_retired_flag_is_a_usage_error`; `tests/supervisor/spawn.rs`: `first_run_only_keeps_a_token_on_the_first_leg_and_strips_it_after_a_producer_ran`, `a_reauths_reset_leg_keeps_its_first_run_only_token_and_its_respawn_drops_it`; backend `rows/spawn/detach_start_mode_tests.rs` (`only_a_resume_by_id_is_first_run_only`, `mode_flag_matches_the_sot_capsule_cli`) and `comm/identity/layers_tests.rs::a_capsule_naming_a_voyage_folder_is_not_the_rows` |

Also held by tests that belong to no one guarantee: `tests/dependents.rs` (the crate graph: only backend, frontend and
topology depend on the capsule, and it depends on no crate of ours but platform and wire; the crate is `sot-capsule` in
`rust/capsule/`); `tests/capsule/` for a run's own mechanism (`output_end.rs`: a producer's output end before its exit
is explained, recorded or fatal; `unix_only.rs`: spawn failure, signal death, resize, a slave reopen; `no_record.rs`: a
run writes no record besides its two files) and `src/capsule/{join,output}_tests.rs`, `producer/` tests (the DA1
handshake, close-on-exec pty births, an executable that resolves); platform's transport clients against the row
acceptor (`tests/socket_unix/`, `tests/pipe_win/`, `tests/macos_kernel_facts/`: platform's guarantees 6 and 13, kept here
because they need a real `RowServer` or the `sot-capsule` binary, `../platform/CLAUDE.md`).

Gaps, stated: no capsule test makes `preflight_volume` refuse a run's start (guarantee 1); the Windows arms of
guarantees 4, 5, 6 and 10 are proved by the hosted Windows run alone, and the Linux-only suites above are cross-checked
(not run) on the other systems.

## Files
- `Cargo.toml`: the manifest. `test-support` turns on platform's (`test_log`, `test_copy`, `test_scan`, `test_isolated`,
  `test_wait`, `test_exec` and, on Unix, `test_foreign`) and the capsule's own test hooks (the screen writer's); the
  backend, frontend, topology and updater turn platform's on in their dev-dependencies. `native-barrier` forwards
  platform's, and the supervisor holds its barriers under it (`sot_platform::test_barrier::hold`).
- `build.rs`: stamps the build id (`SOT_CAPSULE_BUILD_SHA`, `sot_capsule::BUILD_ID`) from the full git sha, or
  `SOT_BUILD_ID`; the row hello carries it as `build`.
- `src/lib.rs`: the module tree, `BUILD_ID`, `SupervisorPhase`, `Error` and `Result`, the `storage_exhaustion` re-export.
- `src/supervisor/`: `mod.rs` (the process, its exit codes and its main thread's loop), `authority.rs` (`Shared`: the
  state the management calls and the main thread share), `fence.rs`, `birth_claim.rs`.
- `src/capsule/`: `writer_loop/` (`run`: start, the main loop, teardown), `producer/` (`pty/` on Unix, `conpty/` on
  Windows, the host handshake), `screen.rs`, `ended.rs`, `output.rs` (the output budget), `join.rs`.
- `src/row/`: `server.rs` (acceptor and connection threads), `run.rs` (`RowRun`: the run's fan-out, request queue and
  watch record), `client.rs` (`RowClient`), `input.rs` (the one terminal writer), `manage.rs` (the `Management` trait),
  `proto.rs` (the ops, payloads and `RowHelloExchange`).
- `src/attach_client/`: `client.rs` (`FeAttachClient`), `attach.rs` (the worker), `supervisor_client.rs` (the daemon's
  calls and the watch).
- `src/endpoint.rs`: the endpoint seam. `src/storage.rs`: the storage reading of platform's table.
- `src/bin/`: `sot-capsule` (`supervise`, `build-id`) and the two test-fixture helpers.
- `tests/`: integration and whole-process tests, by target (`capsule/`, `supervisor/`, `fe_client/`, `row*.rs`,
  `fe_attach.rs`, `socket_unix/`, `pipe_win/`, `macos_kernel_facts/`, the challenge suites, `dependents.rs`, `conpty.rs`,
  `isolation_guards.rs`, `test_body_fixture.rs`, `support/`). Start reading at `src/supervisor/mod.rs`, then
  `src/capsule/writer_loop/mod.rs` (`run`), `src/row/` for a viewer and `src/attach_client/supervisor_client.rs` with
  `src/supervisor/authority.rs` for the daemon's calls.

## Rules
- Modules are `pub` where integration tests or other crates reach them: those see only pub items.
- A change to a wire tag, magic or limit, to the row number, or to an exit code is an interface change with other
  processes and versions.
- The test support (`test_exec`, `test_isolated`, `test_wait`, `test_log`, `test_scan`, `test_copy`) and its rules are
  platform's (`../platform/CLAUDE.md`, Rules). No test here proves what code does by reading source text.
- A `SOT_TEST_*` seam read by a production binary is inert unless named: `SOT_TEST_END_MARK_FAILS`,
  `SOT_TEST_SUPERVISOR_BOOT_FAILS`, `SOT_TEST_RESET_HOLD_MS`, `SOT_TEST_RESET_CLEAR_FAILS`, `SOT_TEST_EXIT_HOLD_MS`,
  `SOT_TEST_RUN_OVER_HOLD_MS`, `SOT_TEST_RECOVERY_DELAY_MS`, `SOT_TEST_END_RUN_HOLD_MS`, `SOT_TEST_ANNOUNCE_HOLD_MS`,
  `SOT_TEST_ROW_LISTENER_DIES` and `SOT_TEST_ROW_LISTENER_DIES_ON`.
- Every supervisor a test starts runs under an isolated `SOT_RUNTIME_DIR` and inside `CapsuleGuard`
  (`tests/support/capsule_guard.rs`), and kills go through a pidfd or a process handle.
- Another crate can reach the processes this crate starts (the agent) through its public items: `capsule::run`,
  `capsule::producer::Producer::spawn`, `capsule::producer::conpty::ConptySpawn::spawn` and `supervisor::supervise`.
  `rust/clippy.toml` lists none of them, so its process-spawn lint does not see a call from another crate; today none
  calls them but `src/bin/sot-capsule.rs`.
