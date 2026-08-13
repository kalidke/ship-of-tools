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
