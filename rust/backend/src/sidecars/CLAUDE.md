# rust/backend/src/sidecars: sidecars (charter)

## Idea
Each helper process the daemon relies on is owned by one daemon task from spawn to reap and is spoken to over its own
stdio. Callers submit and wait; they never spawn, kill or retry.

## Owns
- Per row a Julia kernel (`Kernel`, kernel.rs) and a Julia REPL (`Repl`, repl/).
- Per daemon one Pluto (`Pluto`, pluto.rs) and one MathJax node child (`MathJax`, mathjax.rs).
- One sampler per monitored host (`MonitorHub::start` and `supervise`, monitor.rs).
- The julia choice: `julia::resolve_bin` takes an absolute `SOT_JULIA_BIN`, then juliaup's default channel, then a verified PATH candidate, never a bare `julia`. On Windows the selected existing file is inspected through ordinary filesystem links: `IO_REPARSE_TAG_APPEXECLINK` and inspection errors are refused; a directory called `WindowsApps` is not evidence of an alias. PATH skips refused candidates; an explicit/default-channel alias is an error. A missing explicit absolute path still fails at spawn. The kernel, the REPL, Pluto, quarto (`run_quarto`) and the update prepare (`prepare_julia`) all run its answer.

## Promises
- A kernel caller waits at most `KERNEL_REQUEST_TIMEOUT` (10 s, `Kernel::request`) and never spawns or kills.
- `supervisor_loop` respawns a dead kernel with a backoff from 250 ms doubling to 30 s on platform's `Redial`, started
  over only after a kernel that served `STABLE` (60 s) after its hello: a kernel that never answers, or answers and then
  dies, keeps the doubling however long its precompile ran (`run_one_generation` returns when it answered).
- Pluto and MathJax respawn on the next call after a death (`ensure_supervisor`).
- Pluto's proxy port is a supervisor-owned generation grant, published only after a loopback READY URL and released on every supervisor exit or cancellation before cleanup awaits. An old generation's release cannot erase its replacement's grant (`bound_pluto_port`).
- Pluto polls child exit and its supplied Signal during each stdin write and flush. Cancelling a submission retires the supervisor, releases its grant and closes current, pending and queued replies before checked cleanup; it never resends a partial OPEN line.
- A REPL restart explicitly retires and joins its owned supervisor before replacement; it never relies on Julia reaching stdin EOF. Errors retain retirement ownership and prevent replacement.
- Each Julia child (kernel, REPL, Pluto) runs with a temporary folder of its own in the daemon's temporary folder (`ChildTmp`: `TMPDIR`; `TMP` and `TEMP` on Windows). Its owner ends the child and reaps it, then removes the folder (`ChildTmp::retire`), so a killed child and its descendants in its process group (its job on Windows) leave no temporary file. Limits: a daemon close or crash ends without reaping the living children, so their folders stay in the system temporary folder until the OS clears it (on some systems only at boot); a descendant that leaves the child's process group on Unix (Pluto's notebook worker after a server crash, a detached process) runs on without its folder (ADR 0050 residual 7).
- Every sidecar child starts through `Signal::spawn`. Kernel, REPL, Pluto, MathJax and monitor supervisors receive a caller-supplied `&'static Signal` for both spawn and shutdown observation; the monitor's respawn backoff ends at its fire. REPL and MathJax select the daemon's process `Signal` only where their production handles are constructed.
- A remote host's sampler ssh is built from `SSH_OPTS`, so it turns ssh sharing off as the bridges do
  (`sampler_command`).
- The kernel runs only its own `julia/kernel` project (`run_one_generation`).
- A dead monitor source shows as a `stale` tick and respawns after 5 s; its reason is logged once per change
  (`should_log`).
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
`revoke_browser_ports`, `loopback_port_from_url`, `rust/protocol/src/page_url.rs`, `ensure_proxy_for_url`.

## Folders
- `core/`: the plugin ABI the kernel hosts.
- `julia/kernel/`, `julia/repl/`, `julia/pluto/`: the Julia projects the children run.
- `julia/plugins/`: the FileType plugins the kernel loads.
- `rust/backend/sidecars/mathjax/`: the MathJax script (`render.mjs`); never moved.

## Files
- `mod.rs`: declares the eight modules and `WireRequest`.
- `child_tmp.rs`: a Julia child's own temporary folder (`ChildTmp`), made before its spawn; `retire` ends the child and removes the folder after its reap.
- `contract_tests.rs`: native REPL/MathJax private-Signal spawn, shutdown closeout and owned-child observations, and the Linux process-tree listener observer (the MathJax helper listens nowhere; the observer rejects a listening node tree); no source-text assertions.
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
  Pluto's `READY`/`OPEN`/`URL`/`ERR` lines with julia/pluto/start.jl; MathJax `{id, tex, display}` with
  rust/backend/sidecars/mathjax/render.mjs.
- A supervisor uses the `Signal` it was given for spawn and `fired()`; it never selects a different shutdown signal inside its service loop.
- A test that finds julia or node through the process environment (`contract_tests::executable`, `julia::resolve_bin`)
  runs its body in `isolated`, or holds `paths::ENV_TEST_LOCK` from that read through its spawn; the ignored Windows
  julia test in `lifecycle/contain.rs` instead runs alone (`--test-threads=1`) in its own CI job.
