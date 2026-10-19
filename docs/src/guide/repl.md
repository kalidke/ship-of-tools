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
juliaup's default channel, else a `julia` on `PATH`. Both start with
`--startup-file=no`. The REPL then loads your depot's `config/startup.jl`
(`DEPOT_PATH[1]/config/startup.jl`, usually `~/.julia/config/startup.jl`)
itself, with its standard output pointed at its standard error, so what the file
prints shows as error output; a file that throws is reported once, at your
first evaluation, and the REPL still starts. The kernel never loads it, because
it never runs your code. Julia processes your code starts with `Base.julia_cmd()`
also get `--startup-file=no`.

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
restart, and neither is anything in the REPL's own temporary folder, `tempdir()`,
which goes with its process; other content on disk always does.)

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

An evaluation runs on its own task, so whenever it yields (`sleep`, I/O that waits,
`yield()`) the REPL reads the next request, and you can interrupt it mid-eval. An
interrupt schedules a real `InterruptException` onto the running evaluation — the
same semantics as `Ctrl-C` in the stock REPL — which surfaces as an `error` frame. It
reaches only the evaluation's own code and the display of its value (a figure
rendering, for example): an interrupt sent before that starts stops it before it runs,
and one sent after it has finished does nothing. An evaluation that never yields (a
tight numeric loop, a long `ccall`) holds the REPL's one thread, so the REPL cannot
read the interrupt. While an evaluation is running, an interrupt that names it (or
names none) and has gone a second unanswered makes the daemon send the REPL one
interrupt signal, as `Ctrl-C` does to the stock REPL: Julia raises it at the code's
next safepoint, which stops an allocating or calling loop, and holds it through C
code (BLAS), compiling and garbage collection until they return. The REPL keeps
everything it defined. Code with no safepoint at all (`while true end`) ignores that
one signal, and the interrupt is answered with an error that says so. A second
`Ctrl+C` in the drawer while the same evaluations run, or `sot-fe repl interrupt
<ws> --force`, forces it: five signals in a burst, which Julia forces into code with
none (it prints `WARNING: Force throwing a SIGINT` in the evaluation's output).
**Forcing can crash or hang the REPL** when the thread is inside C code (a BLAS call
is a measured crash), compiling or collecting garbage, and the REPL's state is then
lost; `--fresh` restarts it. The daemon sends a signal only for an evaluation it has
seen start and not finish, never to an idle REPL, though it cannot see an evaluation
that has just finished and whose `done` it has not yet read. On Windows the signal is
a Ctrl+Break to the REPL's console process group, which reaches every process in it,
a server an earlier evaluation left running with `run(cmd; wait = false)` included,
and a console program's default Ctrl+Break handler ends it. Code that catches the
`InterruptException` and goes on still holds the REPL (`sot-fe repl interrupt` says
so after 10 s); a restart ends it at once: `r` on a `.jl` file in Files mode, or
`sot-fe repl run <ws> <path> --fresh`, whose run every window's drawer shows. On Julia 1.12, 1 of 20 interrupts of a loop that ends on its own at that very moment was answered with an error (`InterruptException`, code `repl_exception`) while the REPL kept running and kept its state; a signal that arrives as the REPL idles between evaluations can end it on 1.12, which only a synthetic signal has produced. `Ctrl+C` in the drawer names, by number, the
evaluations the drawer shows as running, and stops the running evaluation only if
its number is one of them (an interrupt that names other evaluations sends no
signal); each window numbers its own evaluations, so two windows on one REPL can
share a number.

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
- **Revise.** Ship of Tools does not load Revise; a `using Revise` in your
  startup.jl (or in an evaluation) loads it, and the REPL then calls
  `Revise.revise()` before each later evaluation, so an edit to a tracked
  package's `src/` or an `includet` file takes effect on the next one. A
  revision that fails (a syntax error in the edit) shows as error output of that
  evaluation, which then runs the old code. Without Revise, re-running a script
  picks up edits to that script, but an edit to a package's `src/` takes effect
  only after it is included again.

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
| `browser` | `{url, open, fe}` — a loopback URL for a live browser-served artifact (`wglshow(fig)`, a `BrowserView`); `open` says whether a frontend opens a tab (the window in use, the one used last); `fe`, when present, names the one frontend that opens it |
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

`wglshow` serves the figure over Bonito on a loopback port the OS assigns, at an
address with a secret in it (another account on the box that finds the port gets
nothing), and returns a `BrowserView`, which makes the
frontend open the figure in your OS browser with no URL to copy. The frontend hands the browser a one-use local
address that redirects to the figure, so the secret is never on a command line. On a remote backend the page and
its WebSocket ride the daemon proxy over the control connection (ADR 0035), with no dedicated forward, so
pan/zoom/rotate work whether the backend is local or remote. One server per REPL lives as long as the REPL;
calling `wglshow` again shows the new figure at the same address. With several frontends attached, the figure opens
in one window, the window in use (the one you used last); `wglshow(fig; open = "<fe>")` opens it only on the frontend
`sot-fe --fe <fe>` names, and `open = false` opens it nowhere.

WGLMakie and Bonito are resolved from *your own* project env at call time
(`using WGLMakie` first) — Ship of Tools ships no plotting dependency of its own,
so the REPL stays light until you ask for an interactive figure.

## See also

- [REPL Pane](interface.md#The-REPL-Pane) — the drawer's layout, keys, and how to drive it.
