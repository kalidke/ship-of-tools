# The REPL

*This is the deep dive: the frame schema, dispatch semantics, and figure-rendering
mechanics. For the drawer's layout, keys, and how to drive it day-to-day, see
[REPL Pane](panes/repl.md).*

Ship of Tools keeps a **persistent Julia REPL** available throughout a session. It is
your interactive Julia session — the same bindings, the same loaded packages —
but its output is structured, and the figures it produces render inline in the
native window rather than as text.

The REPL drawer toggles with `Ctrl+J`.

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

- **Run a whole `.jl` file** from Files mode. `R` `include`s the cursored file
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

## Structured output frames

The REPL's display shim emits **structured, typed frames** instead of a single
undifferentiated text stream, so the frontend can render each kind of output
correctly. Frames are length-prefixed JSON; the shape borrows IJulia's
`display_data`, flattened so every field sits at the top level of the frame. The
backend wraps each frame as a `repl.frame` event on the main protocol stream.

| Frame kind | Carries |
|------------|---------|
| `stdout` | `{text}` — streamed incrementally as the evaluation prints |
| `stderr` | `{text}` — streamed incrementally |
| `value` | `{mime, text}` — the last expression's value rendered as text |
| `image` | `{mime, data_base64, bytes}` — e.g. `image/png` |
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
default 1241, auto-forwarded by the launcher alongside the Pluto/video/docs
ports) and returns a `BrowserView` — which makes the frontend open the figure in
your OS browser, no URL to copy. The WebSocket that carries interaction events
rides the *same* forwarded port, so pan/zoom/rotate work whether the backend is
local or remote. The server lives as long as the REPL, and calling `wglshow`
again replaces it.

WGLMakie and Bonito are resolved from *your own* project env at call time
(`using WGLMakie` first) — Ship of Tools ships no plotting dependency of its own,
so the REPL stays light until you ask for an interactive figure.

## See also

- [REPL Pane](panes/repl.md) — the drawer's layout, keys, and how to drive it.
- [The Orchestrator](orchestrator.md) — the LLM can dispatch code to this same REPL via a tool.
