# ADR 0019: Frontend control channel + state readback

**Status:** Accepted
**Date:** 2026-05-29

## Context

The dev `claude` runs inside the frontend's Terminal drawer (ADR 0016/0017) and
drives DevEnv development from there. ADR 0017 already established that an
in-terminal agent **cannot synthesize keystrokes** into the winit window but
**can write a file** — that's how the relaunch sentinel works. The user wants to
generalize this: let the in-terminal agent *drive the FE* (switch workspaces on a
timer, reload keybindings after editing them, push help/notifications, drive
mode/nav) and *observe* it (know the active workspace, available sessions, mode).

The frontend↔backend protocol (ADR 0010) is the wrong layer for this: it carries
project state, not FE UI intent, and the backend is remote. UI navigation
(`active_workspace_id`, BL-pane retarget, mode, focus, keybindings) is owned by
the frontend. So the control surface must be local to the FE.

## Decision

### 1. Command channel = a watched directory of JSON command files

Commands are dropped as individual JSON files under
`<devenv-state-dir>/fe-commands/` (`%LOCALAPPDATA%\devenv\fe-commands\` on
Windows; `$XDG_STATE_HOME/devenv/fe-commands/` else — the same per-machine state
dir as the relaunch sentinel). One JSON object per file. A persistent watcher
thread (sibling to ADR 0017's one-shot relaunch watcher, but it loops instead of
breaking) polls every 400 ms, reads each `*.json` file, **deletes it first**
(so a malformed file can't loop forever), parses it into an `FeCommand`, pushes
onto a shared `Mutex<VecDeque<FeCommand>>`, and wakes the window via
`request_redraw()`. `window_event` drains the queue **on the main thread** and
dispatches each command through the *same methods the keybinds use*
(`switch_to_workspace`, `cycle_workspace`, the keybindings loader, mode/nav
`Action`s) — so commands inherit the same routing guarantees as keys (incl. the
ADR-0014 per-workspace tree-reply guard).

Rationale for files over a socket: the in-terminal agent drops files with its
plain Write tool (no helper binary, no pipe client), it's cross-platform, and it
matches the ADR-0017 precedent exactly. 400 ms latency is irrelevant at this
cadence (per-minute switching, occasional reloads). Writers SHOULD write to
`<name>.json.tmp` then rename to `<name>.json` to avoid a partial-read race; the
watcher only consumes `*.json` and tolerates a parse failure by logging+dropping.

Command schema (variants land incrementally; commit 1 ships the first two):

```jsonc
{"cmd":"workspace","slug":"MyAnalysis"}   // slug null/"default" → daemon default
{"cmd":"cycle_ws","dir":1}                   // +1 next, -1 prev; wraps
{"cmd":"reload_keybindings"}                 // (later commit)
{"cmd":"notify","text":"…","level":"info"}   // (later commit)
{"cmd":"mode","mode":"files"}                // (later commit)
{"cmd":"nav","action":"down"}                // (later commit)
```

### 2. State readback = `fe-state.json`

The FE writes `<devenv-state-dir>/fe-state.json` (atomic temp+rename) whenever
the relevant state changes, so the in-terminal agent can observe rather than
screenshot:

```jsonc
{"rev":N,"active_workspace":"slug-or-null","workspaces":["…"],"mode":"files","focus":"nav","host":"myhost"}
```

Read-only from the agent's side; advisory, debounced. (Lands in a dedicated
commit so it can be verified independently.)

## Consequences

- **Positive:** the in-terminal agent gets a real, scriptable control+observe
  loop over the local FE with zero new transport — reuses the watcher pattern,
  the state dir, and existing dispatch methods. "Switch every minute" = a `/loop`
  that drops a `cycle_ws` file each tick.
- **Positive:** the same channel is open to a human or any other in-terminal
  program (the file convention is the API).
- **Negative / accepted:** 400 ms poll latency and the partial-write race (handled
  by the temp+rename convention + tolerant parse). No auth on the channel — it's a
  local, per-machine, user-owned dir, same trust model as the relaunch sentinel.
- **Negative / accepted:** `mode`/`nav` commands require extracting today's inline
  key-handler logic into callable methods so keybinds and commands share one path
  (deferred to its own commit).

## Alternatives considered

- **Local named-pipe / unix-socket control endpoint:** lower latency and naturally
  bidirectional, but needs new FE listener plumbing and a client the agent can
  drive (it can't open a pipe from the Write tool). Rejected for v1 as not worth
  the plumbing at this cadence; the file channel can be upgraded later behind the
  same `FeCommand` enum.
- **Routing FE control through the backend protocol:** layering violation (backend
  is project state, and is remote), and it can't drive frontend-owned UI state.
- **OS keystroke synthesis (SendInput):** already rejected in ADR 0017 — fragile,
  focus-dependent, and an in-terminal agent can't reliably do it.
