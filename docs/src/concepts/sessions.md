# Sessions and persistence

A **session** is a row in Sessions mode. The rows from every host you are
connected to appear in one list, grouped by host; the machine in front of you
is just another host.

## [Hosts, sessions, REPLs](@id session-model)

- A **host** is a machine running the backend daemon, `sotd`.
- A **session** is one project directory on one host. The backend calls it a
  *workspace*, and the words mean the same thing: one session is one row in
  Sessions mode and one entry in the session strip along the bottom edge.
  `Shift+Arrow` moves between sessions.
- Each session has its own **agent pane** (Claude Code, Codex or a plain
  shell), its own **Julia REPL**, and its own **kernel**, the Julia process
  that serves Modules mode and file previews. The REPL and kernel start the
  first time something needs them, in the project's `Project.toml`
  environment, running the `julia` that `SOT_JULIA_BIN` names, else juliaup's
  default channel, else a `julia` on `PATH`.

```@raw html
<div class="sot-model" role="img" aria-label="One host running two sessions. Each session has its own agent pane, Julia REPL and kernel.">
  <div class="sot-model-host">
    <div class="sot-model-title">host: <code>sotd</code></div>
    <div class="sot-model-rows">
      <div class="sot-model-row">
        <div class="sot-model-title">session <code>survey</code></div>
        <span>agent pane</span><span>Julia REPL (<code>Main</code>)</span><span>kernel</span>
      </div>
      <div class="sot-model-row">
        <div class="sot-model-title">session <code>analysis</code></div>
        <span>agent pane</span><span>Julia REPL (<code>Main</code>)</span><span>kernel</span>
      </div>
    </div>
  </div>
</div>
```

Two sessions never share a Julia `Main`: each REPL is its own process. Inside
one session, you (from the REPL drawer), the session's agent and any helper
agents it starts all run code in that session's one REPL. It runs one
evaluation at a time and does not queue: a request that arrives while another
is running comes back as `busy` and nothing runs. The `julia-repl` skill tells
agents to use separate `julia` processes for parallel runs.

## Capsules: sessions that outlive everything around them

Each session's agent runs in a **capsule**: a small supervisor process
(`sot-capsule`) that owns the agent's terminal and records the session to disk
(its *voyage*). The frontend attaches to the capsule to draw the pane; the
daemon starts capsules and lists them, but does not hold them up.

| What goes away | What happens to the session |
|----------------|-----------------------------|
| the network, or the laptop lid | nothing — the frontend reconnects on its own when the network is back; `F5` retries at once |
| the frontend (quit, crash, relaunch) | nothing — the next frontend reattaches |
| the daemon (restart, upgrade) | the capsule keeps running and the restarted daemon re-adopts it |

On Linux each capsule supervisor runs in its own transient `systemd --user`
scope, outside the daemon's unit, which is what lets it survive a daemon
restart. On a host with no reachable user systemd manager it runs in a
degraded mode that shares the daemon's lifetime, and the daemon log says why.

## Where the state lives

The daemon's state root is `${XDG_STATE_HOME:-~/.local/state}/sot`: the
capsule records and the daemon's own `sotd.log`. It must be a **local, durable
filesystem** — the daemon refuses to start a capsule on a network or volatile
filesystem rather than risk the record. On a home directory shared across
servers, give each host its own local state root (see
[Troubleshooting](../guide/troubleshooting.md#A-session-will-not-start-on-a-shared-home)).

## Switching and restarts

Switching the active session is a frontend state change: it never restarts a
REPL or kernel, so each session's Julia state is where you left it, and two
sessions can run different versions of the same package side by side.

The kernels and REPLs are children of the daemon, so a daemon restart
restarts them. The agents survive it; Julia state does not.

## Reconnecting

Every connect sends the client's session id and the last revision it saw; the
daemon replays the missed events from a bounded ring, or sends a fresh
snapshot if the client is too far behind. That is why a reconnect puts you
back exactly where you were. Protocol details are in
[Design: backend and sessions](../design/backend.md).
