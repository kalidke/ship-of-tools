# The REPL

*This is the deep dive: the frame schema, dispatch semantics, and figure-rendering
mechanics. For the drawer's layout, keys, and how to drive it day-to-day, see
[REPL Pane](interface.md#The-REPL-Pane).*

Ship of Tools keeps a **persistent Julia REPL** available throughout a session. It is
your interactive Julia session — the same bindings, the same loaded packages —
but its output is structured, and the figures it produces render inline in the
native window rather than as text.

The REPL drawer toggles with `Ctrl+J`.

The drawer is Ship of Tools' own input line, not Julia's REPL: `]` at the
start of an empty line enters pkg mode (`pkg>`, run through
`Pkg.REPLMode`), Backspace on an empty pkg line leaves it, Up/Down recall
earlier inputs, and Ctrl+C interrupts a running evaluation. There is no `?`
help mode, no `;` shell mode and no Tab completion; use the Terminal drawer
or the agent pane for shell commands.

The REPL and the kernel run the `julia` that `SOT_JULIA_BIN` names, else
juliaup's default channel, else a `julia` on `PATH`; neither passes
`--startup-file=no`, so Julia's own default applies and the depot's
`config/startup.jl` (`DEPOT_PATH[1]/config/startup.jl`, usually
`~/.julia/config/startup.jl`) loads — a `using Revise` there works.

## A supervised, separate process

The REPL is a Julia process supervised by the backend daemon, distinct from the
Julia kernel that does project introspection. They are separate on purpose:

- The **kernel** owns dispatch tables, mode trees, indexing, and AST hashing.
- The **REPL** owns your interactive state — variables you defined, packages you
  loaded.

Because they are different processes, **killing the REPL does not kill the
kernel.** You can tear down and restart your interactive session — to clear
state or recover from a wedged computation — without losing the project view, the
mode trees, or the index. (In-memory REPL bindings are not expected to survive a
restart; static content on disk always does.)

## Dispatching code

You drive the REPL two ways, without retyping code:

- **Run a whole `.jl` file** from Files mode. `Shift+R` `include`s the cursored file
  into the current persistent session, so its definitions land in your
  interactive state. `r` first resets the REPL into the file's own project —
  walking up to the nearest `Project.toml` — then runs it there.
- **Type at the prompt** in the REPL drawer. `Enter` submits the input;
  `Shift+Enter` inserts a newline, so you can build a multi-line entry before
  submitting.

Per-line and per-block dispatch — running just the line at the cursor, or the
surrounding top-level form computed by the kernel — are **planned**, not yet
built.

A long-running evaluation does not block the dispatch loop: it runs on its own
task, so you can interrupt it mid-eval. An interrupt schedules a real
`InterruptException` onto the running evaluation — the same semantics as `Ctrl-C`
in the stock REPL — which surfaces as an `error` frame.

## [How agents use the REPL](@id agents-repl)

Claude Code sessions run code in the same REPL you use, the session's own,
through the `sot-fe repl` command that the `julia-repl` skill teaches them.
That REPL starts with `--project` set to the session root's directory when
it has a `Project.toml`, else the shim's environment.
The skill is installed for Claude Code only (see
[What the installer changes](@ref install-footprint)); Codex sessions have no
REPL skill yet. The REPL runs one evaluation at a time; a request that arrives
while another runs comes back `busy` (see
[Hosts, sessions, REPLs](@ref session-model)).

| Command | What it does |
|---------|--------------|
| `sot-fe repl run <ws> <path>` | run a `.jl` file and return its output |
| `sot-fe repl eval <ws> --code '<julia>'` | run a chunk of code and return its output |
| `sot-fe repl status [<ws>]` | report whether the workspace has a live REPL |
| `sot-fe repl interrupt <ws>` | stop a running evaluation, keeping loaded packages |
| `sot-fe repl run <ws> <path> --fresh` | restart the REPL into the file's project, then run it |

`<ws>` is the workspace id, label or slug; a session finds its own id in
`$SOT_WORKSPACE_ID`. The call returns the collected stdout, stderr, value and
errors to the agent. Figures are written to files under
`<workspace>/.sot/runs/<run_id>/` and the agent gets their paths; it can then
put one in your preview pane with `show-result <path>`. Nothing adds
`.sot/` to your project's `.gitignore` — add it yourself.

What this means for you:

- **Every agent run appears in your REPL drawer**, labelled with whatever
  label the caller passed with `--origin`, or "session" when none is passed.
  There is no quiet mode.
- **The REPL is shared.** Your drawer, the session's agent and its subagents
  evaluate into the same `Main`. Definitions an agent makes are there
  when you type next.
- **One evaluation at a time.** A run that arrives while another is going is
  refused as `busy`, not queued.
- **Headless.** The REPL has no display; CairoMakie figures come back as files,
  and interactive figures use WGLMakie with `wglshow`.
- **Revise is not loaded.** Ship of Tools does not load Revise; a `using
  Revise` in your startup.jl still runs. Without it, re-running a script
  picks up edits to that script, but an edit to a package's `src/` takes
  effect only after it is included again.

## Structured output frames

The REPL's display shim emits **structured, typed frames** instead of a single
undifferentiated text stream, so the frontend can render each kind of output
correctly. Frames are newline-delimited JSON; the shape borrows IJulia's
`display_data`, flattened so every field sits at the top level of the frame. The
backend wraps each frame as a `repl.frame` event on the main protocol stream.

| Frame kind | Carries |
|------------|---------|
| `stdout` | `{text}` — streamed incrementally as the evaluation prints |
| `stderr` | `{text}` — streamed incrementally |
| `value` | `{mime, text}` — the last expression's value rendered as text |
| `image` | `{mime, data_base64, bytes}` — e.g. `image/png` |
| `browser` | `{url, open}` — a loopback URL for a live browser-served artifact (`wglshow(fig)`, a `BrowserView`); `open` says whether the frontend should auto-open a tab |
| `error` | `{message, stacktrace: [{file, line, fn}, …]}` |
| `done` | `{eval_id, elapsed_ms}` — always the last frame for an evaluation |

`stdout` and `stderr` stream as the bytes arrive, so you see output as it is
produced rather than only at the end. The `done` frame is the completion signal,
correlated by `eval_id` so the backend can clear the UI spinner for that
evaluation. Errors are structured rather than raw stderr text, which lets the
frontend render stacktraces with `file:line` links. Adding a new MIME (HTML,
LaTeX, custom) is a frontend-side render change — the frame schema is open via
the `mime` field.

## Figures render inline

When the last expression is showable as an image — a CairoMakie `Figure`, a
`Plots.Plot`, or anything that implements `show(io, MIME"image/png"(), x)` — the
REPL emits an `image` frame, and the frontend draws it **inline through the
preview layer**: the Ship of Tools renderer paints it into the window.

It is never reduced to a terminal graphics protocol (sixel, kitty, half-blocks).
Rendering visual output natively is a core premise of the project, and it applies
to REPL figures exactly as it applies to file previews. See
[Frontend Rendering](../design/rendering.md).

## Interactive figures in the browser

Static plots render inline (above); an **interactive** figure — one you pan,
zoom, or rotate — belongs in a real browser. Call `wglshow(fig)` on a WGLMakie
figure:

```julia
using WGLMakie
wglshow(surface(-10:0.4:10, -10:0.4:10, (x, y) -> sin(sqrt(x^2 + y^2));
                axis = (; type = Axis3)))
```

`wglshow` serves the figure over Bonito on a loopback port (`SOT_WGL_PORT`,
default 1241) and returns a `BrowserView` — which makes the frontend open the
figure in your OS browser, no URL to copy. On a remote backend the page and
its WebSocket ride the control tunnel through the daemon proxy (ADR 0035) —
no dedicated forward — so pan/zoom/rotate work whether the backend is local
or remote. The server lives as long as the REPL, and calling `wglshow` again
replaces it.

WGLMakie and Bonito are resolved from *your own* project env at call time
(`using WGLMakie` first) — Ship of Tools ships no plotting dependency of its own,
so the REPL stays light until you ask for an interactive figure.

## See also

- [REPL Pane](interface.md#The-REPL-Pane) — the drawer's layout, keys, and how to drive it.
