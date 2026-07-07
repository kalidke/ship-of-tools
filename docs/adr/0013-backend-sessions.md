# ADR 0013: Backend sessions — tmux registry, lifecycle, resume

**Status:** Accepted
**Date:** 2026-05-15

## Context

Phase 1 ran one backend per DevEnv instance, rooted at one project. Phase 2 needs multiple concurrent backends — one per project the user is working on — with a frontend that can list them, switch between them, spawn new ones, and resume the one it was last attached to after a restart. Requirements doc §"Window and layout management" and §"Multi-agent operation" both depend on this surface; multi-agent (later) layers on top.

The naive answer is a new "orchestrator" daemon process that owns the registry, spawns backend children, and routes ops. But the user's workflow already runs everything inside tmux (per ADR 0010 the backend itself lives in a `devenv-backend-<id>` session), tmux already solves: listing sessions, supervising children, multi-attach, surviving disconnect, cross-machine forwarding. Writing a new daemon to do what tmux already does is duplication. The cheaper, more honest answer is to make tmux the registry.

## Decision

**Tmux is the orchestrator.** No new long-running daemon. The frontend talks to tmux for sessions ops (list / create / kill) via `tmux -F` queries and `tmux send-keys`; it talks to each backend daemon directly via that daemon's Unix socket as today.

### Session shape

**One tmux session per backend, plus one default home-base session.**

```
tmux server (on the backend host)
├── sot-home                      # always present; "you are not inside a project"
├── sot-be-mypackage              # one project = one session
│   ├── pane daemon: devenv-backend --project /path --socket /run/devenv/<id>.sock
│   ├── pane llm:    claude orchestrator agent (optional)
│   └── pane shell:  optional ad-hoc shell at project root
└── sot-be-experiment2
    └── …
```

Naming: `sot-be-<slug>` where `<slug>` is the project label (`Project.toml`'s `name`, falling back to dir basename, with `-N` disambiguator on collision). Home-base is `sot-home`.

The daemon process runs *inside* a pane (not as a tmux-naive detached process). The pane is where its stderr / logs surface and where the user can `tmux attach` for debugging. The Unix socket the daemon listens on is independent of tmux — frontend connects to that directly for the structured protocol.

### Persistent metadata

Tmux is runtime-only — sessions die on tmux server restart. Durable state lives on disk:

```
~/.config/devenv/
  state-<hostname>.toml          # global, frontend-local: where we left off
  sessions/<session_id>.toml     # per-backend: how to bring this backend back
```

`state-<hostname>.toml`:

```toml
last_active_session = "<uuid>"
last_mode           = "files"

[window]
geometry = { w = 1600, h = 1000, x = 100, y = 50 }
```

The `-<hostname>` suffix is load-bearing on Linux because multiple hosts in a shared-$HOME cohort would otherwise stomp each other.

`sessions/<id>.toml`:

```toml
session_id    = "<uuid>"
label         = "MyPackage.jl"
project_dir   = "/home/user/julia_dev/MyPackage.jl"
tmux_session  = "sot-be-mypackage"
socket_path   = "/run/user/1000/devenv/<uuid>.sock"
created       = 2026-05-15T22:00Z

[nav_state]
mode          = "files"
cursor_path   = "src/lib.jl"
scroll_lines  = [0, 12, 0, 0]   # per pane

[layout]
left_col_pct  = 50
top_row_pct   = 50

[bl_pane]
attached_to   = "sot-be-mypackage:llm.1"
```

### Sessions mode (the picker)

A new Mode (sibling of Files / Modules), reached by the existing mode-switch hotkey:

- **col 1** — sessions list: `[sot-home] [+ create new] [sot-be-mypackage] [sot-be-experiment2] …` with status sigil (running / unreachable / stopped)
- **col 2** — panes within the cursored session
- **col 3** — pane meta: cmd, pid, last-active, size
- **preview** — live tail via `tmux capture-pane -p -S -200 -t <pane>` rendered as text (refreshes on cursor settle)

Actions:
- **enter** on a session row — attach: swap active backend to this one + BL pane re-targets to the session's default pane
- **enter** on a pane row — BL pane re-targets to that specific pane (active backend unchanged)
- **`+ create new`** — open the **Option C picker**: nav rooted at `$SOT_PROJECTS_ROOT` (default `$HOME/julia_dev`), reusing Files-mode rendering with a different root. Cursor onto a folder, key to confirm. We then `tmux new-session -d -s sot-be-<slug> 'devenv-backend --project <dir> --socket <path>'`, write the toml, attach.
- **`x`** (or similar) on a session — `tmux kill-session`, mark toml stopped (keep for adopt-back)
- **`d`** — delete toml + kill session

### Startup — resume, don't land

DevEnv launched without flags is "resume." Sequence:

1. Read `state-<hostname>.toml` → `last_active_session`.
2. Read `sessions/<id>.toml` → `project_dir, tmux_session, socket_path, nav_state, layout`.
3. `tmux has-session -t <tmux_session>`:
   - **alive + daemon socket answers** → attach; restore nav state, pane focus, BL pane target, scroll positions; render exactly where the user left off.
   - **tmux alive, daemon dead** → banner *"daemon exited at <time> — restart it?"* in the session's row.
   - **tmux dead** → banner *"backend session gone — recreate (re-run daemon) or pick another?"* on first interaction.
4. **Fallback** — global state missing, or the resolved session does not exist anywhere → land on Sessions mode, home-base selected.

`--project <dir>` at launch overrides the resume path: if a backend rooted there is alive, attach; else spawn and attach.

### Reconciliation rules

On startup the frontend reconciles `sessions/` toml files with `tmux list-sessions`:

| toml | tmux | meaning | UX |
|---|---|---|---|
| present | alive | normal | attachable, no banner |
| present | dead | daemon stopped or tmux server restarted | "restart this backend?" |
| absent | alive (matching name pattern) | orphan; another DevEnv or tmux user created it | offer to adopt → write a toml from observed state |
| present | never existed | stale toml | offer to delete |

The reconcile is cheap (`tmux list-sessions -F` is one syscall round-trip); run on startup and after Sessions-mode actions.

### Multi-frontend support

**Free from tmux:** multiple frontends can attach to the same backend's panes simultaneously via tmux's native multi-attach. Two laptops viewing the same `sot-be-mypackage:llm` pane share the vt100 stream identically.

**Free from ADR 0010:** the daemon socket already supports `(client_id, last_seen_revision)` reconnect with ring replay. Two concurrent clients on the same backend each get their own client_id and read replay independently — server-side state is shared, per-client read pointers are not. (Confirming this works for *concurrent* clients — not just sequential reconnect — is a phase-2 spike before claiming the surface.)

**Phase-2 write policy:** first-attached client gets read+write; subsequent clients are read-only followers per ADR 0010's stated deferred policy. Writer takeover via explicit key. Simultaneous writes are a phase-3 problem.

### Cross-host scope

Phase 2 = **single host.** Sessions mode lists backends on the host the frontend's tmux client points at. Switching hosts means relaunching the frontend pointed at a different remote. Phase 3 may add "host" as a tree level above sessions, federating across multiple hosts.

### What this displaces

- ADR 0010's `devenv-backend-<session_id>` single-session naming is generalized: the daemon's tmux session becomes one of many under the `sot-be-*` namespace. Phase 1's existing single-backend invocation is just the case where `sot-be-<default-slug>` is the only session.
- The `--socket` flag stays load-bearing; sockets are now per-backend, derived from session_id.

## Consequences

- **No new daemon process.** Orchestrator role = a frontend-side library + tmux. Tens of lines of `tmux -F` parsing, not a new crate.
- **Tmux server is now load-bearing infrastructure.** If the tmux server dies all backends die with it. Daemons run inside its panes — their lifecycle is coupled to the pane. This is acceptable because tmux is already load-bearing for the LLM pane and the historical single-backend session.
- **Resume is the default UX.** No flag, no picker, just "open and you're where you were." First-time launches with no state file fall through to the Sessions mode landing with home-base.
- **Multi-attach is free; multi-write is deferred.** Phase 2 = view-only second clients. Phase 3 may revisit.
- **Cross-machine is the same as phase 1.** Existing SSH-L forwarding of the daemon socket extends to each backend's socket; the BL pane's tmux forwarding is unchanged.
- **State files don't track project mutation.** If the user moves `project_dir` outside DevEnv we notice via tmux session dead → daemon dead → reconcile prompt. The toml is a pointer, not a copy.
- **`$SOT_PROJECTS_ROOT` is settings-driven.** Defaults to `$HOME/julia_dev`. Sessions-mode picker is scoped there; future Files-mode-goes-universal (Option B in the design discussion) can later replace the picker with a free-roam Files mode + "make-this-a-backend" key, without changing the rest of this design.
- **Phase 2 implementation order**: ADR (this doc) → backend sockets per-session naming → tmux query/spawn helpers in frontend → Sessions mode rendering → picker → startup resume → reconciliation banners.
