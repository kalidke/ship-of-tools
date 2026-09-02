# ADR 0042: First-class local sessions and the multi-host selector

**Status:** Proposed (2026-09-02) — for the maintainer's ratification before
any build. Implements the owner's 2026-09-01 destination for the Ship's Log
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
| `tmux` (today, Linux) | a tmux pane the daemon owns | `pty.open` byte stream over the daemon connection | daemon restarts (ADR 0038 keeper) |
| `capsule` (new; the only runtime on Windows) | a voyage-recorded capsule under its own supervisor | the U3 attach client, on the host's attach lane | frontend AND daemon restarts (the supervisor is a separate process; the record is on disk) |

Local sessions are `capsule` workspaces of the local daemon. They are never
drawer tenants. Messaging between sessions on any hosts is sot-comm exactly
as today: each session's agent joins the relay through the host's tunnel to
the backend daemon, which is what the drawer's agent already does.

## Build order — three slices, each landing green and off by default

### L1 — the capsule workspace runtime (daemon + frontend, one host)

- `workspace.create` gains `runtime: "tmux" | "capsule"` (default: `tmux`
  on Unix, `capsule` on Windows — the platform decides; no user knob unless
  a host offers both). A capsule workspace's state directory lives under the
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

- The Windows host gets capsules as its ONLY session runtime, so P5's "the
  tmux path is deleted" question never arises there — it starts capsule-only.
- The frontend stops being single-daemon; every host is a peer. The drawer
  keeps its special tenant but not a special transport.
- Records for local sessions live on the local disk under the daemon's
  state root — the same durability rules as everywhere (ADR 0039).
