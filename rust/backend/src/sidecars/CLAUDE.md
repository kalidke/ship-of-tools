# rust/backend/src/sidecars: sidecars (charter)

## Idea
Each helper process the daemon relies on is owned by one daemon task from spawn to reap and is spoken to over its own
stdio. Callers submit and wait; they never spawn, kill or retry.

## Owns
- Per row a Julia kernel (`Kernel`, kernel.rs) and a Julia REPL (`Repl`, repl/).
- Per daemon one Pluto (`Pluto`, pluto.rs) and one MathJax node child (`MathJax`, mathjax.rs).
- On Linux, per REPL and Pluto child its own X display (`Display`, display/), held by the child's `ChildTmp`.
- One sampler per monitored host (`MonitorHub::start` and `supervise`, monitor.rs).
- The julia choice: `julia::resolve_bin` takes an absolute `SOT_JULIA_BIN`, then juliaup's default channel, then a verified PATH candidate, never a bare `julia`. On Windows the selected existing file is inspected through ordinary filesystem links: `IO_REPARSE_TAG_APPEXECLINK` and inspection errors are refused; a directory called `WindowsApps` is not evidence of an alias. PATH skips refused candidates; an explicit/default-channel alias is an error. A missing explicit absolute path still fails at spawn. The kernel, the REPL, Pluto, quarto (`run_quarto`) and the update prepare (`prepare_julia`) all run its answer.

## Promises
- A kernel caller waits at most `KERNEL_REQUEST_TIMEOUT` (10 s, `Kernel::request`) and never spawns or kills.
- `supervisor_loop` respawns a dead kernel with a backoff from 250 ms doubling to 30 s on platform's `Redial`, started
  over only after a kernel that served `STABLE` (60 s) after its hello: a kernel that never answers, or answers and then
  dies, keeps the doubling however long its precompile ran (`run_one_generation` returns when it answered).
- A kernel request, its send included, ends within `KERNEL_REQUEST_TIMEOUT`; a running kernel's timeout says `the running kernel did not answer <op>`, a starting one's `kernel still starting`. A kernel that answers nothing for `KernelBounds::answer` (300 s, counted from the later of the oldest request's write and the kernel's last answer), or not its hello for `start` (1800 s), is ended with that reason and started again after the backoff.
- Pluto and MathJax respawn on the next call after a death (`ensure_supervisor`).
- A MathJax render waits up to 60 s (`START_BOUND`) for its node child's ready line, which node writes once its
  modules are loaded, and then answers within 10 s (`RENDER_BOUND`), so a cold start is not counted against a render;
  renders that arrive during a start share it. Past either bound the render is a `mathjax_render_failed` error naming
  it, and its node child is ended, even mid-write, so the next render starts a fresh one (`MathJax::render`,
  `supervisor_task`).
- A Pluto open answers within `PLUTO_OPEN_BOUND` (30 s) or fails naming it, and the server that did not answer is ended, since its replies are matched oldest-first.
- Pluto's proxy port is a supervisor-owned generation grant, published only after a loopback READY URL and released on every supervisor exit or cancellation before cleanup awaits. An old generation's release cannot erase its replacement's grant (`bound_pluto_port`).
- Pluto polls child exit and its supplied Signal during each stdin write and flush. Cancelling a submission retires the supervisor, releases its grant and closes current, pending and queued replies before checked cleanup; it never resends a partial OPEN line.
- A REPL restart explicitly retires and joins its owned supervisor before replacement; it never relies on Julia reaching stdin EOF. Errors retain retirement ownership and prevent replacement.
- Each Julia child (kernel, REPL, Pluto) runs with a temporary folder of its own in the daemon's temporary folder (`ChildTmp`: `TMPDIR`; `TMP` and `TEMP` on Windows). Its owner ends the child and reaps it, then removes the folder (`ChildTmp::retire`), so a killed child and its descendants in its process group (its job on Windows) leave no temporary file. Limits: a daemon close or crash ends without reaping the living children, so their folders stay in the system temporary folder until the OS clears it (on some systems only at boot); a descendant that leaves the child's process group on Unix (Pluto's notebook worker after a server crash, a detached process) runs on without its folder (ADR 0050 residual 7).
- On Linux each REPL and Pluto child starts with an X display of its own (`ChildTmp::for_user_code`,
  `Display::start`), which Pluto's notebook workers inherit: an Xvfb on the lowest free display number that only the
  child's cookie opens, so no other account can. It ends after the child's reap (`ChildTmp::retire`: a restart, a
  `--fresh`, the child's death, its row's end), at the Signal's fire and with its owner's drop, and leaves no process,
  socket file or lock behind. A start that fails says why (the daemon's log, `SOT_NO_DISPLAY`, the REPL's first eval)
  and the child starts without one. The kernel gets none. Tests `a_repl_child_has_its_own_display_which_ends_with_it`
  (repl/project_tests.rs), `pluto_has_its_own_display_which_ends_with_it` (pluto.rs) and display/'s own.
- Every sidecar child starts through `Signal::spawn`. Kernel, REPL, Pluto, MathJax and monitor supervisors receive a caller-supplied `&'static Signal` for both spawn and shutdown observation; the monitor's respawn backoff ends at its fire. REPL and MathJax select the daemon's process `Signal` only where their production handles are constructed.
- A remote host's sampler ssh is built from `SSH_OPTS`, so it turns ssh sharing off as the bridges do
  (`sampler_command`).
- A monitor sampler silent for `SAMPLER_SILENT` (20 s) is ended, shown stale, and started again after `RESPAWN_BACKOFF`.
- The kernel runs only its own `julia/kernel` project (`run_one_generation`).
- A dead monitor source shows as a `stale` tick and respawns after 5 s; its reason is logged once per change
  (`should_log`).
- A process name reaches the daemon as `/proc` gives it: the sampler's awk joins a stat line a `\n` in the name split
  (`SAMPLER_SH`), and the daemon decodes each line lossily and makes its control characters spaces before parsing it
  (`ChildLines`, `supervise`; tests `a_process_name_with_any_bytes_keeps_its_host_sampled`,
  `a_process_name_with_a_newline_is_read_whole`, Linux only).
- A child's output is bytes: every sidecar child's stdout and stderr are read through the wire's `ChildLines`, which
  decodes each line lossily (an invalid sequence becomes U+FFFD), so no byte a child writes ends its generation or stops
  its stderr drain. The children may write any byte (the kernel's JSON3 writes a string's bytes as they are) and none
  re-encodes. Tests `a_kernel_reply_holding_an_invalid_byte_keeps_its_generation` and
  `a_kernel_stderr_line_holding_an_invalid_byte_keeps_its_generation` (kernel.rs),
  `a_repl_line_holding_an_invalid_byte_keeps_the_child` (repl/ops.rs) and
  `a_pluto_line_holding_an_invalid_byte_keeps_the_server` (pluto.rs).
- Only the hub runs the declared `[monitor]` roster, read once at start (`load_hosts`, `sampling_roster`); only a Linux
  daemon samples itself (`without_local_sampler`).
- The Julia children's page servers listen on 127.0.0.1 and answer nothing without a secret drawn from the OS's
  secure generator, which never reaches another account: Pluto serves every path under its session secret
  (`base_url`), so nothing, not even its own files or `/ping`, answers without it, each notebook worker checks the Distributed
  cluster cookie it read from its stdin before it reads a message, and a `wglshow` page sits behind its secret path,
  its websocket behind its session id, with its assets inside the page (`julia/pluto/session_options.jl`,
  `julia/repl/src/wgl.jl`).

## Connections
Each connection is one row of docs/integration.md, owned by its provider. Provides: `Kernel::request`, `file.preview`,
`repl.eval`, `repl.run_file`, `repl.interrupt`, `repl.execute`, `kernel.request`, `math.render`, `pluto.open`,
`monitor.subscribe`, `monitor.unsubscribe`, `monitor.history`, `repl.frame`, `monitor.tick`, `bound_pluto_port`,
`allowed_proxy_ports`, `Kernel`, `Repl`, `julia::resolve_bin`. Uses: `dispatch`, `SSH_OPTS`, `Redial`, `STABLE`, `Signal::spawn`, `Contained`, `Contained::wait_until_exited`, `Signal`,
`child_signal::process`, `Workspaces::resolve`, `row_or_reply`, `capsule_guard`, `sot_state_dir`, `sot_config_dir`,
`host_name`, `state_dir_hash`, `resource_dir`, `rust/backend/src/paths.rs`, `record_browser_port`,
`revoke_browser_ports`, `loopback_port_from_url`, `rust/protocol/src/page_url.rs`, `ensure_proxy_for_url`, `ChildLines`.

## Folders
- `core/`: the plugin ABI the kernel hosts.
- `julia/kernel/`, `julia/repl/`, `julia/pluto/`: the Julia projects the children run.
- `julia/plugins/`: the FileType plugins the kernel loads.
- `rust/backend/sidecars/mathjax/`: the MathJax script (`render.mjs`); never moved.

## Files
- `mod.rs`: declares the nine modules and `WireRequest`.
- `child_tmp.rs`: a Julia child's own temporary folder (`ChildTmp`), made before its spawn, and for the REPL and Pluto
  its display (`for_user_code`); `retire` ends the child, then the display, and removes the folder after its reap.
- `contract_tests.rs`: native REPL/MathJax private-Signal spawn, shutdown closeout and owned-child observations, a
  node child slower to start than a render's bound, and the Linux process-tree listener observer (the MathJax helper listens nowhere; the observer rejects a listening node tree); no source-text assertions. The X11 connection probe the display tests share (`x11_answer`, `x11_cookie`).
- `display/`: a user-code child's own X display (Linux; see its page).
- `julia.rs`: which julia binary runs (`resolve_bin`).
- `kernel.rs`: the per-row kernel and its supervisor.
- `repl/`: the per-row Julia REPL child.
- `pluto.rs`: the per-daemon Pluto child.
- `mathjax.rs`: the per-daemon MathJax child.
- `monitor.rs`: the host monitor, one sampler per host.
- `monitor_tests.rs`: the monitor's unit tests and roster-config tests.
- `ops.rs`: kernel.request, math.render, pluto.open, monitor.subscribe, monitor.unsubscribe, monitor.history

## Start here
kernel.rs `run_one_generation` for a kernel's life; julia.rs `resolve_bin` for which julia runs; monitor.rs `supervise`
for a sampler's life.

## Rules
- The kernel resolves julia at every generation (`julia_bin` in kernel.rs), Pluto at every start (`ensure_supervisor`)
  and the REPL at every spawn, and a resolver error fails the spawn; MathJax resolves node once, in `MathJax::new`.
- A wire shape changes together with its other side: kernel NDJSON and `KERNEL_PROTOCOL_VERSION` with julia/kernel;
  Pluto's `READY`/`OPEN`/`URL`/`ERR` lines with julia/pluto/start.jl; MathJax's `{"ready":true}` line and
  `{id, tex, display}` with rust/backend/sidecars/mathjax/render.mjs.
- A supervisor uses the `Signal` it was given for spawn and `fired()`; it never selects a different shutdown signal inside its service loop.
- A test that finds julia or node through the process environment (`contract_tests::executable`, `julia::resolve_bin`)
  runs its body in `isolated`, or holds `paths::ENV_TEST_LOCK` from that read through its spawn; the ignored Windows
  julia test in `lifecycle/contain.rs` instead runs alone (`--test-threads=1`) in its own CI job.
