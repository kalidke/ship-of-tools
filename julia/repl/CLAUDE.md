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
- `src/wgl.jl`: browser-served artifacts, `BrowserView`, `page_server`, private `wgl_server` listener selection and `wglshow`.
- `src/frames.jl`: how an eval's output becomes typed frames, and the BrowserView announcements.
- `test/runtests.jl`: the streaming tests and the stdlib-only guard test.
- `test/bonito/`: the `wglshow` page test's own environment (Bonito): the page carries its assets, its port has no asset route, and (Linux) serving it opens exactly one listener (CI's "wglshow pages" job). Actual owned-process listener, loopback HTTP/bind and selection-preservation controls, with an extra-listener rejection control, and the live-port refusal proof.

## Start here
`serve` for an op; `stream_eval_frames` for output; `wglshow` for browser artifacts.

## Rules
- The Rust spawn recipe activates the user directory, including a bare workspace, and places this shim behind it on `JULIA_LOAD_PATH`; user package commands do not edit the installed shim project.
- `repl.ready` is the first envelope, and every envelope is written under `OUT_LOCK` (`serve`, `write_envelope`).
- One eval at a time, and a second gets error then done (`handle_eval`).
- Text frames precede value or error, and done is last (`stream_eval_frames`).
- Every request gets a terminal `res` (`emit_fallback_done`).
- A `BrowserView` is announced once per (url, open) (`announce_browserview`).
- WGLMakie code lives only in ext/.
- `wglshow` serves from one Bonito server per child (`page_server`), on a port the OS assigns, at a secret path minted with that server (`WGL_SERVER` holds both). Its page is a Bonito session of its own with `NoServer` (`no_referrer_page`), so its scripts and files travel inside the page and the port answers only the page and its websocket; `/` and every other path answer 404. It needs Bonito 5.1 or a later 5.x (`wgl_bonito_supported`). `wgl_server` binds once per REPL lifetime: the first call may pin a port, later default/same-port calls reuse it, and a different live pin raises `ArgumentError` without changing the listener, secret, routes or announcement. Restart the REPL to choose another port. The listener stays owned until the REPL ends; generic BrowserView servers retain their own multi-port behavior.
- `write_envelope` has a twin in julia/kernel.
