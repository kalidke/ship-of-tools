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
of those can do anything. `scripts/launch-sot.ps1`'s `-Local` switch now
ensures one, idempotently, on the fixed per-user named pipe
`\\.\pipe\sot-<user>-local` (`sotd --label`'s own auto-derivation is a Unix
runtime-dir scheme with no Windows branch, so this pipe name is constructed
explicitly, following the one real Windows precedent already in the repo,
`hosts.toml`'s `[host.local] socket = "\\.\pipe\sot-local"`), with
`--project-root` the user's home and `--label local` — the same
project-root-is-home and `--label` convention the backend host's own `sotd
--project-root $HOME --label sot` already uses. Scoped to `-Local` for now,
not every launch mode: nothing in the default (remote-tunnel) flow consumes
the local daemon until L2 (frontend holds one connection per host, local
included) — which is also when this ensure moves to every launch — and
starting it unconditionally today would let it pin `<prefix>\bin\sotd.exe`
(a mapped image while the process runs, on Windows) before the default
launch's own apply/rebuild step gets a chance to update it: pure exposure,
no benefit yet. **Superseded by the L2b amendment below**: the ensure now
runs on every launch mode, positioned after the apply step so a freshly
applied binary is what it sees, with the local daemon stopped first only
when an update is actually pending. Binary+capsule resolution prefers the COMPLETE dev pair
(`sotd.exe` AND `sot-capsule.exe` both present in the dev checkout's
`rust\target\release`) over the complete install pair, refusing only when
NEITHER pair is complete — matching, not opposing, the frontend's own
dev-build-first resolution: `-Local` runs the dev frontend from this same
checkout, so the daemon and its capsule runtime (L1a's own
`current_exe().parent()` resolution) must come from that same origin or a
dev frontend ends up talking to an older installed `sot-capsule.exe` —
exactly the skew ADR 0041's "same release" rule warns about. The daemon is
spawned detached, so it outlives the launcher and every frontend relaunch;
`-Local` connects the frontend to this persistent daemon instead of
spawning a fresh per-session one, as it used to. `scripts/shutdown-sot.ps1`
stops it LAST, after the frontend and tunnel are already down — sotd has no
clean-stop IPC op and installs no signal handler on either platform
(Linux's own `sotd.service` has no `ExecStop` either, so its "graceful"
stop is already just systemd's unhandled default SIGTERM), so
`Stop-Process` is what "clean stop" reduces to here. Capsule workspace
supervisors (`sot-capsule.exe`) are never touched by this — they are
separate detached processes and the one authority over a workspace's live
state, and the daemon re-adopts every still-running one via `--resume` the
next time it starts, which is exactly what a shutdown-and-relaunch does.
Liveness (the idempotency check, the readiness wait, and stop confirmation)
is a bounded named-pipe CONNECT probe, not a namespace listing — a pipe
NAME persists under `\\.\pipe\` while any dead client still holds a handle
to it, so presence there is not health. Logic lives in a new standalone
`scripts/sot-local-daemon.ps1` (start/`-Stop`), tested by
`scripts/tests/test-local-daemon.ps1` on the Windows CI leg.

Three properties this slice relies on without building them further: (1)
the daemon is started with a plain `Start-Process`; it outlives the
launcher and every frontend respawn only when the launcher itself runs
outside a kill-on-close job, which is how the shortcut and a console both
start it — the same assumption the launcher's own frontend-supervisor loop
already rests on. (2) the fixed per-user pipe has the same trust model as
the loopback tunnel port — a hostile co-user on a shared Windows box could
squat either name; a personal workstation is the deployment target. (3)
upgrade transaction: a running daemon pins its `sotd.exe` and a running
supervisor pins its `sot-capsule.exe` as mapped images — rebuilding the
backend from the checkout requires stopping the local daemon first
(`sot-local-daemon.ps1 -Stop` or the shutdown script); the general,
versioned-runtime-dir answer is U4's upgrade transaction (see "What is
deliberately NOT built" below).

Out of scope here: connecting the frontend to the local daemon in the
default (remote-tunnel) launch mode, and starting the local daemon on every
launch mode — both are L2's "one connection per host" below.

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

**Amendment 2026-09-02 — L2b: the launchers open one tunnel per remote,
start the local daemon on every launch, and the local endpoint has one
derivation.** L2a shipped the frontend half (`hosts::resolve_connections`,
one connection per `hosts.toml` entry, local-first when present); this
closes the launcher half the L2 bullets above assumed but didn't build yet.
Four pieces:

(A) **One derivation.** `session_socket_path(label)` — and the private-
runtime-dir resolution it needs on Unix — moved from `rust/backend` into
`sot_protocol::session_socket`, gaining the Windows branch it lacked
(`\\.\pipe\sot-<user>-<label>`, `<label>` run through the same `slug()`
every platform uses). The backend re-exports it (`sotd --label` and `sotd
session-socket-path` are unchanged call sites); the frontend now calls it
directly for its implicit local connection. One function, three
consumers, never a second guess of what the pipe name is.

(B) **Local is implicit.** `hosts::resolve_connections` adds a `local` entry
FIRST unconditionally — no `hosts.toml` section required — with its
endpoint defaulting to `session_socket_path("local")`. An explicit
`[host.local]` section still works but now only for overriding `socket`;
every other field on it is ignored, since local is never tunneled. The
`hosts.toml.example` `[host.local]` block became one sentence.

(C) **The daemon names its own pipe.** `sot-local-daemon.ps1` resolves the
dev-or-install binary pair first (unchanged preference order), then asks
IT — `sotd session-socket-path local` — for the pipe path used by the
probe, the spawn argv, and the `-Stop` match. The script's own
`"sot-$env:USERNAME-local"` construction (and `launch-sot.ps1`'s matching
copy, which also stopped passing `--socket` for `-Local` — the frontend
derives it via (B)) is deleted. Consequence: `-Stop` with no `-PipeName`
test override now also needs a resolvable binary pair to know what to
stop, where it previously didn't — accepted, since a running daemon keeps
its own binary resolvable at its original location while it runs.

(D) **Every launch ensures the local daemon.** The ensure moved out of the
`-Local`-only block to run on every launch, right after the staged-update
apply and before either mode's frontend starts. A pending update (the
apply step's own staged-update pointer, or `SOT_LAUNCH_REBUILD` from a
successful self-update pull) stops the local daemon first, so it never
pins a binary about to be replaced — checked, not unconditional, so an
ordinary launch pays nothing extra. `-Local` still hard-errors on failure;
the default mode fails open (the `local` row just shows unreachable).

(E) **One tunnel per remote.** Both launchers now iterate every
`[host.<name>]` entry with an `ssh_alias` and open its own SSH forward,
ensuring that host's backend the same way `default_host`'s always has
(`New-RemoteEnsureCommand` / `sot_ensure_remote_host`) — but nonfatal per
host: an unreachable remote logs one line and the launch continues, and
the frontend's own hosts.toml read shows it unreachable. `tcp_port` is
required per remote; `default_host` alone may still fall back to
`SOT_TCP_PORT`/18743. `shutdown-sot.ps1` now discovers every configured
port (via the same `Get-TunnelPlan`) so its tunnel-kill regex covers all
of them, not just `default_host`'s. `-Local` still opens no tunnels at
all. `Read-SotHosts`/`Get-TunnelPlan` (PowerShell) and
`sot_hosts_table`/`sot_tunnel_plan` (bash) — pure, no ssh — moved into
`scripts/sot-hosts.ps1`/`scripts/sot-hosts.sh`, shared by both launchers
and their own tests (`scripts/tests/test-tunnel-plan.{ps1,sh}`).

Not built here: L3 (remote attach, below), ADR 0035 per-host proxying, or
retiring `-Local` (it keeps its "no tunnels, no freshness" meaning).

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
- No versioned-runtime-dir / atomic-swap answer for L1c's OWN
  launcher-managed local `sotd.exe`/`sot-capsule.exe`: while running they
  are mapped images (Windows) that a rebuild-from-checkout cannot replace in
  place, so a dev must stop the local daemon first
  (`sot-local-daemon.ps1 -Stop` or the shutdown script); the general answer
  is U4's upgrade transaction, same as above.

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
