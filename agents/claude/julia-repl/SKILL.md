---
name: julia-repl
description: Run Julia in the workspace's persistent REPL (`sot-fe repl run|eval|interrupt|status`) instead of spawning `julia` — the packages stay loaded between runs and the call returns real collected output (stdout, stderr, values, errors, figure paths). Use it for anything that would re-pay a heavy package load; read the contamination rule before the first call.
---

# julia-repl — the workspace already has a warm Julia

**A workspace has a persistent Julia REPL a session can drive, it returns
real output, and it is the alternative to spawning `julia` for anything that
reloads a heavy package.** A fresh `julia -e` pays the whole precompile +
load bill every time (40 s for a Makie-flavoured package; minutes for a
headless test file). The REPL pays it once per workspace.

```bash
sot-fe repl eval "$SOT_WORKSPACE_ID" --code 'using Pkg; Pkg.status()'
```

`$SOT_WORKSPACE_ID` is already in your environment and is the canonical id —
no lookup. `sot-fe repl status` with no argument lists every workspace whose
REPL has been started.

## THE RULE — `Main` is shared and forever

The kernel is the **workspace's**, not yours: the owner's REPL drawer, every
later session and all your subagents eval into the same `Main`. Julia cannot
undefine a module, and a `const` rebind is a one-way door.

- **REPL** for code that reads the loaded world: run the real package's
  tests, call its functions, reproduce a failure, inspect state, iterate on a
  computation.
- **Fresh `julia` process** for anything that *defines a stand-in for real
  code*: stub modules in `Main`, `include_string` with imports redirected,
  method piracy, `const` rebinds, a test-tier `ENV` later runs must not see.
  There, a fresh process is the **correct choice, not a failure to use the
  REPL** — those stubs would shadow the real package for the owner and for
  every run after yours, permanently.

The test: *would the owner's next eval be wrong because of mine?* Yes → fresh
process.

## THE SECOND RULE — one kernel, one eval, no queue

There is **one kernel per workspace** and it runs **one eval at a time**. An
overlapping run is not queued and not retried — it comes straight back as
`outcome: busy` and nothing ran. The owner's REPL drawer is the same kernel,
and so is every subagent you launch.

So divide the labour; do not build a retry loop:

- **REPL** — the sequential, stateful, load-heavy work of ONE agent:
  reproduce a failure, iterate on a function, rerun a suite against
  already-compiled packages, inspect state between runs. This is where the
  40-second package load stops being paid per run.
- **Fresh `julia` processes** — parallel fan-out, always. Twelve subagents
  running tests want isolation anyway (see THE RULE), and pointing them at
  one REPL turns a working parallel run into a stream of `busy` rejections.

If a fan-out must share compiled packages, serialize it through one caller
that owns the REPL, and let the others stay on fresh processes.

## HOW TO CALL IT — a line or two, or a file

Every call is one of two shapes, never anything in between:

- **A line or two with `eval --code`.** Read a value, check state, call one
  function: `names(M)`, `stage_position(RIG)`, `length(frames)`.
- **Anything longer goes in a `.jl` file under the workspace root, run with
  `repl run`.** That means a loop, a sequence of hardware steps, a
  multi-line function, or anything you expect to edit and run again.
  Keep these in `dev/output/repl/<purpose>.jl`, one file per purpose, and
  edit it in place between runs.

Why a file wins past two lines:
- **Errors carry the file's line numbers.** An error comes back as
  `in expression starting at …/zstack.jl:14`, not a position inside a
  quoted shell string.
- **No shell quoting.** `$`, quotes and backslashes in Julia code stop
  fighting the shell.
- **The owner can read exactly what ran.** The file is a record of the
  step, and re-running it after an edit is the iteration loop.
- **It is re-runnable.** `repl run` re-`include`s the file, so edited
  definitions take effect without Revise.

`repl run` keeps the kernel's project. If the file sits under a different
`Project.toml`, it runs anyway and prints a note; it never switches. A file
that errors partway leaves everything it defined before the error in `Main`.
Fix the file and run it again; don't patch around the half-state with
evals.

## Commands

| Want | Command |
|---|---|
| Run a `.jl` file, get its output | `sot-fe repl run <ws> <path> [--timeout <s>]` |
| Run a line or two | `sot-fe repl eval <ws> --code '<julia>'` |
| Is there a live kernel? | `sot-fe repl status [<ws>]` |
| Stop a runaway eval, keep the packages | `sot-fe repl interrupt <ws>` |
| Reset the kernel into a file's project | `sot-fe repl run <ws> <path> --fresh` |

`<ws>` = workspace id, label or slug. Exit 0 on `outcome: ok`, 2 otherwise.
`--fresh` **destroys `Main`** (it restarts the kernel into the file's own
project) and is fire-and-forget — output goes to the owner's drawer, not to
you. Use it when you mean to reset, e.g. after a struct change.

## What you need to know before the first call

**Project.** The kernel runs `--project=<workspace root>`, with the default
load-path entries still on it. A run cannot name a different project;
`--fresh` is the only retarget and it restarts. `Pkg.activate` inside an eval
is process-global — it changes the project for the owner and every later run
until a restart, so do package work (`Pkg.add`, `Pkg.instantiate`) in the
workspace's own project and never leave another one activated.

**ENV.** Same process, so `ENV["X"] = …` persists into the owner's next eval.
Use `withenv(...) do … end` so a test tier cannot leak.

**Detecting busy.** Read the `outcome:` token on the first line of the
output — the exit code does not tell you: 0 only for `ok`, 2 for `busy`,
`error`, `timeout` and `repl_died` alike. `outcome: busy` means **nothing
ran**; your code is fine and someone else's run is in flight. `repl status`
has no busy bit, so a rejected run is the only way to learn it.

**Timeouts.** `--timeout <s>`, default 120 s, clamped to [1 s, 30 min]. A
timeout does **not** cancel: you get `outcome: timeout` with partial output
while the run continues — and the kernel stays busy for the owner and every
other agent until it finishes. Same for a call the tool layer kills on your
side. An overlong eval is therefore not just your problem; keep runs short
enough that the owner is not locked out, and say so if one wedges. Never re-run blindly after either — check the
`run_id` or the drawer first, or you execute the side effects twice. Budget
generously for the first call per workspace (precompile).

**Interrupt.** Lands at the eval's next yield point. A tight numeric loop or
a long `ccall`/BLAS may never take it; if repeated interrupts change nothing,
`--fresh` is the honest escalation. Never kill the kernel by pid.

**Output.** Collected and returned at the end — nothing streams. stdout+stderr
are capped at 256 KiB combined and the cap keeps the **head**, so a long test
log loses its tail, which is exactly where the failure summary lives
(`[output truncated]` marks it). Values and errors cap at 64 KiB each.
Figures spill to files under `<ws>/.sot/runs/<run_id>/` and you get the
paths — pass them to `show-result`. For a big log, redirect to a file inside
the eval and read the file.

**Headless.** The kernel inherits the daemon's environment; nothing sets
`DISPLAY` and nothing runs xvfb. CairoMakie works and its figures come back
as figure paths. For interactive figures use WGLMakie and `wglshow`, which
serves over loopback. `using GLMakie` on a headless backend is a GLFW init
error.

**Revise is not loaded.** Editing a source file does not change the next run:
`include` it again, or `using Revise` yourself first (it must resolve from
the workspace project or the default environment).

**Lifetime.** The kernel outlives your session, a `/compact` and every
subagent; it dies with the daemon. What you leave in `Main` is what the next
session finds.

**Visibility.** Every session run registers an entry in the owner's REPL
drawer, labelled `⟨origin ▸ display⟩` — `--origin <who>` sets the label.
There is no quiet mode.

## Don't

- Pass more than a line or two through `--code`: write the file and
  `repl run` it.
- Define stubs, redirect imports or rebind consts in it — fresh process.
- Drive it from parallel subagents: the second one gets `busy`.
- Use it for throwaway parse checks — every run lands in the owner's history.
  `julia -e` is right for those.
- Re-run after a timeout or a missing response — the first run is still going.
- Kill a wedged kernel by pid: `interrupt`, then `--fresh`.
- Reach for `--fresh` to "make it clean": it re-pays the whole precompile
  bill, which is the cost this skill exists to avoid.
