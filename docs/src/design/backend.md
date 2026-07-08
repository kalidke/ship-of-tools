# Backend & Sessions

The backend daemon owns project state. It is the only process the frontend talks
to over the [line protocol](protocol.md), and it sits between the frontend and
the Julia kernels it supervises. For the big picture and the reasoning behind the
language split, read [Architecture at a Glance](../guide/architecture.md).

## What the daemon owns

A single long-lived Rust process (tokio) holds everything stateful:

- **Project state** — the mode trees, preview caches, and per-workspace
  navigation state the frontend reads and mutates over the protocol.
- **File watching** — a `notify` watcher per workspace turns external edits into
  `file.changed` events ([protocol](protocol.md)).
- **Process supervision** — it spawns and supervises the Julia kernel(s) and the
  REPL, restarting the kernel on crash and surfacing failures as protocol events.
- **The orchestrator LLM session** — the Anthropic client and tool-use loop live
  here, not in the frontend.

The frontend never touches kernel state directly, even when everything runs on
one laptop. Local-only operation is "a client connecting to a localhost-bound
backend," not a separate code path.

## Kernel launch and supervision

The daemon spawns the kernel as a child process and frames the conversation over
its stdio, per the [line protocol](protocol.md):

```bash
julia --project=<repo>/julia/kernel \
  -e 'using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout)'
```

Spawning is `tokio::process::Command` with `kill_on_drop(true)`. stderr is
captured into a backend log ring buffer — the *only* place kernel stderr
surfaces, so user-facing errors must travel as protocol events, not prints.

Restart policy differs by process:

| Process | On exit |
|---------|---------|
| Kernel | auto-restart, exponential backoff (1s → 16s, then surface to UI); state rebuilds from disk, so restart is safe |
| REPL | never auto-restarts — a crashed REPL is meaningful; the user decides, and the UI must say "REPL is dead, press X to restart" |

Shutdown is platform-aware because orphaned `julia` processes are unacceptable:

- **Linux** — SIGTERM, wait 5s, then SIGKILL.
- **Windows** — `taskkill /F /T /PID <pid>`; tokio's `kill()` alone leaves
  grandchildren, so SIGTERM semantics are not relied on.

## Transport, persistence, reconnect

The primary workflow is remote: a Windows-local frontend reaches a Linux remote
host where the kernel and GPU live. Local operation is
the fall-through. The session must survive SSH disconnects, reconnect from a fresh
client, and not depend on host-specific port allocation.

- **Backend in tmux on the remote.** The daemon runs inside a named tmux session
  so its lifecycle survives SSH drops and its stderr/logs surface in a pane you
  can `tmux attach` to.
- **Per-session Unix socket, not a TCP port.** It listens at
  `$XDG_RUNTIME_DIR/sot/<session_id>.sock`. The frontend forwards that remote
  socket to a local one over SSH (`-L`, with `StreamLocalBindUnlink=yes` and
  `ServerAliveInterval=15`). Where Unix-socket forwarding isn't available — older
  Windows OpenSSH — it falls back to a per-session local TCP port allocated at
  runtime, never fixed.
- **Transport auth.** SSH authenticates the Unix user for remote socket access,
  and the socket path is private to that user. Direct TCP remains token-gated
  because `localhost` on a shared remote is machine-scoped, not user-scoped.
- **Reconnect carries revision.** Every connect sends
  `{session_id, client_id, last_seen_revision}`. The backend replays missed
  events from a bounded ring or sends a full snapshot if the client is too far
  behind; heartbeats (~5s) evict stale clients. `last_seen_revision` is what makes
  reconnect feel like tmux — without it every disconnect would lose the
  orchestrator session and any in-flight kernel work.

`client_id` is in the handshake from day one; multi-client policy (first client
read+write, second read-only follower) is stated but deferred.

## Sessions

A session is one project's backend, registered and supervised by tmux rather than
by a second daemon. The insight here is that tmux already lists, supervises,
multi-attaches, survives disconnect, and forwards across machines — so **tmux is
the registry**, and the "orchestrator role" is a frontend-side library plus
`tmux -F` queries, not a new crate.

- **One tmux session per backend, plus `sot-home`.** Sessions are named
  `sot-be-<slug>` (from `Project.toml`'s `name`, falling back to the directory
  basename, with a `-N` disambiguator on collision). Home base is `sot-home`.
- **Durable metadata on disk.** Tmux is runtime-only, so pointers live under
  `~/.config/sot/`: `state-<hostname>.toml` records where the frontend left
  off (the `-<hostname>` suffix matters because the Linux boxes share `$HOME`);
  per-workspace toml records how to bring a backend back.
- **Sessions mode** is a normal [mode](../guide/architecture.md): col 1 lists
  sessions with a running/unreachable/stopped sigil, col 2 the panes, col 3 pane
  metadata, and the preview is a live `tmux capture-pane` tail.
- **Resume is the default.** Launched with no flags, Ship of Tools reads
  `state-<hostname>.toml`, resolves the last session's toml, and — if the tmux
  session is alive and the daemon socket answers — restores nav state, focus, and
  scroll exactly where you left off. If the daemon died, or the tmux session is
  gone, it shows a banner offering to restart or recreate. A startup
  reconciliation pass matches `sessions/*.toml` against `tmux list-sessions`
  (one cheap syscall) and offers to adopt orphans or delete stale toml.

## Workspaces

Two requirements surfaced after the initial sessions design: switching projects
must not stop the Julia kernel, and the deployment must stay one tunnel, one
socket. The earlier "frontend reconnects to a different daemon socket per
session" approach satisfied the first but fought the second, so the two
concerns — where kernels live vs. which process is the protocol entry point — are
split.

**One daemon. One Julia kernel child per workspace, spawned directly by the
daemon and routed by `workspace_id`.**

- The kernel is a direct child of the daemon (`std::process::Command`,
  `--project=<workspace.project_root>`, framed over stdio), **not** a tmux
  process. The workspace's tmux session still holds the shell/BL pane and any
  optional LLM pane, but not the kernel.
- The load-bearing invariant: **switching the active workspace never restarts a
  kernel or rebuilds Julia state.** The kernel holding `x = 5` keeps holding it
  while you visit another workspace and return.
- **Switching is a frontend state change.** There is no `workspace.switch` op;
  the frontend just sets `active_workspace_id` and re-fires `tree.root` with the
  new id. Cost is one round-trip plus a redraw.
- **`workspace_id` is additive.** Tree/preview/kernel/repl/concept/pty ops gain
  an optional `workspace_id`; a missing field resolves to the default workspace,
  so existing single-backend invocations keep working. Lifecycle ops are
  `workspace.list`, `workspace.create`, `workspace.destroy`.
- **Kernels can run different `Project.toml` environments** — each is its own
  Julia process, so `MyPackage@0.1` in one workspace and `@0.2` in another
  coexist.

The trade-off: the daemon owns its kernel children, so a daemon restart loses
Julia state across all workspaces. That buys away kernel-discovery sweeps and
health probes; spawning sits behind a `KernelHost` trait so a tmux-hosted
implementation can be swapped in later (phase 3) without protocol changes, if
"kernel survives daemon restart" ever becomes a felt pain point. Cross-machine
deployment is unchanged — one daemon, one socket, one `ssh -L`, with workspace
switching in-band on that single connection.
