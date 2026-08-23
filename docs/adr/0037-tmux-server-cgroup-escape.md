# ADR 0037: tmux server cgroup escape — daemon restarts must not kill the fleet

**Status:** Accepted (2026-08-23). Built with this ADR.
**Date:** 2026-08-23

## Context

The daemon creates and supervises agent sessions on a tmux server (ADR 0013,
ADR 0023). tmux's process model is that the **first client to run after a
server death auto-spawns the server as its own child**. When the daemon runs
as a systemd user service — the recommended deployment (ADR 0030 §3) — and it
happens to be that first client (which after a full tmux death it almost
always is: the boot repair sweep re-creates every registered workspace
session), the auto-spawned tmux server lands **inside `sotd.service`'s
cgroup**.

systemd's default `KillMode=control-group` then makes every unit stop kill the
entire cgroup. Combined with `Restart=always`, the consequence class is:

- `systemctl --user restart sotd` (routine deploys, config changes) →
  **SIGKILL to the tmux server** → every session on it dies: all `sot-be-*`
  agent sessions, and — on a deployment that points `SOT_TMUX_SOCK` at the
  user's default socket — every *unrelated* session the user keeps there too.
- Any daemon **crash** does the same on the way through the restart cycle.

This was root-caused live on a shared dev host (2026-08-23) by a peer agent
session: journal evidence showed a daemon restart stopping the unit, the tmux
server dying with it, and the replacement server — spawned three seconds
later by the restarted daemon's own `new-session` — sitting in the new unit
cgroup (`/proc/<server>/cgroup` = `…/sotd.service`), primed for the next
massacre. It retroactively explains earlier "the whole agent fleet died when
the daemon bounced" incidents that had wrongly been hunted as daemon bugs:
the daemon's code was clean; systemd cgroup teardown was the killer. One
observed kill also caught a package manager mid-upgrade in a pane, corrupting
a toolchain install — the blast radius is not limited to lost sessions.

The trap is self-reinforcing: once a captive server exists, *every*
subsequent restart both massacres the fleet and re-creates the captive
server. And it is invisible from inside the sessions — they simply die.

## Decision

**Route the one server-spawning tmux invocation through a transient systemd
scope**, so a server the daemon causes to exist is a *sibling* of the
service, not a member of its cgroup:

```
systemd-run --user --scope --collect --quiet -- tmux -S <socket> new-session …
```

The scope's lifetime is exactly the lifetime of the processes in it — i.e.
the tmux server — and a unit stop of `sotd.service` does not touch it.

Mechanics (all in `rust/backend/src/tmux.rs` + `rust/backend/src/pty.rs`):

1. **Only `new-session` can spawn the server.** Every other invocation the
   daemon makes (`list-sessions`, `has-session`, `send-keys`, `kill-session`,
   …) errors out against a dead server rather than starting one, so momentary
   clients stay unwrapped — their cgroup membership doesn't matter.
2. **`TmuxClient::create_session`** checks, per call: is a server accepting
   connections on the private socket (a `UnixStream::connect` liveness probe —
   a stale socket file refuses), and is scoped spawning available? If the
   server is absent and scoping is available, the `new-session` runs under
   `systemd-run`; otherwise the plain path is unchanged. A scoped attempt
   that fails **falls back open** to a plain spawn with a warning naming the
   hazard it re-opens — an in-cgroup server beats no session at all.
3. **The pty attach path** (`spawn_tmux_pair`, `new-session -A` for the
   LLM/boot panes) cannot be wrapped — its client must live on the pty. When
   the server is absent it instead **pre-creates the session detached through
   `create_session`** (same name/cwd/awareness-env, no command — identical to
   what `-A` would create), so its own `-A` merely attaches. Best-effort:
   on pre-create failure the attach still creates the session, with the old
   hazard, logged.
4. **Scoping is gated, probed once per process** (`scoped_spawn_available`):
   Linux, **and** running under a systemd unit (`INVOCATION_ID` present — a
   terminal-launched dev daemon's children are not at unit-teardown risk),
   **and** a 3s-bounded `systemd-run --user --scope -- true` probe succeeds
   (user manager + bus reachable). Anything else → plain spawns, with a
   warning when the probe fails *under* systemd.

Rejected alternatives:

- **`KillMode=process` / `KillMode=mixed` on the unit.** Surgical-looking but
  wrong: the daemon's *other* children (Julia kernels, REPL processes, boot
  ptys) rely on cgroup teardown for cleanup; loosening KillMode leaks them on
  every restart to protect one process. The unit template now carries a
  comment warning against this "fix".
- **A keeper session / `exit-empty off` server pre-start.** Starting a
  scoped, session-less server needs `set -g exit-empty off`, which mutates
  global server behavior — unacceptable on shared-socket deployments where
  the server also serves the user's own sessions.
- **Documentation only.** The failure is a full fleet kill with data-loss
  potential (the toolchain corruption above), triggered by a routine
  `systemctl restart`. A one-invocation wrapper is cheap; docs alone are not
  a fix.

## Consequences

- A tmux server spawned by the daemon survives daemon restarts and crashes.
  Workspace sessions, agent processes, and anything else on the socket live
  through `systemctl --user restart sotd`.
- **The fix protects new servers only.** A server that is *already* captive
  in the service cgroup stays captive until it dies — the first restart after
  deploying this change is still a massacre, one last time. Deployments
  should plan that restart deliberately (or migrate the live server's cgroup
  by hand via a transient scope, for operators comfortable with `busctl`).
- Non-systemd platforms and terminal-launched dev daemons are untouched:
  every gate in `scoped_spawn_available` fails closed to the previous
  behavior.
- The `--collect` transient scopes are fire-and-forget; failed ones don't
  accumulate in `systemctl --user` output.
- The wrapped spawn adds one `systemd-run` round-trip (~tens of ms) to the
  *first* session creation after a server death — a once-per-server-lifetime
  cost.
