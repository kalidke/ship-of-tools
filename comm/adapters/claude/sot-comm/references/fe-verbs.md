# `sot-fe` — the rarer verbs and flags

`sot-fe --help` is the source of truth (it grows with the binary). This is a
shorter tour of the ones worth knowing before you need them.

## `repl run` / `repl eval`

Runs a `.jl` file (or evals code) in a workspace's persistent REPL and
returns the COLLECTED output (stdout/stderr/value/error + figure paths). A
real request/response op (`repl.execute`): blocks until done or `--timeout`
(120s default), non-destructive — runs in the REPL's *current* project, no
reset. `--fresh` on `repl run` instead RESTARTS the REPL into the file's
project first (`repl.run_file`, `fresh:true`) — use it to clear a STALE
kernel (e.g. after a struct/field change Revise can't hot-reload); it's
fire-and-forget, the include's output streams to the FE drawer instead of
being collected.

Using it well (field lessons, 2026-09-17):

- Read the outcome block from stdout in the foreground. Never pipe it
  through `tail`/`head` and never background it; a run that seems stuck for
  exactly `--timeout` means stale comm scripts on that box — reinstall
  (`/sot-install`) before building any workaround.
- Anything longer than one line goes in `--stdin` (or a file inside the
  workspace root for `repl run`, which refuses paths outside it). A
  heavily quoted `--code` string fails at Julia's parser instantly, and a
  fast completion leaves NO daemon log line — only slow ones are logged.
- Edits not taking effect: Revise tracks only packages loaded after
  `using Revise`; for a file loaded before that, `include` it again into
  its module. A binding created by `include` inside an eval is not visible
  to a closure in that same eval (world age): read it in the next eval.
- Right after a daemon restart the workspace may read `unknown` until the
  frontend re-registers; wait for `repl status` to list it.

## `repl interrupt`

Stops the RUNNING eval, keeps the kernel (compiled packages intact) — the
least destructive recovery rung. Try this before `repl run --fresh` (which
re-pays minutes of precompile) and long before any manual `kill`.
Workspace-wide: it interrupts whatever eval is running there, regardless of
who started it. Lands at the eval's next yield point, so a tight
non-yielding compute loop may never take it — then `--fresh` is the honest
escalation. Exit 0 = interrupted; 3 = nothing to interrupt; refuses (exit 3)
when the REPL is `not_started`/`dead` (sending would spawn a kernel) or
`starting` (a boot isn't a wedge).

## `repl status`

The daemon's own view of a workspace's REPL: lifecycle
(`not_started | starting | ready | dead`) + workspace root. Run this BEFORE
any ps/ss archaeology — `dead` needs no cleanup, the next eval respawns it.
An unreachable daemon reports as a TRANSPORT failure, distinct from any REPL
verdict — the two look identical from a failing command but point at
opposite fixes.

## `preview` / `reveal` flags

- `--roi x,y,w,h` — aim the viewport at a source-image-pixel rect (ADR 0022
  vocabulary, round-trippable); the FE clamps to image bounds.
- `--caption <text>` — a figure caption drawn under the image (images only,
  ≤300 chars). **Sticky** to that (workspace, file): survives switches and
  re-previews. Re-previewing the same file WITHOUT `--caption` clears it, so
  pass the new caption when you regenerate with different parameters.

## `goto --boot`

Seeds autostart so the FE boots `ccb` on the switch — the scriptable
spawn→goto→boot primitive. Send this **directed** (`--fe <handle>`); a
broadcast `--boot` would switch and boot every attached FE.
