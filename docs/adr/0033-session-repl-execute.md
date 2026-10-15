# ADR 0033: session-driven REPL execute + output collect (`repl.execute`)

**Status:** current — accepted. Phase 1 (run + collect) implemented + verified end-to-end.
Phase 2 (session runs visible in the user's drawer) implemented + wire-verified;
live-window rendering pending an FE rebuild + ADR-0017 relaunch.

## Context

A Claude session (the orchestrator, or a peer session over sot-comm) can already
navigate to a `.jl` file and drive the frontend (ADR 0025 `fe.command`). It could
NOT run that file in a workspace's persistent Julia REPL and read the output
back. We want: **run a `.jl` file (or a code chunk) in a workspace's REPL and
grab the collected output** — and, later, have that run show up in the user's
live REPL drawer (shared history).

The existing REPL surface (ADR 0009) is a poor fit for "grab the output":

- `repl.eval` / `repl.run_file` are **immediate-ack + stream**: the response
  carries an empty `frames` vec, and output arrives as separate `repl.frame`
  evts over a per-backend `broadcast::channel(256)`.
- That broadcast bus is **lossy and global**: a subscriber that falls behind gets
  `RecvError::Lagged(n)` and loses `n` frames irrecoverably, and every workspace's
  frames share the one bus. A tight `println` loop outruns a collector — so
  collecting a run's output *off the broadcast bus* yields a successful-looking
  but silently **incomplete** result. (This was the single worst risk in the
  first design pass; caught in external review.)
- The **frontend deliberately drops** `repl.frame` evts for any `eval_id` it did
  not itself originate — an external session can't "listen in".
- The shim does **not** emit a `done` frame on every path — a missing run-file
  path returns only a terminal `res`, no frames — so a collector keyed on `done`
  hangs.
- Envelopes are capped at **1 MiB** (`codec.rs`); reviving `frames` inline would
  blow the cap on one stdout flood or one base64 figure.
- The shim has a **single-eval guard** (stdout/stderr redirect is process-global),
  so a session run and a user run cannot coexist.

## Decision

A dedicated **request/response op `repl.execute`** — authoritative, not a flag on
the streaming ops (overloading one verb with two lifecycle contracts risks a
future FE freezing its own connection).

Request (`ReplExecuteReq`): `workspace_id` (required), `input`
(`run_file{path}` | `eval{code,mode}`), optional `timeout_ms`
(default 120 s, clamped [1 s, 30 min]).

Response (`ReplExecuteRes`): an explicit `outcome`
(`ok | error | busy | interrupted | timeout | restarted | repl_died`), `run_id`,
`workspace_id`, `elapsed_ms`, **bounded** `stdout`/`stderr`, `values[]`,
`error{message,stacktrace}`, `figures[]` (file paths), `truncated`,
`project_dir`/`project_source`.

Mechanics:

1. **Loss-free collection, not the broadcast bus.** The supervisor tees every
   frame for the run's `eval_id` into a dedicated per-run `ExecAccum` collector
   (bounded by `EXEC_TEXT_CAP` = 256 KiB of stdout/stderr; value/image/error/done
   always kept) *in addition to* the best-effort broadcast. Frames can never
   `Lagged`-drop from the authoritative result.
2. **Completion keys off the shim's terminal `res`, not `done`.** The execute
   submission carries both a reply oneshot (captures the `res`, which the shim
   emits on *every* path incl. missing-file) and the collector. Frame ordering
   guarantees all frames precede the `res`, so the collector is complete when the
   `res` arrives. The child's end drains `pending`: `restarted` when a restart
   retired it, else `repl_died`, and `error` says which. Nothing hangs.
3. **Bounded response.** stdout/stderr capped in the collector; value/error text
   capped per-field; **images spill to `<ws>/.sot/runs/<run_id>/fig-N.<ext>`**
   (base64 is never inlined) and only paths are returned. The response stays well
   under 1 MiB.
4. **Timeout without interrupt.** On timeout the handler returns `outcome:timeout`
   with partial output and does **NOT** send `repl.interrupt` — a generic
   interrupt could race and kill a *subsequent* user eval (the shim's interrupt
   is not run-scoped). The run keeps going; its frames still reach the drawer.
5. **Concurrency via the shim guard.** A run submitted while another eval is in
   flight is rejected by the shim's single-eval guard; the handler maps that to
   `outcome:busy`. No backend pre-admission coordinator in phase 1.
6. **Non-destructive.** Runs `include`/eval in the REPL's *current* project — no
   `restart_with_project` reset (that destroys the user's `Main` and, per ADR
   0017's drawer-clear coupling, would make displayed history lie). A
   project-mismatch is surfaced faithfully in `stderr`/`error`.
7. **Backend-issued `eval_id`** from an atomic counter based at 2^40 — distinct
   from a window's own small count, positive and < 2^53 (safe for
   `jq`/IEEE-754), NOT a mutated high bit. The `run_id` string derives from it.
   Since 0.6.6 the same counter numbers every run (amendment 2026-10-14).
8. **CLI:** `sot-fe repl run <ws> <path>` / `sot-fe repl eval <ws> (--code|--stdin)`
   — a real request/response call (the interleaved `repl.frame` evts on the
   connection are skipped by op-grep). Reuses `sot-fe`'s endpoint resolution +
   `hello`/token auth. Bounded single-line response ⇒ bash is adequate here
   (unlike the streaming/blob ops). Exit 0 on `ok`, 2 otherwise.
9. **Recovery + introspection CLI** (added 2026-08-12): `sot-fe repl interrupt
   <ws>` and `sot-fe repl status [<ws>]`. Both are thin surfaces over capability
   the daemon already had — `repl.interrupt`, and `repl_state` as returned by
   `workspace.list` — and neither needed a new op. They exist because without
   them the documented recovery for a wedged run was "hand-write JSON frames
   over the socket", and answering "is this REPL alive?" meant `ps`/`ss`
   archaeology over facts the daemon already knew. `interrupt` is the
   least-destructive rung of the ladder: it keeps the kernel's compiled
   packages, where `--fresh` re-pays the precompile bill and a manual kill
   bypasses the supervisor. Behaviours worth keeping (several were review
   findings against the first cut of this item — the notes below correct it):
   - **All four repl verbs** resolve the workspace CLIENT-side (id, **label**,
     or slug) via `workspace.list`; the daemon's own `resolve()` takes id/slug
     only, so a `<Label>` would otherwise come back `unknown_workspace`.
     `run`/`eval` do it via a passthrough that skips the lookup entirely for
     canonical `ws-*` ids and caps the lookup's socket budget at
     min(`--timeout`, 10)s — item 8's single-round-trip description holds for
     `ws-*` callers; label/slug callers buy one bounded pre-flight.
   - `interrupt` REFUSES (exit 3) on `repl_state ∈ {not_started, dead}`. Every
     submission path calls `ensure_supervisor()`, which (re)spawns whenever no
     child is live — so an interrupt aimed at either state would *spawn* a
     kernel (and pay its precompile) to interrupt nothing. It also refuses on
     `starting`: a booting child reads stdin only once its serve loop is up,
     so the frame would sit unread past any budget (reading as a transport
     failure for a healthy state) and then kill the first queued eval.
   - `interrupt` surfaces the shim's `interrupted` flag rather than treating
     "request delivered" as success: `{interrupted:false, note:"no eval in
     progress"}` is a distinct outcome (exit 3) from a real cancellation
     (exit 0) — an escalation ladder keys on exactly that bit. The interrupt
     is **workspace-wide** (the shim kills its single `CURRENT_EVAL`; there is
     no per-run scoping), and it lands at the eval's next **yield point** —
     a non-yielding compute-bound eval may never take it, in which case
     `--fresh` is the honest escalation. `status` cannot confirm delivery:
     the wire has no busy/idle bit.
   - `status` does NOT print `kernel_running`: that field is the lazily-built
     introspection Kernel handle (`kernel_built()`), unrelated to the REPL
     child and stale by design. It labels the path `root:` (the workspace
     root) because the child's *active* project can differ (`--fresh` restarts
     into the file's project; a no-`Project.toml` workspace runs in the shim
     env) and the wire carries neither. A row with no `repl_state` key (a
     pre-rollout daemon; the field is `#[serde(default)]`) reports
     `unknown` rather than collapsing to `not_started`.
   Both report an unreachable daemon as a distinct TRANSPORT failure rather than
   as a REPL verdict — the two are indistinguishable from a failing CLI and
   point at opposite fixes (a real session lost an afternoon to a stale `tcp:`
   endpoint that fails exactly like a dead kernel). Same guard for a daemon
   that ANSWERS but refuses (token/auth): its `{error, code}` res is surfaced
   verbatim instead of crashing jq on a missing `.workspaces`.

## Consequences

Phase 1 (this ADR) is implemented and verified end-to-end: ok / error+stacktrace
/ repl_died / busy / timeout / truncation(flood) / path-validation / persistent
`Main` state across executes / exit codes. Figure-spill code is in place but not
yet exercised (the test REPL project had no plotting package).

Phase 2 uses a backend `Started` frame before submit to register a session-originated run in the drawer. The frontend keys it by the dial host and eval_id, deriving its workspace from the frame's hint, and renders origin and display as its prompt. The backend closes every run it announced before returning a repl.execute report: submit failure and a collected result lacking Done receive a synthetic Done, including timeout, restarted, repl_died and terminal res-only errors, and every one but a timeout gets an error frame with the report's error before it, so the entry says why it ended. A collected Done suppresses handler synthesis. Validation before Started creates no run. A timeout closes the report's drawer entry while the underlying eval may still run; this does not deduplicate later child frames. `sot-fe repl … --origin <who>` labels the entry. Every `repl.eval` and `repl.run_file` run is numbered and announced the same way (amendment 2026-10-14); a `--fresh` run's `Started` comes after the restart and before the run, and the shim's `done`, the close-out of a child that ends first, or a synthetic `Done` on submit failure closes it.

Deferred:

- **Backend execution coordinator** (atomic `Idle→Running(run_id,origin)`
  pre-admission, so busy is decided before submit and user evals participate) and a
  **session-initiated stop**, which would use the shim's eval-scoped interrupt
  (amendment below).
- **`--reset-repl`** (explicit, refuses-while-busy, broadcasts a reset boundary)
  if a session needs the file's own project env.
- Connection-level frame filtering if "own-run-only" ever becomes a security
  boundary (today every authenticated connection sees the live frame stream).

## Amendment (2026-10-09, 0.6.6): the eval-scoped interrupt

`repl.interrupt` takes an optional `eval_ids`. The shim interrupts the eval that holds its guard only if that eval's id
is among them, so the window's Ctrl+C, which names every eval its drawer shows in flight, does not stop an eval
accepted after the one the user meant. The scope is by number: a window names its own evals by its own numbers, which
the daemon maps to its numbers for that window's runs, and other runs by the daemon's numbers (amendment 2026-10-14).
An interrupt without `eval_ids` still stops whichever eval runs, and `sot-fe repl
interrupt` sends none: item 4's "not run-scoped" and item 9's "workspace-wide" describe that form. An interrupt reaches
an eval only in its user code, which is its code and the display of its value. One that arrives before it starts stops
it before it runs, and the eval is still answered with an error frame, done and res, so item 2's "every path" holds; one
that arrives after it has finished is
answered `interrupted:false`. Item 4 stands: a timeout sends no interrupt.

## Amendment (2026-10-14, 0.6.6): one numberer

The daemon numbers every run on every REPL (`repl.eval`, `repl.run_file`, `repl.execute`) from item 7's counter
(`rust/backend/src/sidecars/repl/numbers.rs`) and announces each with `Started` before its submission, so a number names
one run in every window. A window still numbers its own evals and `r`/`R` runs, with one count per window, and its
drawer entry for a run carries that number. The daemon keeps it as an alias of that window (`CallerNumbers`). A window
is its hello's declared host and `instance` (ADR 0046 decision 1), so each connection of it, a redial's included, shares
the aliases; a connection whose hello names no instance is a window of its own. Each connection of the window is written
the run's frames under the window's number and is never sent the run's `Started`, a run the daemon refuses it (a full
queue, a REPL that cannot start) is written to it not at all, since the reply says why, and the `eval_ids` of an
interrupt from it are mapped to the daemon's numbers. A window's number names one run: a later run it numbers the same
takes the number (the Windows launcher keeps a window's `instance` across a relaunch, whose count starts at 1 again),
and the earlier run's later frames reach it as any other run's. The client roster keeps a window's aliases while it has
a connection and after its last one leaves, so a window that redials, even after the daemon reaped its silent link, is
written the rest of its runs; it keeps at most 256 runs per window (a finished run goes first) and 64 windows with no
connection (the one away longest goes first). Every other connection is written the run under the daemon's number, after
its `Started`, so a second window shows the run as an entry of its own. Before this, every window's first eval was 1,
and a second window appended the first window's runs to its own entry with the same number. No payload changed: a window
that predates this is written its own runs under its own numbers and the other windows' runs announced, except that it
counts each row from 1, so of its two rows' evals in flight on one host the later takes the number and the earlier's
later output comes under the daemon's number, which that window drops; a window against a daemon that predates this sees
what it saw before.

## Amendment (2026-10-15, 0.6.6): a run cut by a link drop

A window's link can drop while one of its runs is in flight, and the window can redial the same daemon while the run
goes on: the daemon keeps the window's numbers across its connections (amendment 2026-10-14). The REPL's supervisor
marks each of a window's runs running when it writes the run to the child, and ended before it publishes the run's
`done`, the close-out's synthetic `done` included (`CallerNumbers::ran`, `ended`). Every hello reply names the window's
runs still running, by the window's numbers (`HelloRes::repl_running`), read after the connection subscribed to the
frame bus, so the `done` of each run it names is written to that connection. It names no run still queued or refused,
no run whose request never reached the daemon, no other window's or session's run, and nothing after a daemon restart:
the REPL children end with the daemon, and its records are in memory. The field is additive: a daemon that predates it
names none, and a window that predates it ignores it.
