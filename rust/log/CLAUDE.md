# rust/log: capsule (charter)

## Idea
A row's terminal lives in a capsule: one process per row, the supervisor, runs the agent on a real terminal, keeps
its latest screen and serves the row socket, and every viewer attaches through one client over it. No
append-only record of the terminal is kept (decision D2, passes 6a and 6b). The crate is `sot-log`; it rests on
`sot-platform` (`../platform/CLAUDE.md`), which holds the machine facts, durable publication, locks and the peer
challenge's OS steps, and re-exports none of it. The capsule's contract is written in pass 6d; this page says what is
true after the merge of the leg into the supervisor.

## Owns
- Limit: a screen writer that timed out at a run's end (`ScreenFile::finish`) can still publish a stale snapshot over a
  later run's `screen` (queue item SCREEN-WRITER-SPAN: one writer whose ownership spans runs).
- The supervisor, `sot-capsule supervise`: `supervisor.lock` and the daemon's four management calls
  (`src/supervisor/`). It runs the agent itself, from its main thread (`capsule::run`); a connection's
  thread of the row socket answers a management call through `Shared`, and a third thread watches the row listener.
- The agent's run: the terminal, the vt100 parser, the screen file
  `<state_dir>/screen` and `<state_dir>/ended` (how the last run ended: whether an end was asked for, the producer's
  uptime and exit; `src/capsule/ended.rs`). Each run's start clears `ended`, and a reset replaces it with a record that asks for no end. The state folder holds
  nothing else of the capsule's: no segments, no blobs, no voyage folder, no pointer.
- The transports' client half (`src/lane/`): `SocketClient` and `PipeClient`, the bounded connect they come from, the
  client seam (`Client`, `Endpoint`) and the one local connect, `connect_own`. Every server is the row socket's: the
  lane servers, the management lane SOM0, the supervisor lane SOSV and the attach lane SOA0 are gone (passes 6c-1a3 and
  6c-1b2).
- The attach client every viewer uses (`src/attach_client/client.rs` over the worker in `attach.rs`, both on the row
  socket), and the daemon's calls on a row's supervisor (`supervisor_client.rs`, over `RowClient`).
- The row socket (`src/row/`), the way in for every viewer and for the daemon: a blocking acceptor, `RowClient` and
  the input thread, in wire's frames, and the four management ops (`row.status`, `row.end_run`, `row.reset`,
  `row.stop`) a row hands to the `Management` its supervisor installs. Pass 6c-1a2 switched the window, the wake and
  the bridge to it, and pass 6c-1b1 the daemon's calls; 6c-1c adds the watch.

## Promises
- One process per row: the agent is the supervisor's own child. The supervisor takes `supervisor.lock` (or adopts the
  claim it was born holding) before it binds the row socket or starts the agent, and a held fence exits 70 with nothing bound.
- The agent dies with its supervisor. Linux: the agent forks from the supervisor's main thread, which lives as long as
  the process, and the kernel's parent-death signal (`PR_SET_PDEATHSIG`) fires when it ends, so a SIGKILL of the
  supervisor ends the agent (`tests/supervisor/one_process.rs`: the agent ignores SIGHUP, so only that signal can).
  Unix: the supervisor also holds the terminal's master, so its end hangs up the agent's session. Windows: the ConPTY
  agent runs in a kill-on-close job, so the last handle's close ends its tree. Limits: macOS has no parent-death
  signal, so only the hangup ends the agent there: an agent that ignores SIGHUP can outlive a SIGKILLed supervisor
  (queue item MACOS-AGENT-WATCH: a kqueue `NOTE_EXIT` watcher); the ordinary case, an agent that does not ignore SIGHUP,
  is tested on macOS (`tests/macos_kernel_facts/kqueue.rs`), the SIGHUP-ignoring case on Linux and Windows only; a
  descendant that leaves the session survives on Unix, and one in another job (Windows)
  or session (Unix) is not the supervisor's to end.
- Phases: `starting` (until the agent has written or lived two seconds), `ready`, `ending`, `ended_no_respawn` (the
  agent is gone because an end was asked for; only a reset or a stop remains) and `terminal` (sticky: run admission refuses a terminal supervisor, so none begins and no agent starts). The supervisor
  rests only after a requested end: an agent that cannot be started or ends by itself ends the supervisor with 75 or
  76, so the daemon's watchdog is the one restart budget (rows' contract). A `--resume` over a run that was asked to
  end (`<state_dir>/ended` with `requested`) rests in `ended_no_respawn`; a `--resume` with no `ended`, or one that
  records no request, starts a fresh agent like a `--start`, so a folder whose supervisor died before any run resumes
  into an agent; a record that cannot be read (not the record's JSON) is not a record of no end, so the supervisor is
  terminal before any run (`tests/supervisor/spawn.rs::an_unreadable_end_record_starts_no_agent_on_resume`). A
  supervisor names only the runs it began: one that has run an agent reports, while resting, the run whose end it rests
  on (`row.status`'s `run`), held in memory, and one restarted with `--resume` that rests on a requested end names no
  run, so the daemon's cell latches on no run and releases on the first (`tests/supervisor/spawn.rs::a_run_asked_to_end_rests_after_a_supervisor_restart`).
  A stop is not an end of the run: it leaves no request, so the next start runs the agent again.
- The daemon's four calls are ops of the row socket, each answered in the calling connection's own thread, every outcome a
  reply payload (an `error` reply is kept for a protocol fault: an unknown op, a bad payload): `row.status` answers at
  once `{phase, run?}`; `row.end_run {run, reason}` is fenced by the run, so another run's id is `stale_run`
  and changes nothing, and is otherwise admitted only while that run is live and not already ending, answering `ended`
  once the run has ended (so a `reset` sent after the answer is admitted), `failed` (no run is live, an end is in
  progress, the supervisor is terminal, or the end could not be recorded), or `ending` when 25 s have passed; `row.reset`
  is fenced by the phase alone, admitted only from `ended_no_respawn` and answering `reset` once the end record is
  replaced (by a durable write of a record whose `requested` is false, before the answer: that write is the reset's
  publication, and a removal would not be flushed on Windows, so a supervisor killed after the answer resumes into an
  agent on every system, `tests/supervisor/authority.rs::a_reset_answered_before_a_kill_is_not_undone_by_the_resume`; a
  publication that fails makes the supervisor terminal, answers `failed`, and starts no run,
  `a_reset_whose_publication_fails_leaves_the_supervisor_terminal_and_starts_no_run`); `row.stop` ends the agent and answers `stopping` as its connection's last reply, and the
  process exits (0, or 69 when it was terminal) once that connection has ended, or after 3 s, and its shutdown then ends every
  connection: a closed connection stays registered until both of its threads have ended, so `close_all` reaches a writer
  that a peer which does not read has stuck (on Windows a pipe flush blocks until the peer reads, and `CancelIoEx` ends
  it). Proofs: `row/run_tests.rs::a_closed_connection_stays_reachable_until_its_threads_have_ended` on every system;
  `tests/row_pipe.rs::a_peer_that_never_reads_its_stop_reply_cannot_hold_the_shutdown` on the hosted Windows run only (on
  Linux the same test in `tests/row_management.rs` passes at the base too, since the reply fits the socket buffer).
  `tests/row_management.rs` and the supervisor suite run on Linux only; on Windows and macOS they are cross-checked. A requested end that
  cannot be recorded in `<state_dir>/ended` (`mark_end_requested`) is not an end: the run fails, the caller is answered
  `failed`, the supervisor is terminal and starts no other agent, so a later `--resume` does not run an agent whose end
  was acknowledged (`tests/supervisor/spawn.rs::a_requested_end_that_cannot_be_recorded_is_not_answered_ended`, over the
  test seam `SOT_TEST_END_MARK_FAILS`); every end answered `ended` is recorded as a requested end in `ended` before the
  answer, even when the agent ended first, so a resume rests: when the agent's run returns, admission closes under the
  lock that reads whether an end was admitted (`Shared::close_run`), so an end admitted before is recorded and one
  after is answered `failed` (no run is live) (`tests/supervisor/spawn.rs::an_end_run_asked_after_the_agent_ended_by_itself_is_never_answered_ended_unrecorded`,
  over the test seam `SOT_TEST_RUN_OVER_HOLD_MS`); a supervisor that goes terminal sends its running agent
  `Stop` first. No call carries an id and nothing is kept for a resubmit. A completion answer is kept under its own
  admission, so another run's admission never clears it. A client that lost a reply, or got `ending`, settles it by
  reading status from the same supervisor (the dial's `(pid, created)` must match), for at most 10 s: for `end_run` the
  run gone means ended (with the supervisor terminal, failed) and the run still there means unknown, and for `reset` a
  run other than the one it rested on (possibly none) means done and the same run with the supervisor resting or
  terminal means not (a resting supervisor begins a run only after a reset); nothing is ever asked a second time, and a replacement run's status never stands for the original
  (`tests/row_management.rs::{an_end_run_whose_reply_is_lost_after_the_run_ended_reports_ended,
  a_reset_whose_reply_is_lost_is_settled_by_the_run_status_names}`, over a stand-in row that drops the connection). The
  daemon's calls are bounded from the request (status 5 s, end_run, reset and stop 30 s; a dial adds up to 2 s before
  it, and a recovery up to 10 s after). Limit: management shares the row's
  8 connections with the viewers (the observer holds one), so a ninth dial gets no slot, which the daemon reads as one
  failed observation and never as an end. The dial checks the hello's row number alone (`ROW_PROTOCOL`, which freezes
  at the 0.7.0 tag), so a row of another number is `VersionSkew`, which rows reads as `foreign`; a row from before this
  pass answers the four ops `unknown_op`, which rows reads as `unreachable` (the dev-box limit).
- The supervisor keeps `<state_dir>/screen`: the vt100 fork's checkpoint (format 2), handed every 5 s while the screen
  changed (and at the agent's end) to one writer thread, which writes it through platform's `write_durable`. The slot
  between them keeps only the newest snapshot, so a stalled or failing disk never reaches the output loop; a failed
  write is noted once per failing streak and the next pace hands over a fresh snapshot. The end waits for the writer at
  most 2 s. A run starts its parser from the file, with a `[restarted <UTC time>]` line below the retained content (on
  Windows both are scrolled into the history, because the ConPTY host repaints the visible screen); a file that does
  not restore is removed and the screen starts empty. Limits: the file can be 5 s old, a failing disk can leave it old
  for good, and scrollback in it is at most `CAPSULE_SCROLLBACK_ROWS` (200).
- Input is written at most once: `input_recorded` means the bytes were written to the pty's master. The client reports
  an input whose acknowledgement was lost as delivery unknown and never sends it again.
- One authority per state dir (`supervisor.lock`, taken by `lock_supervisor`), which also makes one run per state
  folder: the supervisor runs one agent at a time on its main thread. The supervisor's first act is to make its state
  folder (on Windows the start creates none). A run's start proves its state folder's volume (`preflight_volume`: a
  network state root is refused there, platform's guarantee 8). Test seams, each inert unless named:
  `SOT_TEST_END_MARK_FAILS`, `SOT_TEST_SUPERVISOR_BOOT_FAILS` (the supervisor fails right after making its state
  folder, exit 69, before its fence), `SOT_TEST_RESET_HOLD_MS` (the main thread waits that long after a reset is
  answered, before the next run), `SOT_TEST_RESET_CLEAR_FAILS` (the reset's publication fails) and
  `SOT_TEST_RUN_OVER_HOLD_MS` (the main thread waits that long after a run returned, after it recorded an asked end and
  before it decides how the run ended).
- The row socket `row-<h>` (`<runtime dir>/row-<h>.sock`, a pipe `\\.\pipe\sot-row-<h>` on Windows) is served by the
  supervisor for as long as the supervisor lives (it outlives each run it serves), in wire's frames. A connection is served nothing before its `row.hello`
  is admitted: the challenge's OS steps first, then the hello's reply naming the row's `(pid, created)`. The hello
  has one absolute deadline (Unix) and its framing keeps wire's envelope cap on the whole line. A peer of another
  account is dropped at accept. The runtime folder must be private (`runtime_dir` checks it; `RowServer::bind` on a
  path of the caller's own only documents it). On Windows every pipe instance carries the protected owner-only
  descriptor, read back from each instance by `tests/row_pipe.rs` (hosted).
  The hello's `row` number (`ROW_PROTOCOL`, now 1) is the one gate between a row and a client: no reader judges a
  frame's `v`, and a client refuses another row number by name, never by a hang. A client that cannot prove the peer
  says so (`Unproven`), apart from a deadline that expired (`Timeout`).
- **A viewer that falls behind is resynced, never cut for it, and its input replies are never dropped.** The window
  shows an input whose delivery is unknown (`delivery unknown`, `run ended`) in its status line. The rule is below.
- Eight connections are eight real ones. A connection has two threads, a reader and a writer; its slot returns only
  after both have ended, and `RowServer::shutdown` stops the acceptor, ends every connection's stream and joins every
  connection thread. A viewer that cannot keep up is resynced, not cut: when 256 output frames wait for it they are all
  dropped, it is taken out of the live set, and the next ground state sends it a fresh snapshot, after which output
  follows in order; its answers (a request's reply, a demotion) are never dropped, and a snapshot it has not begun to
  receive is replaced by the fresh one, so a viewer held back through many resyncs keeps one screen queued, not one per
  resync, and a driver that types under a flood keeps its connection and gets every reply (`tests/row_flood.rs`,
  `row::run`'s resync test). A viewer whose answers alone fill 64
  frames, or that makes no progress writing for 5 s, is cut: its stream is shut down so its blocked threads return,
  and its slot returns when they end. On Unix that is `shutdown(2)`; on Windows it is `CancelIoEx` on the pipe's
  handle, with later calls failing at once. Limits: a Windows pipe has no read timeout, so a peer that
  never says hello holds its two threads and a slot until it closes or the row ends; a write blocked in the
  supervisor's own terminal (below) is not a row thread's. The client's incoming queue is bounded (256 frames), so a
  caller that stops pumping stops the client reading, and the row drops its output and resyncs it; a request's write is
  under its deadline.
- `row.attach` sends the screen as a snapshot cut at a parser ground state, in blobs of at most 1 MiB with the last
  flagged, then the output in order in blobs of at most 64 KiB. One connection at a time is the driver. A driver's
  attach applies its own size to the terminal first, waiting for the loop's answer, so the snapshot is cut at that
  size; a failed OS resize is a refused attach or resize, never a success. A reattach as a watcher gives up the pen.
  Only a driver's `row.input` is written and its `row.resize` applied (2x2 to 512x256); a watcher's is refused by name
  and writes nothing; a second driver (an attach as driver, or `row.take`) demotes the first with `row.demoted`.
- Row requests (input, resize) wait in a queue of 64, never more: one that finds it full is refused as `busy`. The loop
  drains at most 16 a pass and has at most one wake pending, so output, ticks and the exit checks still run under a
  flood. Input is acknowledged as written, or reported `delivery unknown` when the loop did not answer in 2 s (it may
  still write it; the client never sends it again), and input queued for one run is answered `run ended` when the run
  changes and never written into the next. The run id is a UUIDv7 minted at each agent start.
- Limits of this landing: a live 6b row on a dev box is not served by the row socket, and no candidate is cut from main
  until 6c-2. On Linux the challenge refuses a peer that started in the clock tick the client connects in
  (`validate_pin`), so a supervisor dialed within 10 ms of its start can be `Unproven`; a later connection, with a
  fresh anchor, is proven, and a test binary that is both ends waits out one tick first. The row socket is the
  capsule's first dependency on `sot-protocol`.
- The row socket is the way in for every shipped viewer (pass 6c-1a2; the drawer's attach and the old lane's own clients
  are not shipped). `FeAttachClient` is one worker thread over `RowClient`: it waits until the row is viewed and the
  endpoint's link is up, dials once, restores the snapshot into its parser and then follows the output, restoring any
  resync snapshot the row sends; a lost connection is redialed on platform's `Redial` (250 ms doubling to 4 s, started
  over only after a connection that lasted `STABLE`). A login (an ssh one has topology's 30 s) and the row's hello (10 s,
  from the stream being up) have budgets of their own. The window attaches as the driver, the wake and the dialog tick
  as watchers that take the pen only to type; a client that was demoted, also while its snapshot was on its way, takes
  it again on its next input. Input typed before the attach is held and sent after it, 8 KiB at most; each queued input
  is tagged with the run it was typed into, and one left over when the connection comes back to another run is answered
  `run ended`, counted and never written. A headless client takes the pen once per run: its Enter never takes it in a
  replacement run, and in the same run after a reconnect it does not take it again. Unknown delivery (`delivery unknown`,
  `run ended`) is the outcome the window says in its status line. The worker stops reading the row once 4 MiB of
  output sit unconsumed, so a viewer that does not pump is resynced by the row. A failure ends the episode only when it
  cannot get better (another account's socket, a row number this client does not speak, a refusal by the daemon, access
  denied, a socket path that can never fit) or when the row's socket has been absent, though the daemon answers, for
  120 s running; anything else is retried, and a daemon that cannot be reached starts that window over. The drawer's
  attach (dormant until 6c-2 deletes it) names a row no row has, so it ends terminal after those 120 s.
- A row listener that dies (a permanent accept failure) does not leave a running agent nobody can reach. The supervisor
  watches it, blocked on an event the acceptor sends as it dies (no clock), and binds it once more; a second death, or a
  bind that fails, ends the live run unrequested
  (`Command::Stop`, no request recorded) and the supervisor exits 75, so rows' watchdog decides through its one restart
  budget (`tests/row_supervisor.rs::a_row_listener_that_keeps_dying_ends_the_supervisor_with_75`, over the test seam
  `SOT_TEST_ROW_LISTENER_DIES`); a listener lost while the supervisor rests (its run ended, nothing else can stop or
  reset it) ends it the same way, with 75, and frees its fence, and no run is admitted once the listener is lost
  (`tests/row_supervisor.rs::a_listener_lost_while_the_supervisor_rests_ends_it_with_75`, over
  `SOT_TEST_ROW_LISTENER_DIES_ON=<file>`, and `supervisor/authority_tests.rs`; a death that races the end of the
  supervisor is not rebound; `the_row_listeners_watch_waits_on_an_event` proves the watch gains no wakeups). Limit: a
  viewer that dials in the gap between the death and the rebind sees a refused connection and retries.
- An end retires the row's request queue. When the loop leaves its main loop (or the agent could not start), `RowRun::end_run`
  closes admission and answers every queued request `run ended`; a request that arrives after is answered `run ended`
  at once, until the next run starts, so input sent to an ending or ended run never times out as `delivery unknown`
  (`tests/capsule/row_run.rs::input_sent_while_a_requested_end_is_in_progress_is_answered_run_ended`).
- A bind failure of the row socket ends the supervisor with 75 (`EXIT_AGENT_NOT_STARTED`), before any agent starts:
  it is a row that did not start, which rows' watchdog is to count against its one restart budget, ending the row terminal
  only when the failure lasts through it. Rows' watchdog restarts a supervisor that died before its first answer, since rows read a start from the fence file
  (`capsule_workspaces::a_row_socket_that_cannot_be_bound_ends_the_row_terminal_through_the_budget`).
  The Unix bind reclaims a socket file left behind without probing for a listener: the fence is held, so the file is
  this row's own.
- Every write to the terminal has one owner (`src/row/input.rs`): one thread, started with the run's terminal, writes
  typed input and the host's DA1 reply in the order they were handed over, so the main loop never blocks on the terminal
  (`a_paste_the_agent_has_not_read_does_not_stop_its_output`: a paste fills the input buffer, the agent then asks DA1 and
  writes more output than the run queues before reading). Up to 16 typed writes wait behind it and one more is refused
  `busy`. The run retires it at its end, within 2 s: what waits is answered `run ended` and never written, and the thread
  is joined; a write blocked in the terminal returns when the agent's terminal is hung up. Limit: a thread that still
  does not end within the bound is noted and left; it holds the cloned terminal until the write fails.
- Attach is ground-gated (`RowRun::tick`, once a pass, and `RowRun::output`), so a viewer joins only at a parser ground
  boundary, after every byte the parser has seen was published.
- `sot-capsule supervise` exits 0 (a stop), 69 (`EXIT_TERMINAL`: it cannot take its fence), 70 (`EXIT_CONTENDED`, the
  fence was already held), 75 (`EXIT_AGENT_NOT_STARTED`: the agent could not be spawned, or the row socket could not be bound) or 76 (`EXIT_AGENT_ENDED`: it
  started and ended by itself), the last two with no stop or end asked for (`tests/supervisor/spawn.rs`); rows' one restart
  budget decides when a row that exits 75 or 76 ends terminal, a row whose socket could not be bound included. A supervisor
  killed has no code. Limits: a supervisor that fails to take over an inherited claim (a wrong or unlocked descriptor)
  also exits 70, which does not prove another live authority; and on Unix an executable that passes preflight but fails
  at exec can read as an aborted child, so 76 for a program that never started (the exec-error pipe is closed). There is
  no storage hold and no exit 71. `--survival` and `--assume-no-rollback-target` are
  accepted and ignored.
- Only linux, macos and windows build.
- A reply on a local connection is trusted only after the challenge in platform (`../platform/src/identity/`); the
  row hello's request and reply are `RowHelloExchange` (`src/row/proto.rs`).

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `sot-capsule supervise`,
`supervisor_client`, `FeAttachClient`, `rust/backend/src/rows/run/headless.rs`,
`rust/frontend/src/ui/agent_pane/attach.rs`, `ended`, `screen`, `Endpoint`, `DaemonLaneEndpoint`. Uses:
`DaemonLaneEndpoint`, `lane.connect`, `try_lock_daemon`, `preflight_volume`,
`owner_protected_pipe_descriptor`, `harden_own_stdio`, `inherit_none`, `boot_identity`, `process_created`, `IdentityExchange`, `write_durable`, `remove_durable`, `storage_exhaustion`,
`Redial`, `STABLE`.

## Folders
- `src/capsule/`: a run, one producer under a live parser, its screen file and its ending.
- `src/supervisor/`: the one process of a row: the fence, the management calls and the main thread's loop.
- `src/lane/`: transport pieces (the connect bound, the error type), the client seam and the two platform clients.
- `src/attach_client/`: one client for every viewer of a row, and the daemon's calls on a row's supervisor.
- `src/row/`: the row socket: `RowServer` (acceptor and connection threads), `RowRun` (the run's fan-out and request
  queue, what the loop feeds and drains), `RowClient` (the blocking client) and the hello's payloads and exchange.
- `../platform/`: the platform crate, whose charter is `../platform/CLAUDE.md`.
- `src/bin/`: the sot-log crate's binaries (`sot-capsule` and the two helpers).
- `tests/`: integration and whole-process tests (the map is in Files below).
- `../vt100/`: the project's fork of vt100-ctt, the terminal-state parser.

## Files
- `Cargo.toml`: the crate manifest; the `test-support` feature is switched on for this crate's own tests, and brings
  `test_log`, `test_copy`, `test_scan` and, on Unix, `test_foreign`, and turns on platform's `test_isolated` and `test_wait` (the
  backend, frontend and updater turn it on in their dev-dependencies, and platform's own beside it).
- `build.rs`: stamps the build id (`SOT_LOG_BUILD_SHA`, `sot_log::BUILD_ID`) from the full git sha, or `SOT_BUILD_ID`.
- `tests/`: integration and whole-process tests, by target:
  - `capsule/`: `capsule::run` over a real producer or a fake one: the run to its end, `ended` and the screen file
    (`main.rs`, `no_record.rs`, `screen_io.rs`, `output_end.rs`), what a `RowClient` on the row socket sees of a run
    (`row_run.rs`, over `rowside.rs`), the Unix and Windows mechanisms (`unix_only.rs`, `windows_only.rs`, which holds the ConPTY snapshot-fidelity proof on the row socket).
  - the row socket: `row.rs` (a fake loop), `row_limits.rs`, `row_flood.rs`, `row_switch.rs`, `row_pipe.rs` (Windows),
    `row_supervisor.rs` (a real supervisor: the row socket serves, no lane is bound, and the listener's watch waits on
    an event), `row_management.rs` (the daemon's calls on the row socket), `fe_attach.rs` (`FeAttachClient`),
    `fe_client/` (the client against a real supervisor, and the headless client).
  - `supervisor/`: the real-process supervisor, driven over the row socket.
  - the transports' clients, against the row acceptor: `socket_unix/` and `pipe_win/` (cancel, concurrent submit, the
    connector's bounds and descriptor flags, an excess connection), `connect_own.rs`, and the identity challenge
    (`challenge_unix.rs`, `challenge_macos.rs`, `pipe_win/challenge.rs`: a real `RowServer` answers the hello, and the
    adversarial cases are the exchanges in `support/row_challenge.rs`), with `other_account.rs`,
    `winhandle_windows.rs` and `macos_kernel_facts/`.
  - `conpty.rs`, `isolation_guards.rs`, `test_body_fixture.rs`, and `support/` (`capsule_guard.rs`, `row_challenge.rs`).
- `src/lib.rs`: the module tree, the `storage_exhaustion` re-export, `BUILD_ID`, `SupervisorPhase` and `Error`/`Result`.
- `src/secret.rs`: `redact` and `RedactingWriter`, the masking of page secrets in both binaries' logs.
- `src/test_barrier.rs`: `hold`, a named phase barrier a real process can be held at (Unix, feature `native-barrier`, which no installed build enables; inert unless `SOT_TEST_BARRIER_DIR` names a folder).
- `src/test_copy.rs`: `copy_release_folder`, the one copy of a project folder for a test, leaving out every Julia manifest (feature `test-support`).
- `src/test_log.rs`: timestamp-free `capture()` and formatting-preserving `install()` (feature `test-support`), with behavioral tests of the capture format, first-callsite routing, parallel isolation and guard restoration.
- `src/test_foreign.rs`: `ForeignListener`, a socket another OS account (`nobody`) listens on, for the User isolation
  tests, and `elevation_or_skip`, the one rule for a test that needs root: it skips only when `sudo -n true` fails (a
  failure on CI), and every later helper failure fails the test (feature `test-support`, Unix).
- `src/capsule/`: a run's runtime, its screen and ended files, and the producers.
- `src/supervisor/`: the supervisor, its fence, and `Shared`, the state its management calls and its main thread share.
- `src/lane/`: the platform clients (`socket_unix/`, `pipe_win/`), the client seam and the error type.
- `src/attach_client/`: the attach client, its worker (`attach.rs`) and the daemon's calls on a row's supervisor.
- `src/row/`: the row socket's server, run side, client and protocol types; tests `tests/row.rs`, `row_limits.rs`, `row_supervisor.rs`, `row_pipe.rs`.
- `src/lane/connect_own.rs`: the connect that asks platform's account check.
- `src/storage.rs`: `storage_exhaustion`, the capsule's reading of platform's storage table.
- `src/test_scan.rs`: the source scans' one walker, `rust_sources()` (every workspace member's `src/` and `tests/`), and its production view `production_sources()` with `without_test_modules`, and `enclosing` and `is_ident` (the `fn` or `struct` a match lies in, and the identifier test at its edges) (feature `test-support`).
- `src/bin/`: `sot-capsule` and the two test-fixture helpers.

## Start here
Read `src/supervisor/mod.rs` for the process and its exit codes, then `src/capsule/writer_loop/mod.rs` (`run`) for
a run, `src/capsule/screen.rs` and `src/capsule/ended.rs` for the two files it leaves; for the daemon's calls, `src/attach_client/supervisor_client.rs` and `src/supervisor/authority.rs`; for a viewer, `src/row/`.

## Rules
- Modules are `pub` where integration tests or other crates reach them: those see only pub items.
- A change to a wire tag, magic or limit, or to an exit code, is an interface change with other processes and versions.
- A fixture selects its executable on the child command. T2's actual SSH fixtures compare the parent PATH and SHELL before, during and after execution; no lexical allowlist proves environment preservation.
- Every subscriber a test installs goes through `test_log`: `capture()` reads this thread's events without formatter timestamps or colour; `install()` preserves the supplied subscriber's formatting and timer. Behavioral tests exercise first-callsite routing, parallel captures and guard restoration; new callers use this owner. The two binaries' `main` functions install production subscribers.
- Test-program writers use `sot_platform::test_exec::write_executable` (platform's, feature `test-support`; its Linux FIFO proof observes actual parent descriptors while the child writer is active), a copy of a built program included (`write_executable(&to, std::fs::read(&from)?)`, never `std::fs::copy`, whose writer is the test process); its separate Unix writer leaves no writable program descriptor in the test process. The retained real descriptor test proves this owner property; `std::fs::copy` is in `rust/clippy-tests/clippy.toml`'s disallowed methods, so a copy of a program is a compile error in the clippy all-targets run, and a data copy carries `#[allow(clippy::disallowed_methods, reason = ...)]` on its statement.
- Named test reruns use `test_isolated`; direct fixtures retain child wait/status, termination, entry and separate output observations before raising failures. Regression proofs require observed prerequisites and the selected failure's exact cause; wrong-cause controls use the same verifier. Duration/deadline APIs share one wait implementation; byte output preserves valid text and escapes invalid UTF-8. Wrapped fixtures retain launcher status and both streams, validate the native role/PID witness against exact entry, and share direct fixtures' wait/drain/finalization implementation; ordinary direct fixtures still validate their spawned child PID.
- Every Rust source-text scan reads through `test_scan`, and none cuts a file at its first `#[cfg(test)]` (`test_scan::tests::no_scan_cuts_a_file_at_its_first_cfg_test`).
- Helper binaries `sot-pty-helper` and `sot-conpty-helper` are test fixtures and never ship.
- Another crate can reach the processes this crate starts (the capsule's agent) through its public items, as read on 2026-10-05: `capsule::run`, `capsule::producer::Producer::spawn`,
  `capsule::producer::conpty::ConptySpawn::spawn`, `supervisor::supervise` and, on
  Unix, `host::process_tree::Launch::begin` (the native launcher). `rust/clippy.toml` lists none of them, so the lint
  does not see a call from another crate; today no other crate calls them but `src/bin/sot-capsule.rs`, which reaches
  them through `capsule::run` and `supervisor::supervise`, and the daemon-lifetime harness
  (`rust/backend/tests/daemon_lifetime`), which calls `Launch::begin`.
