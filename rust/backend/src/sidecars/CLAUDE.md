# rust/backend/src/sidecars: sidecars (charter)

## Idea
Each helper process the daemon relies on is owned by one daemon task from spawn to reap and is spoken to over its own
stdio. Callers submit and wait; they never spawn, kill or retry.

## Owns
- Per row a Julia kernel (`Kernel`, kernel.rs) and a Julia REPL (`Repl`, repl/).
- Per daemon one Pluto (`Pluto`, pluto.rs) and one MathJax node child (`MathJax`, mathjax.rs).
- One sampler per monitored host (`MonitorHub::start` and `supervise`, monitor.rs).
- The julia choice: `julia::resolve_bin` takes `SOT_JULIA_BIN`, then juliaup's default channel, then a verified PATH
  entry.

## Promises
- A kernel caller waits at most `KERNEL_REQUEST_TIMEOUT` (10 s, `Kernel::request`) and never spawns or kills.
- `supervisor_loop` respawns a dead kernel with a backoff from 250 ms doubling to 30 s.
- Pluto and MathJax respawn on the next call after a death (`ensure_supervisor`).
- The shutdown signal kills every child: kernel and Pluto select on the `Signal` passed in, MathJax and the monitor on
  `crate::lifecycle::child_signal::fired()`.
- The kernel runs only its own `julia/kernel` project (`run_one_generation`).
- A dead monitor source shows as a `stale` tick and respawns after 5 s; its reason is logged once per change
  (`should_log`).
- Only the hub runs the declared `[monitor]` roster, read once at start (`load_hosts`, `sampling_roster`); only a Linux
  daemon samples itself (`without_local_sampler`).

## Connections
- ops.rs and repl/ serve `repl.*`, `kernel.request`, `math.render`, `pluto.open` and `monitor.*`, and
  files/preview calls `Kernel::request` for plugin previews.
- server/mod.rs builds `MathJax`, `Pluto` and `MonitorHub`; server/dispatch.rs serves `monitor.*` inline through ops.rs.
- rows/workspace.rs holds a `Kernel` and a `Repl` per row.
- The page proxy's allowlist (pages/proxy.rs) reads `bound_pluto_port`.
- `paths::resource_dir` finds julia/kernel, julia/pluto and the MathJax script.

## Folders
- `core/`: the plugin ABI the kernel hosts.
- `julia/kernel/`, `julia/repl/`, `julia/pluto/`: the Julia projects the children run.
- `julia/plugins/`: the FileType plugins the kernel loads.
- `rust/backend/sidecars/mathjax/`: the MathJax script (`render.mjs`); never moved.

## Files
- `mod.rs`: declares the seven modules and `WireRequest`.
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
- The kernel resolves julia at every generation (`julia_bin` in kernel.rs); Pluto and MathJax resolve their binary once,
  in `Pluto::new` and `MathJax::new`.
- A wire shape changes together with its other side: kernel NDJSON and `KERNEL_PROTOCOL_VERSION` with julia/kernel;
  Pluto's `READY`/`OPEN`/`URL`/`ERR` lines with julia/pluto/start.jl; MathJax `{id, tex, display}` with
  rust/backend/sidecars/mathjax/render.mjs.
- The shutdown wiring exists in two forms (kernel and Pluto take the `Signal`; MathJax and the monitor use
  `ChildGuard` and `fired`); change one, check the other.
