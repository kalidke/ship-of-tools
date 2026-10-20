# julia/repl: the Julia REPL shim, one process per workspace (sidecars)

The shim runs user code. It speaks newline-delimited JSON over stdin/stdout and is stacked behind the workspace's own
project by `JULIA_LOAD_PATH`, so its `[deps]` are stdlib only, forever: a registered dependency would be shadowed by
the version a user's manifest pins. That is why `json.jl` exists, and a guard test in `test/runtests.jl` enforces it.
Part of the sidecars; charter: rust/backend/src/sidecars/CLAUDE.md.

## Files
- `Project.toml`: the package, stdlib dependencies only.
- `ext/ShipToolsReplWGLMakieExt.jl`: the WGLMakie package extension, the only place WGLMakie code lives.
- `src/ShipToolsRepl.jl`: the module, the child's entry `run_shim`, `serve` and its ops (eval, run_file, interrupt), `write_envelope`.
- `src/json.jl`: the stdlib-only JSON codec.
- `src/wgl.jl`: browser-served artifacts, `BrowserView`, `page_server`, private `wgl_server` listener selection and `wglshow`; `WGLDisplay`.
- `src/frames.jl`: how an eval's output becomes typed frames, the BrowserView announcements, the wire guard (`EvalStdout`, `point_stdout_away_from_wire!`) and `revise_loaded`.
- `test/runtests.jl`: the streaming tests and the stdlib-only guard test.
- `test/shim_child.jl`: the shim as the daemon starts it, a child process running `run_shim` with a scratch depot whose `config/startup.jl` is the user's startup file: the wire, the startup output, error rendering, the display stack, a failing startup file.
- `test/interrupt_child.jl`: the daemon's interrupt signal (SIGINT; on Windows a Ctrl+Break to the child's own console process group) against the shim as the daemon starts it: code that never yields is stopped, with a safepoint by one signal and without one by the forcing burst, its interrupt is answered `interrupted:true`, the REPL keeps its state, and (Julia 1.13 and later) a signal between evals is dropped.
- `test/stray_interrupt.jl`: the daemon's signal landing in the shim's own code after an eval's code ended: the answer frames and the done and res writes survive it, and two in a request's answer and its error answer leave `serve` serving.
- `test/answer_then_next.jl`: an output stream that hands `serve` the next request the moment a given frame of an answer (by default its done frame) is written, the order a real pipe allows, and an eval request's line (`eval_line`).
- `test/bonito/`: the `wglshow` page test's own environment (Bonito): the page carries its assets, its port has no asset route, and (Linux) serving it opens exactly one listener (CI's "wglshow pages" job). Actual owned-process listener, loopback HTTP/bind and selection-preservation controls, with an extra-listener rejection control, and the live-port refusal proof.

## Start here
`serve` for an op; `stream_eval_frames` for output; `wglshow` for browser artifacts.

## Rules
- The Rust spawn recipe activates the user directory, including a bare workspace, and places this shim behind it on `JULIA_LOAD_PATH`; user package commands do not edit the installed shim project.
- `repl.ready` is the first envelope, and every envelope is written under `OUT_LOCK` (`serve`, `write_envelope`).
- A child says once, in one stderr frame at the start of its first eval, why the daemon ended the child before it
  (`SOT_PREVIOUS_END`, the daemon's sentence: its display ended) and why it has no display of its own
  (`SOT_NO_DISPLAY`, its reason), in that order (`DISPLAY_NOTICE`, `spawn_eval`).
- One eval at a time, and a second gets error then done (`handle_eval`). An eval releases that guard once, just before its done frame and res go out (`spawn_eval`), so a request sent the moment an answer arrives is accepted.
- Only the shim's own locked writer reaches the wire. The child's entry `run_shim` swaps Julia's startup `TextDisplay(stdout)` (bound to the wire) for a real `TextDisplay` on `EvalStdout`, which writes to the current `stdout` (the eval's capture pipe), so `display(x)` in an eval is a stdout frame; packages that look for the stack's `TextDisplay` (Plots, Makie, Bonito) still find it. Then, when `io_out` is the process's stdout, it redirects that stdout to stderr, so a task's display or `println` between evals goes to stderr, which the supervisor drains. All of this runs before the user's startup file (`Base.load_julia_startup`; the spawn passes `--startup-file=no`), so startup.jl runs with stdout on stderr and its prints show as stderr; an error in it is said once, as a stderr frame at the first eval (`STARTUP_FAILURE`), and the shim still serves. Code that calls `serve` directly gets none of this. stderr is not the wire.
- `--startup-file=no` also reaches every Julia process user code starts through `Base.julia_cmd()`: those children do not load the startup file.
- Anything that renders a user value or exception after user code ran goes through `Base.invokelatest` (`sprint(showerror, ...)` included), because `serve`'s world is older than the startup file and every eval.
- Before each eval, inside `stream_eval_frames` after its redirects, when `Main.Revise` is a module (the user's startup file or an earlier eval loaded it), the shim calls `Revise.revise()` (`revise_loaded`): the shim is not a Julia REPL, so Revise never revises on its own. The lookup and the call go through `invokelatest`, because `serve`'s world is older than the `using Revise` that bound it. Revise logs a failed revision instead of throwing, and that log and any throw are stderr frames of that eval; the eval still runs.
- Text frames precede value or error, and done is last (`stream_eval_frames`).
- Program output is bytes, not UTF-8: only a character a read cut waits for its other bytes (`utf8_prefix`), and the
  JSON writer writes a malformed character as U+FFFD (`write_json_string`), so no byte holds back later output or
  fails a frame.
- Every request gets a terminal `res`, except one that a signal stops before its handling (`serve`); `spawn_eval`
  answers each eval once, a failure of its streaming machinery included.
- An interrupt reaches an eval only in its user code (`run_user_code`), which is its code and the rendering of its value
  (`stream_eval_frames`): while that runs it is raised there, one that arrives before it starts stops it before it runs,
  and one that arrives after it has finished is answered `interrupted:false`. A wire line is one write (`write_envelope`). An interrupt that names `eval_ids` reaches only an eval among them (`handle_interrupt`).
- Code that never yields holds the shim's one thread, so `serve` reads no request while it runs; the daemon's interrupt
  signal (one ordinary signal; a burst on a forced request) stops it (`run_shim` sets `Base.exit_on_sigint(false)` after
  the startup file; rust/backend/src/sidecars/repl/CLAUDE.md), and the requests waiting then are answered
  `interrupted:true` for the eval it stopped (`SIGNALED`, cleared by the next eval's code or when no request is
  waiting). A signal lands in whatever Julia code runs, so the shim's code after the eval's code ended drops it and runs
  the step again (`past_interrupts`): `serve`'s wait for the next request, the eval's wait for its readers, a reader's
  drain (`stream_pipe`), the restoring after the code, the error and value frames, and the done and res writes; a frame
  can then be written twice, never lost (a reader keeps a chunk it has read until it is emitted). One that lands in `revise_loaded` holds the interrupt (`:held`), so the code
  never runs and the eval ends as interrupted. One that lands in a request's handling answers that request with an error
  (`repl_exception`), written again if another lands in it, and none ends `serve` (`serve_request`), whose end is the
  REPL's: after a forced eval they can come several close together (rust/backend/src/sidecars/repl/escalation.rs). On
  Julia 1.12, 1 of 20 interrupts of a loop that ends on its own at that moment was answered with
  that error, the REPL alive after each; a signal that arrives while the thread idles in a finished task's end ends the
  REPL on 1.12 (Julia raises it where nothing can catch it), which only a synthetic signal has produced.
- A `BrowserView` is announced once per (url, open) (`announce_browserview`).
- WGLMakie code lives only in ext/.
- `wglshow` serves from one Bonito server per child (`page_server`), on a port the OS assigns, at a secret path minted with that server (`WGL_SERVER` holds both). Its page is a Bonito session of its own with `NoServer` (`no_referrer_page`), so its scripts and files travel inside the page and the port answers only the page and its websocket; `/` and every other path answer 404. It needs Bonito 5.1 or a later 5.x (`wgl_bonito_supported`). `wgl_server` binds once per REPL lifetime: the first call may pin a port, later default/same-port calls reuse it, and a different live pin raises `ArgumentError` without changing the listener, secret, routes or announcement. Restart the REPL to choose another port. A figure has one live viewer (`WGL_VIEWERS`, `replace_viewer!`): a new page load of a figure closes the viewer before it. The listener stays owned until the REPL ends; generic BrowserView servers retain their own multi-port behavior.
- `write_envelope` has a twin in julia/kernel.
