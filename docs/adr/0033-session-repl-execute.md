# ADR 0033: session-driven REPL execute + output collect (`repl.execute`)

Status: accepted. Phase 1 (run + collect) implemented + verified end-to-end.
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
(`ok | error | busy | interrupted | timeout | repl_died`), `run_id`,
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
   `res` arrives. Child death drains `pending` → `repl_died`. Nothing hangs.
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
   from a frontend's small per-workspace counter, positive and < 2^53 (safe for
   `jq`/IEEE-754), NOT a mutated high bit. The `run_id` string derives from it.
8. **CLI:** `sot-fe repl run <ws> <path>` / `sot-fe repl eval <ws> (--code|--stdin)`
   — a real request/response call (the interleaved `repl.frame` evts on the
   connection are skipped by op-grep). Reuses `sot-fe`'s endpoint resolution +
   `hello`/token auth. Bounded single-line response ⇒ bash is adequate here
   (unlike the streaming/blob ops). Exit 0 on `ok`, 2 otherwise.

## Consequences

Phase 1 (this ADR) is implemented and verified end-to-end: ok / error+stacktrace
/ repl_died / busy / timeout / truncation(flood) / path-validation / persistent
`Main` state across executes / exit codes. Figure-spill code is in place but not
yet exercised (the test REPL project had no plotting package).

Phase 2 (implemented, this ADR): a backend-emitted `Started` control frame
(a new `ReplFrame` variant carrying `run_id`/`origin`/`display`) rides the
existing `repl.frame` broadcast BEFORE submit; the FE pre-registers a drawer
entry keyed by the canonical `(workspace_id, eval_id)`, stops dropping foreign
`eval_id`s, and renders the run with a distinct `⟨origin ▸ display⟩` prompt.
A synthetic terminal `done` is emitted for timeout/repl_died (the shim's own
won't arrive). `sot-fe repl … --origin <who>` labels the entry. Wire-verified;
seeing it in the live drawer needs an FE rebuild + ADR-0017 relaunch.

Deferred:

- **Backend execution coordinator** (atomic `Idle→Running(run_id,origin)`
  pre-admission, so busy is decided before submit and user evals participate) +
  **run-id-scoped interrupt** in the shim (targeted cancel — the safe basis for a
  session-initiated stop).
- **`--reset-repl`** (explicit, refuses-while-busy, broadcasts a reset boundary)
  if a session needs the file's own project env.
- Connection-level frame filtering if "own-run-only" ever becomes a security
  boundary (today every authenticated connection sees the live frame stream).
