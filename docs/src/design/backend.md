# Backend & Sessions

!!! note "Status: partly built"
    The daemon, its per-row Julia kernel and REPL, and the socket transport are built. An in-app orchestrator LLM
    session is not built (see [Requirements](requirements.md)), and the daemon starts no tmux sessions.

The backend daemon owns project state. It is the only process the frontend talks
to over the [line protocol](protocol.md), and it sits between the frontend and
the Julia kernels it supervises. For the big picture and the reasoning behind the
language split, read [Architecture at a Glance](../guide/architecture.md).

## What the daemon owns

A single long-lived Rust process (tokio) holds the row registry, the file watchers and the kernel and REPL children:

- **Rows** — the registry of rows (workspaces), each with its project directory, its kernel and REPL, and its file
  watcher.
- **File watching** — a `notify` watcher per workspace turns external edits into
  `preview.changed` events ([protocol](protocol.md)).
- **Process supervision** — it spawns and supervises each row's Julia kernel and REPL, restarting the kernel on
  crash and surfacing failures as protocol events.

The frontend never touches kernel state directly, even when everything runs on
one laptop. Local-only operation is "a client connecting to the local daemon's
socket," not a separate code path.

## Kernel launch and supervision

The daemon spawns the kernel as a child process and frames the conversation over
its stdio, per the [line protocol](protocol.md):

```bash
julia --project=<repo>/julia/kernel --startup-file=no \
  -e 'using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout; project_root="<row root>")'
```

Spawning is `tokio::process::Command` through `lifecycle::child_signal::Signal::spawn`, which puts the
child in its own process group (Unix) or job (Windows). Each stderr line
goes to the daemon log at debug, and a generation that fails logs the
child's last 30 stderr lines at warn — the *only* place kernel stderr
surfaces, so user-facing errors must travel as protocol events, not prints.

Restart policy differs by process:

| Process | On exit |
|---------|---------|
| Kernel | auto-restart, backoff from 250 ms doubling to 30 s, started over only after a kernel that served a minute after its hello; callers get `Dead` meanwhile; state rebuilds from disk, so restart is safe |
| REPL | not restarted in the background: its state is shown as `dead`, and the next eval starts a new REPL, which says why the old one ended |

Orphaned `julia` processes are unacceptable, so the whole tree of every process
the daemon starts, but those ADR 0050 names as outside it, is killed (SIGKILL to its process
group on Unix, `TerminateJobObject` on Windows) when its owner lets go of it or
the daemon's controlled end fires the child signal, whichever comes first (ADR 0050, "controlled exits"). The tree is killed before its
leader is reaped, because the leader's pid is the group's number. There is no
SIGTERM grace and no `taskkill`. A process that deliberately left, for example a
Unix descendant that called `setsid` or `setpgid`, is the one exception (ADR 0050,
residual 7).

## Transport, persistence, reconnect

A common workflow is remote: the window runs on one computer and reaches a daemon on another, where the kernel and
GPU live. Local operation is the fall-through. The session must survive SSH disconnects, reconnect from a fresh
client, and not depend on host-specific port allocation.

- **Backend as a user service on the remote.** The daemon runs as the
  `systemd --user` unit `sotd.service` (installed by the release installer),
  so its lifecycle survives SSH drops.
- **A per-user local socket, not a TCP port.** On Unix it listens at
  `$XDG_RUNTIME_DIR/sot/sessions/<label>.sock` (a private runtime folder; `sot` is the label of the persistent
  daemon), on Windows at a named pipe. Cross-machine, the frontend reaches
  it by spawning its own `ssh` child rather than forwarding a port — see
  [Transport](../concepts/transport.md).
- **Transport auth.** The daemon has no TCP listener and no app-level token. The socket's folder is private to the
  user, the daemon checks the connecting process's OS identity when it accepts, and cross-machine access is `ssh`.
- **Reconnect carries revision.** Every connect sends
  `{session_id, client_id, last_seen_revision}`. The backend replays missed
  events from a bounded ring or marks a full snapshot pending if the client is too far
  behind. Each connection pings every 30 s, and the daemon reaps one that has been silent for 90 s.

## Workspaces

One daemon holds many rows (workspaces), reached over one socket and one `ssh` child. Each row has its own Julia
kernel and its own persistent REPL, and the daemon routes an op to them by `workspace_id`.

- The kernel is a direct child of the daemon, framed over stdio. It runs the daemon's own `julia/kernel` project
  and reads the row's sources statically; it never runs user code and never loads the user's environment.
- User code runs in the row's REPL, also a daemon child. The REPL activates the row's project directory as its
  environment, so two rows can use different `Project.toml` environments (`MyPackage@0.1` in one, `@0.2` in
  another) in separate Julia processes. Variables live in the REPL.
- **Switching is a frontend state change.** There is no `workspace.switch` op: the window sets its active workspace,
  tells the daemon with `workspace.activate`, and re-fires `tree.root` with the new id. Nothing restarts, so a REPL
  holding `x = 5` still holds it when you return to its row.
- **`workspace_id` is optional on tree, preview, kernel, REPL, concept and pty ops**; a missing field resolves to the
  default workspace. Lifecycle ops are `workspace.list`, `workspace.create` and `workspace.destroy`.

The daemon owns its kernel and REPL children, so ending the daemon ends them and a REPL's variables go with it. The
rows themselves, each running its agent under a capsule supervisor, outlive the daemon (see
[Sessions and persistence](../concepts/sessions.md)).
