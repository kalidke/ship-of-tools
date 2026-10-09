# rust/backend/src/lifecycle/daemon_children: what ends with a daemon (lifecycle)

The one owner of "every process the daemon starts, at any depth, ends with it", except what is outside by design: a
row's capsule (ADR 0050 and the rows charter). On Linux that is the lifetime guard. Windows has no aggregate job: each
contained child's own kill-on-close job, held by the daemon (the lifecycle charter's containment), ends its tree when the
daemon dies, and the interval between a child's creation and its job assignment is ADR 0050's stated limit. macOS has
nothing after an abrupt end (ADR 0050). Part of lifecycle; charter: rust/backend/src/lifecycle/CLAUDE.md.

## Files
- `mod.rs`: declares the folder's modules (the guard is Linux only).
- `guard.rs`: the Linux lifetime guard: `install`, the guard's loop, `drain`, the mirrored exit and `guard_pid`.

## Start here
`guard.rs` `install`, then `keep_guard` for the guard's life and `drain` for what it does when the daemon is gone.

## Rules
- The guard is a process, never a thread: `install` runs in the serving prologue after the durable parent's start and
  before the runtime. `main` asks `require_one_thread` before the durable parent's fork and refuses the boot unless the
  process has exactly one thread, so both forks run in a single-threaded process. The durable parent is therefore never a
  descendant of the guard, and a capsule supervisor it starts is never drained.
- The guard is the launched process and the daemon is its child: the guard is a subreaper with all catchable signals
  blocked, forwards all but SIGCHLD to the daemon through a signalfd (made in `install` before the fork, so its failure
  refuses the boot), stops itself after forwarding TSTP, TTIN or TTOU (a shell then sees the job stopped), reaps, and
  after the daemon is reaped kills and reaps its own children until `waitpid` says ECHILD, within `DRAIN_BOUND`. It exits
  as the daemon did, so the launcher, systemd and a test see the daemon's status at the launched pid.
- The drain lists only the guard's own children (`PPid:` is the guard) and only the guard reaps them, so a listed pid is
  still the child it was (an unreaped child's pid is never reused).
- The daemon asks for `PR_SET_PDEATHSIG` SIGKILL: the guard's loss ends the daemon at once. What the daemon started then
  outlives it (under the unit its cgroup ends the rest).
- A start by a broker, a SIGKILL of the guard and an uninterruptible kernel call are outside the guard (ADR 0050, residual
  7); after `DRAIN_BOUND` the guard logs each pid still listed and exits.
- `guard_pid` is the guard's one interface to the daemon (`supervised_by_systemd` compares it with the unit's MainPID).
- macOS has no guard: only a controlled end kills the children (ADR 0050, residual 7).
