# ADR 0042: First-class local sessions and the multi-host selector

**Status:** Accepted (2026-09-02, maintainer: "yes. confirm"). Ratified with
one rule sharpened at the maintainer's direction: the capsule is the DEFAULT
runtime for every NEW session on EVERY host from L1 on; tmux exists only to
keep already-running sessions alive until they end, and is then deleted
(ADR 0037 P5). "Nothing will run on tmux on the new system." Implements the owner's 2026-09-01 destination for the Ship's Log
ladder (ADR 0037, ADR 0041 build-order amendment): sessions from several
hosts in one selector, separated by the wheel icon, with the local machine
being just another host and local sessions being ordinary sessions.
**Date:** 2026-09-02

## The decision in one paragraph

"Local is just another host" is taken literally: the frontend machine runs
its own `sotd` (the daemon already ships and is smoke-tested on Windows)
and its sessions are ordinary workspaces of that daemon. Because Windows has
no tmux, that daemon's workspace runtime is the CAPSULE: one
`sot-capsule supervise <state-dir>` authority per workspace (the merged step
6 U2), and the frontend's session pane attaches to the workspace's voyage
with the merged step 6 U3 client instead of a `pty.open` stream. The
Sessions selector becomes the union of every connected host's workspace
list, grouped under host nodes. Nothing new is invented: the state directory,
the pointer, the fence, the lane and the attach client are all step 6; the
files/preview/REPL machinery per workspace is what the daemon does today;
the host registry is `hosts.toml`. The drawer's contract (ADR 0041, one
drawer, tenant fixed: the SoT LLM plus terminal/monitor/Julia) is unchanged.

## What "first-class" means, precisely

A session row in the selector is a workspace of SOME host's daemon. Every
row has the same surface: a project root (nav + preview served by that
daemon), a Julia kernel (that daemon's), and an agent pane. The agent pane
is fed one of two ways, chosen by the workspace's `runtime`:

| `runtime` | who runs the agent | how the pane is fed | survives what |
|---|---|---|---|
| `capsule` (the default for every NEW session, every host) | a voyage-recorded capsule under its own supervisor | the U3 attach client, on the host's attach lane | frontend AND daemon restarts (the supervisor is a separate process; the record is on disk) |
| `tmux` (legacy; ALREADY-RUNNING sessions only, until they end) | a tmux pane the daemon owns | `pty.open` byte stream over the daemon connection | daemon restarts (ADR 0038 keeper) |

Local sessions are `capsule` workspaces of the local daemon. They are never
drawer tenants. Messaging between sessions on any hosts is sot-comm exactly
as today: each session's agent joins the relay through the host's tunnel to
the backend daemon, which is what the drawer's agent already does.

## Build order — three slices, each landing green and off by default

### L1 — the capsule workspace runtime (daemon + frontend, one host)

- `workspace.create` creates a CAPSULE workspace on every host; `runtime:
  "tmux"` is never chosen for a new session (no knob). Existing tmux
  workspaces keep running as `runtime: "tmux"` rows until they end; the
  daemon creates no new ones. When the last one is gone, the tmux path is
  deleted (ADR 0037 P5) — there is no soak switch, because Windows starts
  capsule-only and the backend host converges by attrition. A capsule workspace's state directory lives under the
  daemon's state root, `<state-root>/workspaces/<workspace-id>/`, and holds
  exactly what step 6 defines: `supervisor.lock`, `drawer.voyage` (the name
  stays; it means "this state directory's voyage pointer"), the journal, the
  voyages. The daemon starts `sot-capsule supervise <state-dir> --start --
  <agent launcher>` with the same breakaway attempt and DEGRADED marker step 6
  pins, and `--resume` on its own restart; it never touches the capsule
  otherwise — the supervisor is the one authority (ADR 0041 Lifecycle).
- `workspace.list` entries gain `runtime` and, for capsule workspaces,
  `state_dir` (the host-local path) and the supervisor's `status` phase.
  `pty.open` on a capsule workspace is refused with `code = attach_direct`;
  the frontend attaches with the U3 client. `workspace.delete`/end routes
  through `end_run` on the supervisor lane; a new voyage is `reset`.
- Frontend: the session pane gains the same `local_term` / `attach_term`
  split the drawer got in U3, keyed by the selected workspace's `runtime`.
  One attach client per selected capsule workspace; deselecting detaches (a
  watcher leaving costs nothing; the record continues).
- Acceptance (real Windows box, CI where it can): a capsule workspace with
  `claude` as its agent appears in Sessions; selecting it shows the live
  screen; typing takes the pen; the frontend is killed and relaunched and the
  screen comes back from the checkpoint; the daemon is restarted and the
  workspace is re-adopted; `end_run` ends it and the record verifies.

**Amendment 2026-09-02 — L1c: the launcher starts and stops the local
daemon.** L1a shipped the daemon-side capsule runtime and L1b the frontend's
attach pane; this closes the remaining gap the decision paragraph above
asserted ("the frontend machine runs its own `sotd`") but the ADR text never
specified: getting a local `sotd` running at all, unattended, before either
of those can do anything. `scripts/launch-sot.ps1` now ensures one on EVERY
launch (not only `-Local`): idempotently, on the fixed per-user named pipe
`\\.\pipe\sot-<user>-local` (`sotd --label`'s own auto-derivation is a Unix
runtime-dir scheme with no Windows branch, so this pipe name is constructed
explicitly, following the one real Windows precedent already in the repo,
`hosts.toml`'s `[host.local] socket = "\\.\pipe\sot-local"`), with
`--project-root` the user's home and `--label local` — the same
project-root-is-home and `--label` convention the backend host's own `sotd
--project-root $HOME --label sot` already uses. Binary resolution prefers
the release install's `<prefix>\bin\sotd.exe` over the dev checkout (the
opposite priority from the frontend's own dev-build-first resolution,
because this daemon is meant to sit untouched across many launches, not be
rebuilt every session), and refuses to start — logging why, without blocking
the rest of the launch — when `sot-capsule.exe` is not its sibling (L1a's own
`current_exe().parent()` resolution). The daemon is spawned detached, so it
outlives the launcher and every frontend relaunch; `-Local` now connects the
frontend to this persistent daemon instead of spawning a fresh per-session
one. `scripts/shutdown-sot.ps1` stops it LAST, after the frontend and tunnel
are already down — sotd has no clean-stop IPC op and installs no signal
handler on either platform (Linux's own `sotd.service` has no `ExecStop`
either, so its "graceful" stop is already just systemd's unhandled default
SIGTERM), so `Stop-Process` is what "clean stop" reduces to here. Capsule
workspace supervisors (`sot-capsule.exe`) are never touched by this — they
are separate detached processes and the one authority over a workspace's
live state, and the daemon re-adopts every still-running one via `--resume`
the next time it starts, which is exactly what a shutdown-and-relaunch does.
Logic lives in a new standalone `scripts/sot-local-daemon.ps1`
(start/`-Stop`), tested by `scripts/tests/test-local-daemon.ps1` on the
Windows CI leg. Out of scope here: connecting the frontend to the local
daemon in the default (remote-tunnel) launch mode — that is L2's "one
connection per host" below; until then the local daemon simply runs
alongside, unconnected, in every launch mode but `-Local`.

### L2 — one connection per host, one tree

- The frontend holds one transport per host in `hosts.toml` that it can
  reach (the local daemon is always one of them); the launcher opens a tunnel
  per remote host as it does for one today. Hello, revision replay and
  reconnection are per connection — the transport already owns those.
- The Sessions tree is the union of the hosts' workspace lists, one host
  node each, the wheel icon between hosts, the local host first. Files,
  preview, REPL and workspace ops route to the connection that owns the
  selected workspace. Mode Hosts stops being a quit-and-relaunch switch
  (ADR 0015) and becomes the list of connected and unreachable hosts.
- Acceptance: the backend host and the local host in one selector; a
  session on each attached in turn; a remote host going away marks its node
  unreachable without disturbing the others.

### L3 — remote attach (the P4 bridge, frontend half)

- A capsule workspace on a REMOTE host is attached through that host's
  daemon: a proxied lane (`attach.proxy` / `mgmt.proxy` ops carrying the
  attach-protocol bytes over the already-authenticated daemon connection).
  The attach protocol is transport-independent by design (ADR 0037, ADR
  0041 P4 note); the daemon adds no semantics, only bytes. The same
  challenge runs end to end because the daemon forwards, never answers.
- Acceptance: a microscope-control PC's capsule session driven from the main
  frontend; the tunnel dropping and returning re-attaches from the
  checkpoint; take-on-first-input and exactly-once input hold across it.

## What is deliberately NOT built

- No session registry, catalog, or second source of truth: the daemon's
  workspace list is the list; the state directory is the record's home.
- No second drawer and no drawer changes: U4 (drawer cutover, upgrade
  transaction) proceeds after L1–L3 as amended; once L1 exists the drawer is
  one more capsule session whose tenant is fixed.
- No new transport for L1 or L2: the daemon connection and its tunnel are
  the transport; L3 proxies over it.
- No cross-host session moves, forks or timelines: those are ADR 0037's
  "later, cheaply" list and stay there.

## Open questions for the maintainer

1. The Windows daemon runs Julia kernels per workspace; the scope PCs run
   Julia already, the frontend workstation may not — is a capsule workspace
   without a kernel acceptable on a host with no Julia (nav + preview + agent
   only)?
2. Default agent launcher for a local capsule session: the same `ccb`-style
   wrapper the drawer uses, or a plain shell with the agent started by the
   user? Proposed: the drawer's wrapper, so a new local session is
   comm-aware from its first turn.
3. Naming: a local session is named by its project root's basename like a
   backend workspace; is the wheel-icon host label the machine name from
   `hosts.toml`?

## Consequences

- Every host gets capsules as the runtime for every new session; the
  backend host converges by attrition as its running tmux sessions end,
  after which the tmux path is deleted (P5) with no switch to flip.
- The frontend stops being single-daemon; every host is a peer. The drawer
  keeps its special tenant but not a special transport.
- Records for local sessions live on the local disk under the daemon's
  state root — the same durability rules as everywhere (ADR 0039).
