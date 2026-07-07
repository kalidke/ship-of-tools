# ADR 0014: Workspaces — one daemon, direct-child kernels per workspace, routed by workspace_id

**Status:** Accepted
**Date:** 2026-05-15
**Amends:** [ADR 0013](0013-backend-sessions.md)

## Context

ADR 0013 introduced "session-per-backend": each project gets its own tmux session containing its own `devenv-backend` daemon, the frontend reconnects to that daemon's socket on session switch. The tmux plumbing (B1–B6) is built and works. What is not built is the frontend transport reconnect on switch — and on revisit the user surfaced two requirements that change the calculus:

1. **Switching workspaces must not stop the Julia kernel.** State (variables, loaded modules, in-flight computations) must be preserved across switches; switching is expected to be fast and frequent, "like tmux windows in the same session."
2. **The deployment topology stays Windows-frontend → Linux-backend over `ssh -L`.** Per-session SSH tunnels or a multi-socket proxy are unwanted complexity; one tunnel, one socket is the contract.

ADR 0013's "frontend reconnects to a different daemon socket per session" satisfies (1) — each daemon's kernel keeps computing — but fights (2). It also conflates two concerns:

- Where the **kernel processes** live (so they survive switches).
- Which process is the **protocol entry point** for the frontend (so the cross-machine connection stays simple).

Splitting these gives us a cleaner model.

## Decision

**One backend daemon. One Julia kernel child per workspace, spawned directly by the daemon and routed by `workspace_id`.**

```
backend host (Linux, native or via WSL on Windows)
├── devenv-backend daemon                    ← single process, frontend's only entry point
│   listens on one Unix socket / TCP port
│   supervises one Julia kernel child per active workspace
│   routes every op by workspace_id
│
└── tmux server
    ├── sot-be-home                       ← home base, default workspace
    │   ├── shell pane (BL pane target)
    │   └── llm pane   (optional claude agent)
    ├── sot-be-mypackage
    │   └── … same shape
    └── sot-be-experiment2
        └── …
```

**The Julia kernel does not live in tmux.** It is a direct child of the daemon, spawned via `std::process::Command` with `--project=<workspace.project_root>`, framed over stdio exactly as today. Lifetime is tied to the daemon. The tmux session for a workspace still exists and still holds the BL/shell pane and any optional claude-agent pane — but the kernel is not in it.

### Workspace lifecycle

| Process | Owned by | Dies when | Restart cost on switch |
|---|---|---|---|
| Daemon | systemd / tmux pane / shell | only on explicit kill | n/a — single instance |
| Julia kernel | daemon (direct child) | workspace destroyed, or daemon dies | **none on switch** — kernel stays running |
| File watcher | daemon (per-workspace) | workspace destroyed | none on switch |
| BL/LLM ptys | tmux panes | tmux session destroyed | none on switch — pane retargets |
| Frontend | desktop | user closes | resumes via `state-<hostname>.toml` |

The load-bearing invariant: **switching the frontend's active workspace never restarts a kernel or rebuilds Julia state.** The kernel that holds `x = 5` keeps holding `x = 5` while you visit another workspace and come back.

### Workspace registry

In-memory `HashMap<workspace_id, WorkspaceState>` on the daemon, persisted to disk at:

```
~/.config/devenv/workspaces/<slug>.toml
```

Per-workspace toml:

```toml
workspace_id  = "<uuid>"
label         = "MyPackage.jl"
slug          = "mypackage"
project_root  = "/home/user/julia_dev/MyPackage.jl"
tmux_session  = "sot-be-mypackage"
created       = 2026-05-15T23:00Z

[kernel]
status        = "stopped"   # "running" while a child is alive — written on spawn, cleared on death
pid           = 0
started       = 0

[nav_state]                 # frontend-managed, daemon preserves on read-modify-write
mode          = "files"
cursor_path   = "src/lib.jl"
scroll_lines  = [0, 12, 0, 0]
```

Discovery on daemon startup: scan `~/.config/devenv/workspaces/*.toml`, build the registry, **do not** eagerly spawn kernels. Kernels lazy-spawn on the first `kernel.request` (or analogous op) for that workspace.

The `sessions/<slug>.toml` from ADR 0013 collapses into `workspaces/<slug>.toml`. There is no longer a separate "session" identity distinct from "workspace" — one tmux session per workspace, one workspace per project, one row in Sessions mode per workspace.

### Protocol — `workspace_id` is additive

Every op that targets project state gains an optional `workspace_id` field:

```
tree.root        { mode, workspace_id?: string }
tree.children    { parent_id, workspace_id?: string }
preview.get      { node_id, workspace_id?: string }
kernel.request   { kernel_op, kernel_payload, workspace_id?: string }
repl.eval        { source, mode, workspace_id?: string }
repl.run_file    { path, fresh, workspace_id?: string }
concept.read     { target, workspace_id?: string }
concept.write    { target, content, expected_ast_hash?, workspace_id?: string }
pty.open         { cols, rows, target?, workspace_id?: string }
```

Missing `workspace_id` resolves to the **default workspace** for back-compat (the workspace whose `project_root` matches the daemon's `--project-root` flag — i.e., the historical single-backend behavior). All existing single-backend invocations continue to work unchanged.

New workspace-lifecycle ops:

```
workspace.list                                  → [{id, slug, label, project_root, kernel_running}]
workspace.create   { label, project_root }      → {workspace_id, slug}
workspace.destroy  { workspace_id }             → {}
```

`workspace.create` writes the toml, creates the tmux session for BL/LLM panes via existing `tmux.create_session`, returns the new id. The kernel does not start until first use.

`workspace.destroy` shuts down the kernel child, stops the watcher, calls `tmux.kill_session`, removes the toml.

There is no `workspace.switch` op — the frontend just tags subsequent ops with the new `workspace_id`. Switching is purely a frontend state change; the daemon is stateless about which workspace is "active."

### Switch flow

1. User hits `s`, picks workspace `mypackage`, hits Enter.
2. Frontend sets `state.active_workspace_id = "mypackage"`.
3. Frontend re-fires `tree.root { mode: "files", workspace_id: "mypackage" }`.
4. Daemon routes to `mypackage`'s file tree (project root, file watcher).
5. Subsequent `preview.get`, `kernel.request`, etc. carry the same id.
6. BL pane retargets to `sot-be-mypackage` via existing `pty.open { target }`.

Wall-clock cost: one round-trip + a redraw. Kernel state untouched on both sides of the switch.

### Cross-machine deployment

Unchanged. One daemon = one socket = one `ssh -L`. Workspace switching is in-band on the single connection.

### Sessions mode → Workspaces mode (cosmetic rename, deferred)

The hotkey `s` and the visible label "Sessions" stay for now. We can rename to "Workspaces" once the registry refactor lands and the new term is the right one in the UI. Rename is a one-character change; not load-bearing.

### Daemon death

If the daemon dies, all its kernel children die with it. This is the explicit trade-off versus tmux-hosted kernels:

- **Cost:** a daemon restart loses Julia state across all workspaces; users re-warm each kernel on next use.
- **Gain:** no kernel-discovery sweep, no socket health probes, no "is this kernel still alive" reconciliation. The daemon owns its children, period.
- **Mitigation:** daemons are stable; restarts are rare (upgrade, deliberate kill). If "kernel survives daemon restart" becomes a felt pain point, the `KernelHost` abstraction below lets us swap to a tmux-hosted implementation later without protocol changes.

### KernelHost abstraction

Kernel spawning sits behind a trait so the daemon-owned model can swap to a tmux-hosted model later if survive-daemon-restart becomes load-bearing:

```rust
trait KernelHost {
    fn spawn(&self, workspace: &WorkspaceState) -> Result<KernelHandle>;
    fn shutdown(&self, handle: KernelHandle) -> Result<()>;
    // health check, framed read/write, etc.
}

impl KernelHost for ChildHost { ... }   // phase 2 — direct child of daemon
impl KernelHost for TmuxHost  { ... }   // phase 3 (optional) — kernel in a tmux pane on its own socket
```

The protocol, the registry, the routing, the frontend Sessions/Workspaces UX do not care which `KernelHost` is in use.

## Consequences

- **One socket, one tunnel.** Cross-machine deployment is unchanged. Windows-via-WSL or Windows→remote-Linux both work with the existing single-port `ssh -L`.
- **Switching is instant.** No kernel restart, no transport reconnect, no Julia warmup. Frontend tags ops with a new id; daemon routes.
- **Kernels can have different `Project.toml` environments.** Each kernel is its own Julia process; module tables are isolated. `MyPackage@0.1` in workspace A and `MyPackage@0.2` in workspace B coexist.
- **`workspace_id` is additive.** Missing field = default workspace; existing single-backend deployments continue to work unchanged.
- **Daemon owns kernel lifecycle.** Daemon death takes kernels with it. Acceptable; swappable via `KernelHost` later if needed.
- **B1–B6 tmux plumbing is reused.** Sessions-mode rendering, `[+ create new]`, BL retargeting all stay. The only Sessions-mode change is that Enter sets `active_workspace_id` and re-fires `tree.root`, instead of merely retargeting BL.
- **ADR 0013's "session-per-daemon" is superseded** by this. Sessions remain one-per-workspace at the tmux level; daemons collapse to one.
- **`~/.config/devenv/sessions/<slug>.toml`** is renamed in concept to **`~/.config/devenv/workspaces/<slug>.toml`**. The `[backend]` identity section ADR 0013 introduced is now `[kernel]` and refers to the workspace's kernel child rather than a per-workspace daemon. Existing toml files written by `--label`-launched daemons can be migrated mechanically on first read.
- **Multi-frontend** remains a phase-3 question. Phase 2 supports one frontend with `active_workspace_id` as a single value. Two frontends each with their own `active_workspace_id` against the same daemon is the natural extension; the daemon already routes per-op so this is mostly a phase-3 frontend coordination question (writer takeover, etc.).
- **Native Windows without WSL is unsupported** — tmux remains required on the backend host for BL/LLM panes. WSL on Windows is treated as "connecting to a local Linux server."

## Implementation order

1. **Filter Sessions mode to `sot-be-*`** — cosmetic, unblocks the visible Sessions list.
2. **Workspace registry on daemon** — `HashMap<workspace_id, WorkspaceState>` + `~/.config/devenv/workspaces/*.toml` persistence + startup scan.
3. **Per-workspace kernel children** — `ChildHost::spawn` per workspace, lazy on first request. Kernel handles keyed by workspace_id.
4. **`workspace_id` threaded through protocol** — additive optional field on tree/preview/kernel/repl/concept/pty ops. Backend routes by id.
5. **Frontend `active_workspace_id`** — Sessions-mode Enter sets it, re-fires `tree.root`, retargets BL pane. Persisted to `state-<hostname>.toml` for resume.
6. **`workspace.create` / `workspace.destroy`** ops — replace the ad-hoc `tmux.create_session` flow in B4.

ADR 0013 stays for historical context; its session-shape and persistent-metadata sections still describe the tmux side of the world. This ADR overrides its "daemon per session" claim and folds the per-session toml into a workspace-level toml.
