# rust/backend/src/sidecars: sidecars (contract)

The helper processes the daemon relies on and speaks to over their own stdio: per row a Julia kernel (`Kernel`,
kernel.rs; previews and project scans, never user code) and a Julia REPL (`Repl`, repl/; user code); per daemon one
Pluto (`Pluto`, pluto.rs) and one MathJax node child (`MathJax`, mathjax.rs); per monitored host a sampler
(`MonitorHub`, monitor.rs); and what every child carries (child/: its temporary folder, its stderr tail and, on Linux,
its own X display). Each child is owned by one daemon task from its spawn to its reap, and callers submit and wait at
a stated bound; they never spawn, kill or retry. `ops.rs` holds the handlers of `kernel.request`, `math.render`,
`pluto.open` and `monitor.*`; the server's op table calls them (`server/dispatch.rs`), as it calls the REPL's.
Two passes wrote and proved this page: pass 8a the kernel, Pluto, MathJax, the monitor, the julia choice, `child/` and
the ops (guarantees 1 to 19); pass 8b the REPL (guarantees R1 to R7, the section "The REPL") and the channel that
announces a browser page's port to pages. Part of the sidecars' subsystem, beside this
folder: `julia/kernel`, `julia/plugins`, `julia/pluto`, `julia/repl`, `core/` and `rust/backend/sidecars/mathjax`.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on and the limit it has. The Tests section holds the test
of each.

1. **One answer for which julia runs, with no bare `julia` from an absolute-path source.** `julia::resolve_bin` takes an
   absolute `SOT_JULIA_BIN`, then juliaup's default channel (the binary in the version's `.app` bundle when one holds
   it, else `bin/julia`; the bundle is looked for on every OS though only macOS installs make one), then a verified PATH
   candidate. Its error is a `JuliaError`: `NotFound` (nothing named, installed or on PATH) or `Refused` (a relative or
   alias override, every PATH candidate refused). The kernel, the REPL, Pluto, quarto (`run_quarto`) and the update
   prepare (`prepare_julia`) all run its answer, the kernel at every generation, Pluto at every start and the REPL at
   every spawn. Limits: a missing explicit absolute path still fails at spawn, not at the choice; a juliaup default
   whose binary is gone falls through to a usable PATH candidate (`julia.rs:84`), it is not refused outright; an empty
   or relative PATH entry can yield a relative answer, because only the tests' entries are absolute (queue); on Windows
   the selected file is inspected through ordinary filesystem links (`IO_REPARSE_TAG_APPEXECLINK` and inspection errors
   are refused; a folder called `WindowsApps` is not evidence of an alias). MathJax does not use it: it runs
   `SOT_NODE_BIN` or the name `node`, a PATH search at spawn (`MathJax::new`).
2. **The kernel is not given the user's startup files or default environment, and its root is data.** It starts as
   `julia --startup-file=no --project=<julia/kernel>` with `JULIA_LOAD_PATH=@:@stdlib`, so no startup file's prints
   land on its wire and the user's default environment (`@v#.#`) and any inherited `JULIA_LOAD_PATH` are not in its
   load path; the workspace root reaches it as the environment variable `SOT_KERNEL_PROJECT_ROOT`, never as text of the
   Julia source it runs. A kernel whose project folder is missing is dead without a spawn. Limits: the child still
   inherits the rest of the daemon's environment, so the depots and other `JULIA_*` variables are the daemon's; a root
   that is not UTF-8 is refused; the kernel loads the plugins its own project names, and what they do is the plugins'
   (guarantee 19).
3. **A kernel caller waits at most `KERNEL_REQUEST_TIMEOUT` (10 s), its send included, and never spawns or kills.**
   `Kernel::request` bounds the whole call. A running kernel that does not answer is `KernelUnavailable::NoAnswer` ("the
   running kernel did not answer <op> within 10s"), a starting one `Starting` ("kernel still starting"), a dead one
   `Dead` (the reason it ended or did not start). `kernel.request` answers each as `code: kernel_unavailable` and any
   other failure as `kernel_request_failed` (`kernel_error_payload`); an unknown row is `unknown_workspace` (rows').
   Limits: a request the kernel's queue does not accept within the bound fails at the bound and was not sent; a request
   still queued when its generation dies gets the generic channel error, so `kernel_request_failed`, not
   `kernel_unavailable` (queue); a caller's timeout does not cancel the kernel's work.
4. **A kernel that is dead or wedged is replaced, on a backoff that a kernel that does not serve cannot reset.**
   `supervisor_loop` respawns after 250 ms, doubling to 30 s (platform's `Redial`), and starts the doubling over only
   after a generation that served `STABLE` (60 s) after its hello: a kernel that never answers, or answers and then
   dies, keeps the doubling however long its precompile ran. A kernel that answers nothing for `KernelBounds::answer`
   (300 s, counted from the later of the oldest request's write and the kernel's last answer) or not its hello for
   `start` (1800 s) is ended with that reason. Limits: a kernel that answers slowly but steadily is kept; the
   generation does not observe the leader's exit, only its pipes and its deadlines, so a dead kernel whose descendant
   holds its stdout, with no request pending, is not replaced until a request arrives or its owner lets go (queue); a
   protocol-version mismatch in the hello is logged, not refused.
5. **A Pluto start belongs to Pluto's supervisor, not to the open that began it, and the waits on it are bounded.**
   The supervisor waits up to `PLUTO_READY_BOUND` (1800 s, the kernel's start bound; both cover a cold precompile)
   for the server's `READY` line, then ends the child and the start fails naming the bound. An open waits on the start
   for at most `PLUTO_START_WAIT` (120 s); past it the open answers `pluto_starting` (not a failure: the window shows
   it without a failure prefix) with the start's age and the child's last stderr lines, and the next open joins the
   same start. A start that fails answers its reason with the same lines. Once the server is ready an open answers
   within `PLUTO_OPEN_BOUND` (30 s, from its submission) or fails naming it, and the server that did not answer is
   ended, since its replies are matched oldest-first; the next open starts a new server (`ensure_supervisor`, which
   also replaces a server that died). Limits: the supervisor's own deadline covers the wait for `READY`, not the
   building of the start before it, and its cleanup awaits an unbounded reap; the open's deadline lives in the caller's
   future, so a caller that is cancelled after its request was accepted leaves the request and the supervisor running
   (queue); before `READY` the supervisor does not observe the leader's exit, so a dead leader whose descendant holds
   stdout reads as starting until the readiness bound (queue); a notebook the server opened by itself, from Pluto's
   home page, is not known to the daemon (guarantee 6).
6. **The daemon knows one fact about Pluto's notebooks: whether its server's supervisor is running.**
   `Pluto::server_up` is true from the server's start (a starting server counts) until the supervisor's channel closes
   or the start fails, and `workspace.list` carries it as `pluto_server_up`, so the update's question covers every
   notebook open in it. Limits: it reports the supervisor's state, not a probe of the server, so before `READY` it can
   read up for a leader that has exited (guarantee 5); Pluto's home page opens and closes notebooks the daemon never
   sees, and the daemon does not know which are open.
7. **Pluto's proxy port is a grant held by one supervisor generation, published only after a loopback `READY`.** The
   grant (`Grant::publish`, read as `bound_pluto_port`) names the port parsed from a loopback `READY` URL
   (`sot_protocol::page_url::loopback_port_from_url`); a `READY` that names another address grants nothing. It is
   released on every supervisor exit or cancellation, before cleanup awaits, and an old generation's release cannot
   erase its replacement's (the generation number). The pages' proxy reads it through `bound_pluto_port`; sidecars call
   nothing in pages for it. Limits: `READY` is the child's own word, so the grant is that of a port the child named, not proof that the listener there is the child's; the grant is a process-wide cell, one Pluto per daemon.
8. **A Pluto submission that is cancelled by the Signal or by the child's death neither hangs nor resends.** Pluto
   polls child exit and its `Signal` during each stdin write and flush; either retires the supervisor, releases its
   grant and closes current, pending and queued replies before checked cleanup; a request pushed after the close is
   answered; a partial `OPEN` line is never resent. Limit: the answer is an error naming the end, not the open's
   result; the cancelling of a submitting caller is not among these (guarantee 5).
9. **A MathJax render waits at most `START_BOUND` (60 s) for the node child's ready line, then `RENDER_BOUND`
   (10 s) for its answer, while its caller waits.** The ready line is `{"ready":true}`, which node writes once its
   modules are loaded, so a cold start is not counted against a render, and renders that arrive during a start share
   it. Past either bound the render is a `mathjax_render_failed` error naming it and its child is ended, even
   mid-write, so the next render starts a fresh one; a child that dies is replaced by the next render
   (`ensure_supervisor`). Limits: the deadlines are the render future's, so a caller cancelled after its request was
   accepted leaves no deadline on the supervisor, and a blocked write to the child is not cancelled by the `Signal` (the
   outer select does not watch it; queue); a child's death is seen by its stdout's end, so a descendant holding stdout
   defeats the replacement; every render waiting on a child that ends or is ended fails with it (`mathjax sidecar
   terminated`), and one child serves all renders, so a render behind a slow one spends its own bound waiting.
10. **Decoding a child's output never fails: no invalid byte ends a reader or stops a stderr drain.** Every sidecar
    child's stdout and stderr are read through the wire's `ChildLines`, which decodes each line lossily (an invalid
    sequence becomes U+FFFD); none re-encodes. Limits: a line the kernel or Pluto sends that does not parse is logged
    without its bytes; MathJax ends its generation on a first line that is not exactly its ready line, a lossily
    decoded one included; lines and stderr lines have no byte cap (`child_lines.rs:13`); a lossy line that carried a
    binary payload is no longer that payload, and no child here sends one on a text line.
11. **A child that ends other than by the daemon's shutdown leaves its last 30 stderr lines in the log at warn, one
    event, when it wrote any.** `StderrTail` is held by the owner of a kernel, Pluto, MathJax or REPL child; every
    stderr line is also logged at debug under `<child>.stderr`. A Pluto start that fails or is still going answers with
    the child's last lines (`child::last_lines`, `Start::still_starting`: the start's age and the last five lines), as
    do the REPL's execute timeouts and the display's failed start. The kernel's and MathJax's startup errors carry no
    tail in their answer (`kernel.rs:419`, `mathjax.rs:138`); theirs is in the log. Limits: a child that wrote nothing
    logs no tail (`stderr_tail_tests.rs`: `a_child_s_last_lines_are_logged_at_warn` asserts it); a restart by a
    child's owner (the REPL's) and a kernel's owner's drop are not deaths and log nothing.
12. **Every sidecar child starts through `Signal::spawn` under a caller-supplied `&'static Signal`, whose fire ends
    it where its supervisor polls it.** The kernel, Pluto, MathJax and monitor supervisors take the `Signal` for both
    spawn and shutdown observation; the REPL and MathJax select the daemon's process `Signal` only where their
    production handles are built (`server/mod.rs`, `Workspace::repl`), Pluto in `ensure_supervisor`. After the fire no
    supervisor respawns and the monitor's backoff ends; the kernel's and Pluto's blocked stdin writes do not outlive it.
    Dropping a handle during a start ends the child. Limits: MathJax's blocked write and the monitor's initial script
    write are not polled for the `Signal` (`mathjax.rs:284`, `monitor.rs:670`; queue); the Pluto handle reads the
    process `Signal` in `ensure_supervisor`, so a test of its shutdown fires the process-wide one and runs in its own
    process (`isolated`).
13. **A sampler that gives no sample is ended and replaced, once it is running.** One sampler per monitored host: a
    local one on Linux only (`without_local_sampler`) and one ssh per declared remote host, built from `SSH_OPTS`, so it
    turns ssh sharing off as the bridges do (`sampler_command`). A sampler that gives no sample for `SAMPLER_SILENT`
    (20 s), silent or printing only lines that do not parse, is ended, shown stale, and started again after
    `RESPAWN_BACKOFF` (5 s); the reason it logs carries the last line it wrote to stderr, and a repeated reason is
    logged once (`should_log`). A process name reaches the daemon as `/proc` gives it: the awk joins a stat line a `\n`
    in the name split, and the daemon makes control characters spaces before parsing. Only the hub runs the declared
    `[monitor]` roster, read once at start (`load_hosts`, `sampling_roster`). `monitor.history` answers the retained
    samples by tier (`query`). Limits: the script's delivery to the child (`monitor.rs:670`) has no deadline and is not
    polled for the `Signal`, and the silence clock starts after it, so a stalled ssh start that fills the stdin pipe is
    never replaced (queue); ending a sampler ends the local ssh process, not proof that the remote sampler ended; the
    roster is read once, so a change needs a restart; macOS and Windows daemons sample nothing of their own. The sampler
    script avoids here-documents because bash before 5.3 leaks the pipe behind one (`monitor.rs:36`); no test proves it
    (the source scan that did is deleted).
14. **Each Julia child, and each command the updater runs, has a folder of its own, which its owner removes after
    asking the child's tree to end.** `ChildTmp` makes it in this process's held folder (platform's `HeldDir`), names
    it in the child's environment (`TMPDIR`; `TMP` and `TEMP` on Windows), and its owner's `retire` asks the child's
    tree to end, reaps the direct child, ends its display, then schedules the folder's removal on a thread nothing
    waits for and logs a failure. The held folder is made with the first child folder and removed with the last. A
    daemon's controlled end runs no owner's `retire`, so its terminal removes it (`remove_own`, bounded), and a start
    removes the held folder of every process whose lock is free (a crash, a SIGKILL), never a running one's, whatever
    computer or pid namespace it runs in (`sweep_gone`); a folder kept after a failed kill is in that held folder, so
    the later sweep reaches it once its process has ended. Limits: termination is requested, and the direct child's
    reap is checked where `retire` is awaited; the death of its descendants is not proven (`Contained::kill` has no
    wall-clock ceiling and does not establish it), so a descendant can still write into, or on Windows hold, a folder
    being removed; the updater's cancelled call drops its folder without awaiting the reap (`update.rs:63`); a
    controlled end runs `remove_own` even after a failed or unanswered fire; a folder from before held folders
    (`sot-julia-*`) stays until the OS clears it; a temporary folder two computers share is inside the rule only on a
    file system whose locks every client sees; a descendant that leaves the child's process group on Unix runs on
    without its folder, unless on Linux it holds the child's display (guarantee 15); a program that does not take its
    temporary folder from the environment (macOS's `mktemp` given no template) makes its files outside it.
15. **On Linux each REPL and Pluto child has an X display of its own that only it opens, and ending it signals every
    process that holds it.** `ChildTmp::for_user_code` starts an Xvfb (`Display::start`) on the lowest free display
    number, with a cookie in the child's folder that is the only way in (`-auth`, no TCP, no unix socket), which
    Pluto's notebook workers inherit. The display ends after the child's reap (`ChildTmp::retire`: a restart, a
    `--fresh`, the child's death, its row's end), at the `Signal`'s fire and with its owner's drop. Every end of a
    display sends SIGKILL to every process of the daemon's account whose environment holds its cookie (`XAUTHORITY`),
    in the child's group or out of it (`holders::end`, through a pidfd, so the signal reaches the process read, never
    a later holder of its pid). A display that ends under a live child ends the child and its group
    (`ChildTmp::display_ended`), because any account can bind an ended display's name: the REPL's evals in flight get
    the reason, and its next eval starts a new REPL with a new display and says first why the old one ended
    (`SOT_PREVIOUS_END`); Pluto's opens in flight get the reason and its next open starts a new server. A start that
    fails says why (the daemon's log, `SOT_NO_DISPLAY`, the REPL's first eval) and the child starts without one, with
    the display variables it inherited left in place (`a_missing_display_names_its_reason_to_the_child`). The kernel
    gets none. Limits: a signal sent is not a death confirmed, and a holder whose environment is unreadable, whose
    pidfd cannot be opened or whose signal fails is logged at most and can outlive the display; a holder is found by its
    start-time environment, so a process that clears or rewrites its `XAUTHORITY` is not found; the walks of the process
    table are bounded in number (`WALKS`, 16), not in time, so a holder that forks faster than they find it can outlive
    them; a process of another account is never signalled; macOS and Windows start no display (GLMakie draws through
    the system's windowing). Not built: a cgroup per child (ADR 0050's containment, queue item 25's W3).
16. **The page servers the Julia children run answer nothing without a secret from the OS's generator.** They listen on
    127.0.0.1. Pluto serves every path under its session secret (`base_url`), so nothing, not even its own files or
    `/ping`, answers without it; each notebook worker checks the Distributed cluster cookie it read from its stdin
    before it reads a message (`julia/pluto/session_options.jl`); a REPL's `wglshow` page sits behind its secret path
    and its websocket behind its session id (`julia/repl/src/wgl.jl`). Limit: a process of the same account that
    reads the child's environment can read a secret; the guarantee is against another account and against a page that
    guesses.
17. **A notebook is opened only inside a row's root, with Pluto's header on its first line, and with a path the
    server's protocol can carry.** `pluto.open` canonicalizes the path once and confines it to any registered row's root
    (`canonicalize_within_any_workspace`; else `outside_workspace`); it reads the first 96 bytes and requires
    `### A Pluto.jl notebook ###` somewhere on the first line (else `not_pluto_flavored`); `Pluto::open_notebook`
    refuses a path holding a control character with a `PathRefused` before anything starts or is submitted, since the
    protocol is one command per line; the server takes the path after `OPEN ` with only the line end removed. Limits:
    the header check is a substring of the first line, not an exact line; the validation opens the path without a
    regular-file check or a deadline, so a FIFO inside a row's root parks the op (queue); the canonical pathname, not a
    file identity, is what Pluto opens later, so a replacement in between is not detected; a file with Pluto's first
    line is opened whatever else it holds (it is user code in the user's own row).
18. **`math.render` is a stateless transform.** It answers the SVG as a blob (`image/svg+xml`) with its `ex` height and
    the session's current revision, and bumps none; a failure is one `mathjax_render_failed` frame with no blob, never
    a closed connection. Limit: the success path needs the helper's npm modules (`npm ci` in
    `rust/backend/sidecars/mathjax`) and node, and its test fails without them; the other MathJax unit tests use `sh`
    as node.

19. **The kernel loads only the plugins its own project names, gives a file to the first that matches, and bounds the
    external tools they run.** The kernel's project names the built-in
    `FileType` plugins as dependencies and `ShipToolsKernel.jl` imports them eagerly (`julia/kernel/Project.toml`,
    `ShipToolsKernel.jl:9`); the one heavy plugin loads lazily, attempted at most once per kernel (`preview.jl:29`, the
    `LAZY_PLUGIN_FOR_EXT` table); a file goes to the first `FileType` that matches (`core/src/ConceptExplorerCore.jl:169`);
    a preview answers a payload or an error, never an exception to the wire (`preview.jl:99`); an external tool a plugin
    runs has a wall-clock bound (`ConceptExplorerCore.jl:240`). Limits: a lazy load that failed is not retried until the
    kernel restarts; nothing discovers a user's packages as plugins (see the root CLAUDE.md); a plugin's own code is the
    plugin's.

## The REPL
The REPL is guarantees R1 to R7 (pass 8b settled them against the code; the Tests table holds each one's test). What it
shares with the other children is guarantees 10, 11, 12, 14, 15 and 16.

R1. **One `julia` child per row, started by the first submission that needs one and replaced by the next after its
    death.** `repl.eval`, `repl.run_file` and `repl.execute` start it (`Repl::ensure_supervisor`); `repl.interrupt` never
    does (`request_if_running`). The `Repl`'s slot serializes replacement: what is dead or still retiring is retired and
    joined before the replacement opens, on every route to a spawn, and a death respawn uses the project the last
    restart chose. The child runs in the row's project with its own temporary folder and, on Linux, its own display
    (guarantees 14, 15); the shim project and the julia binary are resolved on every spawn attempt, so a repaired
    install is used by the next one. Its lifecycle is `not_started`, `starting`, `ready`, `dead`, announced as a
    `lifecycle` frame, written under a spawn generation so a retired child's late death never overwrites its
    replacement's state. Limits: a REPL that nobody submits to after its death stays `dead`; a child that never says it
    is ready stays `starting` (named by its age and last five stderr lines, never ended by the daemon).
R2. **A caller waits at a stated bound, and a full queue is refused at once.** Admission is the only immediate refusal:
    16 queued submissions per child, then `repl_busy` (`ReplBusy`). `repl.execute` waits at most its `timeout_ms`
    (default 120 s, clamped to 1 s to 30 min) for the child's answer and then answers `timeout` without interrupting,
    so the run goes on and its frames still reach the drawer; `repl.interrupt` waits `REPL_INTERRUPT_WAIT` (10 s). Limits:
    acquiring the slot and retiring a predecessor can wait before admission (`REPL_RESTART_WAIT`, 5 s, bounds the join
    of a retirement, not the whole restart); the `execute` timeout begins after the submission and the child's start,
    and the spilling of figures follows it, so the call can take longer.
R3. **Every accepted run is closed out once, and its waiting reply is answered, however the child ends.** A restart, the
    signal, a row's end, the child's exit or a display's end (`ReplEnded`) answers each waiting request with the end and
    gives each streamed run an `error` frame naming it and a `done`, after the `dead` announcement, including a
    submission that arrived after the close began; the window's run is marked ended before its `done` is published
    (`numbers.rs`). Limit: a run's frames are published on a bounded broadcast bus, so a subscriber that lags loses
    frames; `repl.execute` collects its own frames loss-free.
R4. **An interrupt signals only the eval the child runs, only while it runs, and an eval that never yields is
    reached by the signal, not by the request.** The shim reads requests on the thread that evaluates, so a loop with no
    safepoint cannot read `repl.interrupt`. When the child has said nothing for `QUIET` (1 s) with an interrupt
    unanswered, `Escalation` sends one interrupt signal to the child (`Contained::interrupt`), which Julia raises at the
    next safepoint; a forced request (`force: true`) sends them `BURST_GAP` (50 ms) apart for `FORCE_SPAN` (800 ms; 400 on
    macOS and Windows), which Julia forces into code with no safepoint. Any line from the child restarts the quiet, one
    after a signal ends the signals, and the eval's `done` ends the escalation. Never an idle REPL, an eval the request
    did not name, or one after its `done`. When the signals are over and the quiet has passed with no answer the
    daemon answers the request itself, saying what is left (`--fresh` restarts). Limits: C code, compiling and garbage
    collection take the signal only when they return, and forcing inside them can crash or hang the REPL; on Linux a
    loaded computer counts five signals as two or three, so a forced interrupt may not stop the eval, which the answer says. This is queue item 20
    (an interrupt never reaches an eval that does not yield): since the escalation it does reach one, to the stated limits;
    nothing is built for it in 8b.
R5. **A retirement is checked, and an unfinished one stays owned.** A restart stops the old supervisor, which closes out
    its work, asks for its tree's termination and confirms the direct child's reap; the owner joins it for at most
    `REPL_RESTART_WAIT`. A timeout or a failed retirement is returned as an error, no replacement starts, and what has not
    finished stays owned; a retirement that finished with an error is reported once and then dropped, so the next restart
    can start one. Limit: the termination is requested of the tree, not confirmed for every descendant (containment's
    guarantee).
R6. **A browser page's loopback port is granted to the proxy before its frame is forwarded, for one child's life, and an
    older child cannot grant or revoke for a newer one.** A `browser` frame is a child announcing a loopback server
    (`wglshow`, a user's dashboard). The REPL applies `PortEvent::Allow { owner, generation, port }` through its
    `PortGrants` capability and waits for the answer before it publishes the frame; `End { owner, generation }` goes
    when the child's death is applied and when a respawn opens. `owner` is the row's id, `generation` is the child's
    spawn generation, taken from one counter for the process so a row's later REPL handle starts above the last. A
    receiver refuses a generation below the one it holds for the owner (`PortRefused::Stale`); a refused or failed
    `Allow` withholds the frame and puts a `stderr` frame naming the reason in its place, so the failure is seen at
    the run, never silent; an `End` that fails is logged. Pages owns the receiver (`pages::proxy`), the table and the
    cap; sidecars owns the event type and the capability and names nothing of pages. Limits: the daemon installs one
    receiver for the process (`sidecars::ports::install`, at its start), and a REPL without one refuses every
    `Allow`; a port is granted for the child's life, not for the page's, so a port the child freed and another process
    took stays dialable until the child ends; at most 16 ports per owner are kept, the smallest evicted first, possibly
    the newly announced one; the table keeps one empty entry per owner that has ever announced, until the daemon ends.
R7. **Output reaches the window as `repl.frame`s, and a collected run is bounded.** Frames pass through verbatim, one
    per line, off the child's stdout (guarantee 10 for its bytes). A `repl.execute` collector keeps every frame until
    the stdout and stderr text it holds reaches `EXEC_TEXT_CAP` (256 KiB), keeps whole the frame that crosses it and drops
    later text frames (`truncated`); value, image, error and done frames are always kept. A `value` text and the first
    `error` message are cut at `EXEC_FIELD_CAP` (64 KiB) with a marker; other fields, streamed frames and images are not
    capped. Limit: the 1 MiB envelope bounds a reply, and a run whose frames exceed it fails there.

Item 41 (a row's REPL outlives a daemon restart) is not built and not ordered; the result file says what it would take.

## Owns
- Per row a `Kernel` and a `Repl` handle (the row builds them lazily, `Workspace::kernel`, `Workspace::repl`); the
  child processes and their supervisors' state: kernel generations and backoff, REPL state and number memory, Pluto's
  server, queue and grant, MathJax's child and queue, the monitor's samplers and their sample history.
- Each child's temporary folder, the process's held folder of them, and each user-code child's display and its cookie.
- The julia choice (`julia::resolve_bin`), the Julia projects the children run (`julia/kernel`, `julia/repl`,
  `julia/pluto`), the `FileType` plugins the kernel loads (`julia/plugins`), `core/` (the plugin ABI the kernel hosts)
  and the MathJax script (`rust/backend/sidecars/mathjax/render.mjs`, which never moves).
- The wire of each child: kernel NDJSON and `KERNEL_PROTOCOL_VERSION`; Pluto's `READY`, `OPEN`, `URL` and `ERR` lines
  (`julia/pluto/start.jl`); MathJax's `{"ready":true}` and `{id, tex, display}` lines (`render.mjs`); the REPL's frames.
  A shape changes together with its other side.

## Calls in
`sot_containment` (`Signal`, `Signal::spawn`, `Contained`, `Contained::wait_until_exited`, `process`), `sot_platform`
(`HeldDir`, `remove_ended`, `Redial`, `STABLE`, `host_name`, `state_dir_hash`, `resource_dir`, `sot_state_dir`,
`sot_config_dir`, `SSH_OPTS`), `sot_protocol` (`ChildLines`, the op names and payloads,
`page_url::loopback_port_from_url`, `reply::HandlerOutput`), rows (`Workspaces::resolve`, `row_or_reply`,
`capsule_guard`, for the REPL's ops), files (`canonicalize_within_any_workspace`, for `pluto.open`) and the server's
`Session` (the revision a reply carries). The REPL names no module of pages: its port announcements go through `ports::PortGrants`, which pages implements and the
daemon installs (R6).

## Calls out
`Kernel::request` (files' previews), `Kernel`, `KernelUnavailable`, `Repl` and `ReplFrameMsg` (rows, files, the server),
the handlers in `ops.rs` and `repl/` (the server's op table), `Pluto::server_up` (rows' `workspace.list`),
`bound_pluto_port` (pages' proxy), `julia::resolve_bin` (pages' quarto, the updater), `child::remove_own` and
`child::sweep_gone` (lifecycle's terminal, the server's start), `child::last_lines` (pages' quarto), `MonitorHub` and
`monitor.tick` (rows' registry, the server's tick bus). One edge stays to undo for the package page's cut: the server's `repl_numbers` handed to the REPL's handlers.
Pages depends on sidecars (`bound_pluto_port`) and implements `PortGrants`; sidecars does not name pages.

## Does not
Run user code in the kernel. Serve a page to the browser (pages). Know a row beyond the workspace id its caller names
(rows). Decide when a daemon or a row ends (lifecycle and rows ask; a child's owner ends it). Discover notebooks. Start
a child outside `Signal::spawn`.

## Platforms
Same on all three except: the per-child display and its holders (Linux only); the sampler of the daemon's own host
(Linux only; a remote host's sampler runs wherever the ssh reaches); the julia choice's app-bundle and `APPEXECLINK`
rules (macOS bundles, Windows aliases); the child folder's variables (`TMPDIR` or `TMP` and `TEMP`); contained groups
(Unix process groups, Windows jobs). The MathJax child needs node on the daemon's PATH or `SOT_NODE_BIN`; where it is
missing a render fails with that reason.

## Files
- `mod.rs`: the modules and `WireRequest`, the request line a kernel or REPL child reads.
- `julia.rs`: which julia runs. `kernel.rs`: the per-row kernel and its supervisor. `pluto.rs`: the per-daemon Pluto.
  `mathjax.rs`: the per-daemon MathJax child. `monitor.rs`: the host monitor. `ops.rs`: the handlers.
- `child/`: what each child carries (`tmp.rs`, `stderr_tail.rs`, and `display/` on Linux with its `holders.rs`).
- `repl/`: the per-row REPL (`mod.rs` the handle and slot, `supervisor.rs` the child's task, `lifecycle.rs`, `escalation.rs`,
  `execute.rs` and `ops.rs` the handlers, `numbers.rs` a window's runs).
- `ports.rs`: the port-announcement capability (`PortEvent`, `PortGrants`, `PortSink`) and its one installed receiver.
- `contract_tests.rs`: the fixtures the tests share (the X11 probes, `executable`, `isolated`, `pin_env`, `within`) and
  the native observations of the private `Signal` and of the MathJax helper (it listens nowhere); no source-text
  assertions.

## Start here
kernel.rs `run_one_generation` for a kernel's life; julia.rs `resolve_bin` for which julia runs; monitor.rs `supervise`
for a sampler's life; child/tmp.rs `retire` for how a child ends.

## Rules
- A supervisor uses the `Signal` it was given for spawn and `fired()`; it never selects a different shutdown signal
  inside its service loop.
- A test that finds julia or node through the process environment (`contract_tests::executable`, `julia::resolve_bin`)
  runs its body in `isolated`, or holds `paths::ENV_TEST_LOCK` from that read through its spawn; the ignored Windows
  julia test in `rust/containment/src/contain_tests.rs` instead runs alone (`--test-threads=1`) in its own CI job.
- A Julia child gets its folder and its display from `ChildTmp` and nowhere else.

## Tests
One per guarantee. Paths are under `rust/backend/src/sidecars/` unless they start `julia/`, `rust/` or `tests/`.

| # | guarantee | test |
|---|---|---|
| 1 | julia choice | `julia_tests.rs`: `sot_julia_bin_wins_outright_when_absolute`, `sot_julia_bin_relative_is_a_hard_error`, `falls_back_to_juliaup_default_channel_when_no_override`, `falls_back_to_path_when_juliaup_absent`, `every_path_candidate_rejected_is_an_error_not_a_bare_name_fallback`, `no_julia_anywhere_is_an_error_not_a_bare_name`, `a_juliaup_default_whose_binary_is_gone_is_a_refusal_not_a_miss`, `an_app_bundle_wins_over_bin_when_both_layouts_exist`; Windows: `path_skips_alternate_alias`, `alternate_alias_override_is_refused` |
| 2 | no user environment, root as data | `kernel_tests.rs`: `the_kernel_spawn_passes_startup_file_no`, `missing_kernel_project_reports_dead_without_spawning`; `kernel_root_tests.rs` (a real julia): `a_workspace_root_reaches_the_kernel_as_data_not_as_source`, `the_kernel_s_load_path_holds_no_user_environment` |
| 3 | a caller's bound | `kernel_deadline_tests.rs`: `a_full_channel_still_answers_within_the_bound`, `a_wedged_kernel_is_named_and_replaced`; `kernel_tests.rs`: `kernel_unavailable_display_is_composable`; `ops_tests.rs`: `a_kernel_that_is_unavailable_answers_its_own_code_for_each_way_it_is`, `a_kernel_request_that_fails_for_another_reason_is_not_called_unavailable` |
| 4 | replaced on a backoff | `kernel_tests.rs`: `a_kernel_that_answers_and_dies_is_respawned_on_the_doubling_wait`, `a_kernel_whose_precompile_outlasts_stable_without_serving_backs_off`; `kernel_deadline_tests.rs`: `a_kernel_that_never_says_hello_is_replaced`, `a_kernel_answering_slowly_but_steadily_is_kept` |
| 5 | Pluto's start and open bounds | `pluto_open_bound_tests.rs`: `an_open_the_child_never_answers_fails_at_its_bound` (the next open starts a second server), `an_open_whose_server_fails_its_boot_answers_with_its_stderr`, `an_open_behind_a_server_still_starting_says_so_and_the_next_joins_it`; `pluto_port_parse_tests.rs`: `a_pluto_ready_after_the_old_180s_bound_is_served` |
| 6 | the server's one fact | `pluto_open_bound_tests.rs`: `the_server_is_up_from_its_start_until_it_ends`, `the_server_is_up_while_it_is_still_starting` |
| 7 | the proxy grant | `pluto_port_parse_tests.rs`: `death_releases_proxy_grant`, `inherited_stdout_does_not_keep_grant`, `nonloopback_ready_grants_nothing`; pages' side: `src/pages/proxy_tests.rs` `expired_pluto_grant_refuses_its_port` |
| 8 | cancel and stall | `pluto_port_parse_tests.rs`: `blocked_write_death_releases_grant_and_closes_requests`, `flush_blocked_death_releases_grant_and_closes_requests`, `flush_error_releases_grant_and_closes_requests`, `signal_cancels_blocked_pluto_write`, `a_request_pushed_after_the_close_is_answered` |
| 9 | MathJax's bounds | `mathjax_tests.rs`: `a_render_past_its_bound_is_an_error_and_ends_its_child`, `a_child_never_ready_is_ended_at_its_start_bound`, `a_mathjax_child_that_died_is_replaced_by_the_next_render`; `contract_tests.rs`: `a_slow_start_is_not_counted_against_the_first_render` |
| 10 | output is bytes | `kernel_stream_tests.rs`: `a_kernel_reply_holding_an_invalid_byte_keeps_its_generation`, `a_kernel_stderr_line_holding_an_invalid_byte_keeps_its_generation`; `kernel_tests.rs`: `a_kernel_line_that_does_not_parse_is_logged_without_its_bytes`; `pluto_port_parse_tests.rs`: `a_pluto_line_holding_an_invalid_byte_keeps_the_server`; `mathjax_tests.rs`: `a_mathjax_line_that_does_not_parse_is_logged_without_its_bytes`; the REPL's: `repl/ops_interrupt_tests.rs` `a_repl_line_holding_an_invalid_byte_keeps_the_child` |
| 11 | the stderr tail | `child/stderr_tail_tests.rs`: `a_child_s_last_lines_are_logged_at_warn`, `a_starting_child_is_named_with_its_age_and_last_lines`; `kernel_stream_tests.rs`: `a_kernel_that_fails_its_start_logs_its_stderr_at_warn`; `mathjax_tests.rs`: `a_mathjax_child_that_fails_its_start_logs_its_stderr_at_warn`; `pluto_port_parse_tests.rs`: `a_pluto_that_fails_its_boot_logs_its_stderr_at_warn`; `contract_tests.rs`: `a_repl_child_that_fails_its_start_logs_its_stderr_at_warn` |
| 12 | the `Signal` ends every child | `kernel_tests.rs`: `shutdown_kills_the_kernel_child_and_never_respawns`, `blocked_kernel_write_does_not_outlive_shutdown`, `status_channel_reports_closed_once_every_kernel_handle_drops`; `pluto_port_parse_tests.rs`: `shutdown_kills_the_pluto_child_during_the_ready_wait`, `pluto_grandchild_dies_with_shutdown`, `dropping_the_handle_during_a_start_ends_the_child`; `contract_tests.rs`: `mathjax_uses_private_signal`, `repl_uses_private_signal`, `mathjax_helper_tree_listens_nowhere`; `monitor_tests.rs`: `monitor_backoff_returns_on_shutdown` |
| 13 | the sampler | `monitor_tests.rs`: `a_silent_sampler_is_restarted`, `a_sampler_that_prints_nothing_is_restarted`, `a_sampler_whose_lines_do_not_parse_is_restarted`, `a_remote_sampler_turns_ssh_sharing_off`, `a_process_name_with_any_bytes_keeps_its_host_sampled`, `a_process_name_with_a_newline_is_read_whole`, `only_the_hub_executes_the_monitor_roster`, `only_a_linux_daemon_samples_itself`, `a_box_with_no_hub_declaration_samples_itself`, `a_repeated_failure_reason_is_logged_once`, `query_picks_minute_tier_for_wide_window` |
| 14 | the child's folder | `child/tmp_tests.rs`: `retire_removes_the_folder_after_the_reap`, `the_held_folder_comes_with_the_first_child_folder_and_goes_with_the_last`; `kernel_tests.rs`: `a_generation_removes_its_temporary_folder_after_the_reap`; `pluto_port_parse_tests.rs`: `a_killed_child_leaves_no_temp_files`; `tests/child_folders.rs` (the daemon's start and end) |
| 15 | the display | `child/tmp_tests.rs`: `every_end_of_a_user_code_child_ends_the_processes_that_hold_its_display`; `child/display/tests.rs`: `a_display_opens_only_to_its_cookie_and_ends_with_its_owner`, `a_fired_signal_ends_a_display`, `a_display_whose_server_exits_is_seen_to_end`, `a_display_ends_every_process_that_holds_its_cookie`, `a_missing_display_names_its_reason_to_the_child`; `child/display/holders_tests.rs`: `a_holder_started_after_the_first_listing_still_ends`, `a_live_process_that_holds_no_cookie_is_not_signalled`; `pluto_port_parse_tests.rs`: `pluto_has_its_own_display_which_ends_with_it`, `a_pluto_whose_display_ends_ends_with_it`, `a_pluto_whose_display_ends_before_ready_is_not_served`; the REPL's: `repl/project_tests.rs` `a_repl_child_has_its_own_display_which_ends_with_it`, `a_repl_whose_display_ends_ends_with_it` |
| 16 | a secret before a page | `julia/pluto/test/runtests.jl`: the session secret, the cluster cookie, the stranger's call and `start.jl end to end` testsets; the REPL's `wglshow`: `julia/repl/test/runtests.jl` `wglshow serves its page only on the Bonito line it was built against`; Rust side `repl/project_tests.rs` `repl_page_secret_never_reaches_command_line` |
| 17 | confined, flavored, carriable | `ops_tests.rs`: `pluto_open_outside_every_row_is_refused_and_starts_no_server`, `pluto_open_of_a_file_without_pluto_s_first_line_is_refused_and_starts_no_server`; `pluto_open_bound_tests.rs`: `a_path_with_a_control_character_is_refused_and_reaches_no_child`; `julia/pluto/test/runtests.jl`: `a path ending in a space opens that file, not its trimmed name` |
| 18 | `math.render` | `ops_tests.rs`: `a_failed_math_render_answers_its_code_and_no_blob`; `a_math_render_answers_the_svg_as_a_blob_with_its_ex_height` (the real MathJax child; needs `npm ci`, as `mathjax_uses_private_signal` does) |
| 19 | plugins and core | `core/test/runtests.jl:101` (the external-tool bound); the rest is held by reading the cited lines, no test |
| R1 | one child, lazily, replaced | `repl/respawn_after_death_tests.rs`: `submit_after_instant_child_death_respawns`, `submit_survives_a_launcher_that_leaves_a_grandchild`, `spawn_failure_is_returned_immediately`, `repl_project_is_resolved_at_every_spawn`; `repl/restart_tests.rs`: `restart_reaps_before_replacement`, `selected_project_survives_death`; `repl/supervisor_interrupt_guard_tests.rs`: `request_if_running_never_spawns`; `repl/project_tests.rs`: `bare_workspace_is_active`, `repl_child_arguments_and_environment_match_selected_project`; `repl/lifecycle_tests.rs`: `begin_starting_bumps_gen_sets_state_and_announces`, `stale_generation_cannot_stomp_a_respawned_child`, `current_generation_death_is_reported`, `wire_words_match_protocol_vocabulary`; Windows: `tests/repl_windows.rs` (the real julia, a row's REPL end to end) |
| R2 | stated bounds, a full queue refused | `repl/ops_busy_tests.rs`: `a_full_queue_is_refused_at_once`, `an_unanswered_interrupt_ends_at_its_bound`; `repl/execute_tests.rs`: `a_run_behind_a_repl_still_starting_says_so`, `a_run_the_child_never_answers_ends_at_its_timeout` |
| R3 | every run closed out once | `repl/ops_fresh_tests.rs`: `a_run_waiting_when_the_repl_dies_is_answered_died`, `a_run_waiting_when_fresh_restarts_the_repl_is_answered_and_closed`, `a_windows_run_is_named_running_from_its_write_until_its_end`; `repl/restart_tests.rs`: `channel_close_finishes_streamed_runs`, `a_submission_pushed_after_the_close_is_answered`; `repl/respawn_after_death_tests.rs`: `a_child_that_answers_then_exits_still_delivers_its_answer`; `repl/numbers_tests.rs`: `a_run_is_marked_ended_before_its_done_is_published` |
| R4 | interrupt reaches only the running eval; non-yielding evals by signal | `repl/escalation_tests.rs`: all (`it_signals_only_the_running_eval_the_request_names`, `a_forced_request_for_the_same_eval_replaces_a_plain_escalation`, `a_forced_burst_ends_at_its_span_or_at_the_childs_first_line`, `each_answer_says_what_is_left`); `repl/interrupt_tests.rs`: `one_interrupt_stops_a_loop_with_a_safepoint`, `a_loop_with_no_safepoint_needs_the_force`, `an_interrupt_naming_another_eval_sends_no_signal`; `repl/ops_busy_tests.rs`: `an_interrupt_the_signal_does_not_stop_says_so`, `an_idle_repl_is_never_signalled`, `no_signal_follows_the_evals_done`; `julia/repl/test/interrupt_child.jl` (local gate and release gate) |
| R5 | checked retirement | `repl/restart_tests.rs`: `restart_timeout_retains_owner_and_starts_no_replacement`, `cleanup_error_prevents_replacement` |
| R6 | the port channel | `repl/ports_tests.rs`: `an_allow_is_applied_before_its_browser_frame_is_forwarded`, `a_refused_allow_withholds_the_frame_and_says_why`, `an_end_a_receiver_refuses_is_logged_and_changes_nothing`, `a_repl_with_no_receiver_refuses_every_allow`, `a_new_repl_handle_starts_above_the_last_generation`; `repl/restart_tests.rs`: `retired_generation_cannot_grant`; pages' side, `src/pages/proxy_tests.rs`: `a_stale_generation_is_refused_by_the_table`, `an_older_childs_end_cannot_erase_its_replacements_grant`, `browser_ports_record_revoke_and_allowlist` |
| R7 | output and its caps | `repl/execute_tests.rs`: `repl_execute_reports_are_unchanged`, `a_collected_run_keeps_whole_the_frame_that_crosses_the_text_cap`, `a_field_past_its_cap_is_cut_and_marked`; `repl/numbers_tests.rs`: the window's-numbers tests |

## Sidecars queue
Seen at pass 8a and its review round, handled at the sidecars' next pass; none is a defect seen in use. The REPL's follow the
guarantees' table. Each names the file, the scenario and the fix.
- `julia.rs:355` and `:84`: an empty or relative PATH entry can give a relative answer; a broken juliaup default falls
  through to PATH. Fix: refuse a non-absolute candidate; decide the default's fallthrough.
- `kernel.rs:294`, `:643`: queued submissions at a generation's death get the generic error. Fix: drain the queue with
  `Dead` as `pending` is.
- `kernel.rs:543`, `:455`: no `wait_until_exited` in the service loop. Scenario: a descendant holds stdout of a dead idle
  kernel. Fix: a select arm on the leader's exit.
- `pluto.rs:286`, `:117`: no exit observation before `READY`; the start's construction is outside the supervisor's
  deadline; the cleanup's reap is unbounded. Fix: observe exit in `await_ready` as the serving loop does; bound the reap.
- `pluto.rs:135`, `mathjax.rs:132`, `:284`, `:315`: deadlines live in the caller's future, and MathJax's blocked write
  and death detection (stdout EOF) ignore the `Signal` and the leader's exit. Fix: supervisor-owned deadlines; select the
  `Signal` and `wait_until_exited` around writes.
- `monitor.rs:670`, `:545`: the sampler script's delivery has no deadline and precedes the silence clock. Fix: race it
  against the `Signal` and a bound, keeping the child for checked cleanup.
- `mathjax.rs:253`: a lossily decoded first line fails MathJax's ready check. Fix: compare the parsed ready object.
- `kernel.rs:419`, `mathjax.rs:138`: startup errors omit the captured stderr tail from the answer.
- `ops.rs:153`, `:160`: no regular-file check or deadline before the header read (a FIFO parks `pluto.open`); the header
  check is a substring. Fix: `metadata().is_file()` first and an exact-line compare; pin the file by descriptor.
- `update.rs:63`, `child/tmp.rs:116`, `rust/containment/src/signal.rs:483`: cancelled updater calls drop the folder
  without a reap; removal's result is not awaited; `kill` has no wall-clock ceiling. Fix: containment's.
- `child/display/holders.rs:53`: signals are not confirmed deaths; unreadable environments and failed pidfd opens are
  only logged. Fix: report the count that failed and wait for the pidfds with a bound.
- A cgroup per child (queue item 25's W3) is containment's and unbuilt.
- No test proves the sampler script's avoidance of here-documents; a behavioral proof needs a leak check under bash 5.2.
- D3's MathJax question (keep the node child only if markdown with math is previewed in use) waits for the
  maintainer's answer; the MathJax code is unchanged.
