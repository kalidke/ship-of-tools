# julia/repl: the Julia REPL shim, one process per workspace (sidecars)

The shim runs user code. It speaks newline-delimited JSON over stdin/stdout and is stacked behind the workspace's own
project by `JULIA_LOAD_PATH`, so its `[deps]` are stdlib only, forever: a registered dependency would be shadowed by
the version a user's manifest pins. That is why `json.jl` exists, and a guard test in `test/runtests.jl` enforces it.
Part of the sidecars; charter: rust/backend/src/sidecars/CLAUDE.md (not yet written at this commit).

## Files
- `Project.toml`: the package, stdlib dependencies only.
- `ext/ShipToolsReplWGLMakieExt.jl`: the WGLMakie package extension, the only place WGLMakie code lives.
- `src/ShipToolsRepl.jl`: the module, `serve` and its ops (eval, run_file, interrupt), `write_envelope`.
- `src/json.jl`: the stdlib-only JSON codec.
- `src/wgl.jl`: browser-served artifacts: `BrowserView`, the WGLMakie server, `wglshow`.
- `src/frames.jl`: how an eval's output becomes typed frames, and the BrowserView announcements.
- `test/runtests.jl`: the streaming tests and the stdlib-only guard test.

## Start here
`serve` for an op; `stream_eval_frames` for output; `wglshow` for browser artifacts.

## Rules
- `repl.ready` is the first envelope, and every envelope is written under `OUT_LOCK` (`serve`, `write_envelope`).
- One eval at a time, and a second gets error then done (`handle_eval`).
- Text frames precede value or error, and done is last (`stream_eval_frames`).
- Every request gets a terminal `res` (`emit_fallback_done`).
- A `BrowserView` is announced once per (url, open) (`announce_browserview`).
- WGLMakie code lives only in ext/.
- `wgl_pick_port` has a twin in julia/pluto/start.jl, and `write_envelope` has one in julia/kernel.
