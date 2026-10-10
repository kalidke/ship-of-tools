# ADR 0050: the window close ends this computer's sessions

**Status:** current — Accepted; daemon half lane A, window half lane B, launchers lane C.

2026-10 amendment (0.6.6): the grant order has no token step and a bad token no longer counts as `Foreign`; the daemon's lease check reads no token.

2026-10 amendment (0.6.6, ADR 0049 `## User isolation`): the lease connection says hello first, with role `handoff`, and its next frame is `fe.lease`; wire protocol 3. A refused hello is one reply and a close.

**Date:** 2026-10-02

## Decision

Closing the last window on a computer ends the sessions of the backend that runs on that
same computer, unless the user chose to keep them. A remote backend is unaffected: its
sessions outlive any window, as ADR 0010 and ADR 0043 describe. The daemon decides; the
window and the launchers only tell it what happened.

### Rulings
- (a) The lease is the only proof a window is open, and its end (EOF) the only proof it closed.
- (b) Never silent: a session that could not be ended is always shown.
- (c) Ctrl+Q Yes is the only open-ended keep; a relaunch is a bounded handover.
- (d) Leases are fenced by generation.
- (e) A start never resurrects what a close ended.
- (f) The Windows stop is never cut short.
- (g) On Windows no job the product makes permits breakaway, so everything started inside a row or a
  daemon child ends with it, a daemon or window launched from inside a row included; only a process a broker starts,
  or one started through an app-execution alias, runs outside it.

When intents arrive in order on one lease, the latest wins.

### The lease (proof and grant)

- A window opens a dedicated connection to the local daemon and sends `fe.lease` as its
  first frame, carrying its boot identity, process id and process creation time. The
  daemon peeks that frame beside `lane.connect`; the connection never enters the
  hello-gated loop. `PROTOCOL_VERSION` stays 2: the lease is an additive op, and an older
  daemon answers with an error payload, which the window reads as unsupported.
- The daemon reads the connecting process from the operating system at accept, and grants
  only when every check holds, in this order: no shutdown has begun (otherwise `Closing`);
  the peer and the daemon's own boot identity are determined (otherwise `Undetermined`);
  the token passes, the boot identity equals the daemon's, and the peer's pid and creation
  time equal the claim (otherwise `Foreign`). Any failure to read the operating system is
  `Undetermined`. A bad token counts as `Foreign`.
- Boot identity is in the claim because a relayed window's numeric process identity can
  equal a local forwarder's, and those numbers are only meaningful within one boot. Linux:
  `/proc/sys/kernel/random/boot_id`. macOS: `kern.bootsessionuuid`, unverified on a Mac
  until one runs it. Windows: the registry `BootId` under
  `Session Manager\Memory Management\PrefetchParameters`, decimal, an empty string when
  unreadable (an empty boot compares as unknown, never as a mismatch).
- Only Linux checks the uid, because it comes free with the pid; the macOS socket
  directory is 0700 and the Windows pipe is owner-only.
- A grant whose record write fails is refused as `Undetermined`, never reported granted.

### Departure

- Every granted lease gets a generation, counted per daemon process; an entry leaves by
  generation only, so a late end of a replaced lease is a no-op.
- A lease departs at the first of: a well-formed `fe.leaving` (its intent decides); EOF,
  including a partial line then EOF; an io error; a reply the window has not drained
  within the 5 s write bound. Nothing else ends it. A complete line that is not a
  well-formed request gets an error payload and the lease continues (ruling a). Short of
  that write bound, the daemon never closes a lease itself.
- An end without `fe.leaving` is a `Close`. The intents are `Close`, `Keep` and `Handover`.
  If the departing lease is not the last, nothing else happens. If it is the last:
  `Close` is a shutdown; `Keep` does nothing and is the only open-ended keep; `Handover`
  waits up to `HANDOVER_BOUND` (60 seconds, `lease::HANDOVER_BOUND` in the protocol
  crate), persisted.
- After a `Keep` or a `Handover`, the daemon keeps reading that connection until EOF.
  Every later well-formed `fe.leaving` on it replaces the earlier intent and is applied in
  order as that window's departure at that moment (latest intent wins): an X after a Keep
  or a Handover never leaves sessions running, a Keep after a Handover is open-ended, and a
  Handover after a Keep is bounded. EOF after an intent leaves the last one standing.
- Any granted lease clears a handover, and a pending start the ticker has not yet acted
  on, even past its deadline; a handover that expires with no lease held is a shutdown.
- `fe.leaving{close}` from the last holder is answered after the shutdown, with the count
  of rows not ended, including any no window has yet acknowledged; every other
  `fe.leaving` is answered at once with zero.
  `fe.notice_seen{n}` clears the recorded count iff it equals `n`.
- Every deadline is wall-clock unix milliseconds, so the handover deadline survives a
  restart; one 1 s ticker checks expiry.

### Shutdown

In this order: a backstop thread sleeps `SHUTDOWN_BOUND` (120 s) and exits 1 if the
process is still alive, leaving the next start to finish the job; the record is marked
closing and new leases are refused, the listener dropped and the socket unlinked, before
any row is touched; the run gate closes and in-flight starts drain, until the rows
deadline (`SHUTDOWN_BOUND` minus the 10 s `SHUTDOWN_TAIL`); every capsule row and the
drawer end without resuming anything, retrying a kept row once a second to that same
deadline, and a row of any other runtime is left running and counted not ended; every
process the daemon starts, but a capsule supervisor, receives a checked termination attempt for its contained tree;
descendants that leave it remain residual 7. Each runs in its own process group on Unix and its own job on Windows.
Unix termination requests precede the owner's direct-child reap. Windows async wait obtains the direct-child status
before requesting job termination; Windows blocking and nonblocking exit observation uses wait or try_wait before the
subsequent job request, with the job handle retaining containment identity. Explicit kill requests termination before
waiting on either platform. On macOS only, a real group-request EPERM counts as no live member only after checked
observation confirms that the retained leader has exited without being reaped and a complete libproc process-group
membership and status query finds no live member; live, failed or ambiguous observations preserve the original error,
and leader requests and injected failures remain independently checked. No Unix signal or process-group identity query
occurs after leader reap. Creation through adoption and registration shares the registry mutex with the permanent child
signal. Fire attempts every registered tree and reports errors, without a child-count grace period or waiting for
confirmed death; contained children can still outlive daemon exit. An OS creation or adoption that never returns can
delay fire and process exit; the final record is written; the
waiting `fe.leaving{close}` is answered with the not-ended count, and if that is above
zero the daemon waits up to 5 s for `fe.notice_seen` before exiting 0. Rows that ended
are forgotten, their registration deleted and its directory synced before the final
record clears `closing`; rows not ended stay registered and running, and are counted. The product
never runs `pkill` or `tmux kill-server`.

`gio trash` uses the contained blocking wait with a 5 s wait budget; a successful timeout cleanup means termination requests
succeeded and the direct child was reaped before the recoverable workspace-trash fallback. Request and reap failures are
diagnosed and take that fallback without claiming cleanup succeeded; descendant death before the fallback is not promised.
The budget imposes no wall-clock ceiling on an OS termination or reap.

Exit codes: 0 is a requested shutdown and stays down; 75 is an update restart and starts
again, taken only while no shutdown is under way, so a shutdown's own exit always stands;
1 is a failure (lock timeout or refuse-live). The bounds chain is
`HANDOVER_BOUND (60) < SHUTDOWN_BOUND (120) < DAEMON_LOCK_WAIT (150) < LAUNCH_WAIT (160)`:
a successor waits longer than any shutdown lasts, and a launcher waits longer than a
successor waits for its lock.

### The record, `held.json`

A small file in the daemon's state root, written by tmp file, fsync and rename under the
lease mutex: the writing daemon's boot, the live holders, the handover
deadline, `closing`, the `not_ended` count and the `forget` list (ids that ended but
whose registration file could not be removed). It exists iff some field is non-empty or
true, so one window's `Keep` or `Close` never deletes another's holder entry. On Unix the
parent directory is synced after the rename and after the delete. On Windows the replace
is write-through (`MoveFileExW` with `MOVEFILE_WRITE_THROUGH`); only the delete has no
write-through flag, which is the one Windows record residual, beside Fast Startup. Only
the daemon and the Windows stop script (which reads `closing`) read it.

A row's remembered process scopes are the durable file `row-scopes` in its state dir,
read by every end, a startup Cleanup included.

### Start

1. **Lock first**, before the registry scan: the daemon takes `daemon.lock` in its state
   root (a kernel lock released on any exit, a kill included), retrying every 250 ms. If
   the lock is busy and the session socket answers, the new daemon refuses at once with
   exit 1. If the socket does not answer, it logs once that it is waiting for the previous
   daemon to finish shutting down, and at `DAEMON_LOCK_WAIT` it exits 1 naming the lock
   path. With no state root it logs a warning and runs unfenced, with no record.
2. After the scan and the default-row seed, it reads the record, applies `forget`, and
   plans: no record resumes; an unreadable record, an unknown version, `closing`, a boot
   that differs from the daemon's own (both non-empty), or a passed handover
   deadline plans Cleanup; a handover in the future, or recorded holders, plans a pending
   window; only `not_ended` or `forget` resumes. The plan is a pure function of the
   record, the clock and the boot identity; it takes no process-liveness input.
3. **Cleanup** ends the registered rows without resuming them (deadline `SHUTDOWN_BOUND`)
   and writes the record. The daemon stays up; a start never exits on a recorded close, and zero sessions is a valid
   start.
4. **A8 is lite.** When holders are recorded, the daemon resumes at once and arms the
   persisted handover deadline (`HANDOVER_BOUND`, 60 s). No lease by that deadline means a
   shutdown. There is no held-back state.
5. **The cheaper form of "end".** While a startup Cleanup runs, no grant rewrites a record
   that says closing or names another boot. A kill during Cleanup therefore re-runs
   Cleanup, which fails toward close. This replaces an explicit `end` field in the record
   that an earlier draft carried; no such field exists.
   Every startup Cleanup, whatever its cause, keeps the record moving only toward close until `finish_cleanup`.
6. A not-ended count above zero is sent with every grant until one window acknowledges it.

### What the window and the launchers do

- The window never leases from a harness run (`--ephemeral`, `--capture`) or over an SSH
  endpoint. Before each data connection to a local endpoint with no live lease, it leases;
  `Granted` makes a holder task own the stream for the process's life; `Foreign`,
  `Undetermined` or an unsupported reply proceeds without it; `Closing`, an io error, or a request not sent within `LEASE_REPLY_WAIT` is a
  failed connect. Once the request is sent the window waits for the reply as long as the connection lasts, since
  dropping it would read as this window closing (ruling a). A not-ended count is shown once per daemon, keyed by
  the state root its grant names. With no granted lease it shows a persistent notice naming
  the cause.
- X, an OS close or Ctrl+Q then No sends `Close` and waits for the ack, up to 125 s; an X
  during a Keep's ack wait sends `Close` after the keep. Ctrl+Q then Yes sends `Keep`.
  A self-relaunch (75 or 76) sends `Handover`. A not-ended count above zero is shown as
  "N sessions could not be ended and are still running" and acknowledged with
  `fe.notice_seen`.
- Launchers start the daemon and wait for its socket up to `LAUNCH_WAIT`, never killing a
  daemon that is alive and has not bound yet. The stop script waits up to
  `DAEMON_LOCK_WAIT` for a closing daemon to exit by itself. On Windows, the supervisor holds
  a lease of its own from every relaunch (exit 75 or 76) until the next window is spawned,
  then sends a `Handover`; a converge that finds the launcher code on disk changed re-invokes
  it in the same process, which keeps those leases and hands them over (ADR 0017).

## What was deleted

An earlier draft's host and harness rules, a "peer gone" departure reason, carried leases
and process watches are not built. The lease connection is the only proof, and the
connection is the only handle.

## Residuals

1. Linux and macOS: none from reboots; a record from another boot plans Cleanup at once.
2. Windows, UNVERIFIED: whether the registry BootId changes across a Fast Startup (hybrid)
   shutdown. If it does not, a relaunch handover inside `HANDOVER_BOUND` across a Fast
   Startup cycle lets the first new window resume the old sessions. Recorded holders are
   safe either way (a new process's `(pid, created)` cannot match; they only wait up to
   `HANDOVER_BOUND`). Where BootId is unreadable, the same applies to any reboot.
3. A row counted not ended whose end completes after exit is re-adopted, or started again,
   at the next start. It is listed and counted.
4. An older window never leases, so its close ends nothing.
5. A row ends only the agent's process group, and a descendant that left it (for example by
   `setsid`) survives, in each of these cases: its supervisor died before its end; it was
   started before 0.6.6; it runs on a host without a reachable user systemd manager,
   without a cgroup v2 hierarchy (at `/sys/fs/cgroup`, or `/sys/fs/cgroup/unified` on a hybrid
   host), or without `cgroup.kill` (Linux before 5.14).
   macOS has no such container at all. On Windows the leg's job permits no breakaway (ruling (g)); see residual 7.
6. Closed: a row's remembered scopes are the durable file `row-scopes` in its state dir,
   read by every end, a startup Cleanup included, so a daemon restart no longer loses them.
7. A daemon child's tree is killed with it, and no process can leave it, except as stated here. Linux: every serving
   daemon is the child of a guard that is a subreaper (the 0.6.6 update below), so a process the daemon starts stays a
   descendant of the guard through `setpgid`, `setsid`, Julia's `detach` (Pluto's notebook workers, which Malt starts
   detached, and quarto's julia server, which quarto starts detached, measured with quarto 1.7.31) or a double fork,
   and ends within `DRAIN_BOUND` (10 s) of the daemon's end, however that end came, a SIGKILL included; while the
   daemon runs, such a process outlives the end of the child whose group it left (Pluto's notebook worker when Pluto's
   server ends) unless it exits on its own. A crashed Pluto server can leave one Julia worker per open notebook,
   which ends when the daemon ends (a stated 0.6.6 limit; per-child containment is designed for 0.6.7). Outside it:
   a process a broker starts; a SIGKILL of the guard itself, after which the daemon ends at once but what it started
   does not (under the systemd unit the unit's cgroup ends the rest within the unit's stop timeout); and a process in an
   uninterruptible kernel call, which has SIGKILL pending and ends when the call returns (the guard logs it). Every ssh
   the daemon starts (its two bridges and the monitor's sampler) sets `ControlMaster=no`, `ControlPath=none` and
   `ControlPersist=no`, so none leaves a master behind. macOS has
   no guard, no subreaper and no cgroup: a controlled end of the daemon (Close, the update restart, the backstop, a
   handled signal, a returned error) kills each child's process group, and not a process that left it (Pluto's notebook
   worker, quarto's engine server, a `detach`ed process); after SIGKILL, abort or a crash nothing ends the children;
   each ends on its own, an idle Julia child when its input closes, a busy one when its work ends, quarto's
   engine server after 300 s idle. Windows: nothing started inside a daemon child's job or a
   row's job can leave it. Outside it are a process a broker starts (WMI, COM activation, the task scheduler, a
   service) and a program started through an app-execution alias, which the Store install of juliaup makes `julia`: a
   julia started that way ran, with what it started, outside the starting process's job (measured 2026-10-03; the
   mechanism is not documented). Every julia the daemon runs, the update prepare's included, comes from `resolve_bin`
   (`rust/backend/src/sidecars/julia.rs`). On Windows it inspects the selected existing executable's reparse tag through ordinary filesystem links and refuses `IO_REPARSE_TAG_APPEXECLINK` or an inspection failure; alternate path spelling does not bypass this check. This is not a file-substitution-after-check proof or a general broker-escape prevention rule. Code a row or a REPL runs can still start a broker or an alias.

## Known limits (0.6.6)

- (c) The create gate keeps the id, and the rollback of a failed create can remove a
  pre-existing row. This is a limit only because the start has no held-back state.
- (e) The macOS check has not run on every commit; boot identity on a Mac is unverified.
- (f) Launchers: after the fix round the ensure logs under the install prefix; the Windows log cap (keep 5 per
  stream, 16 MB unheld total) applies across starts, and one running daemon's own log
  file grows within its run. `install.sh` writes the icons, the `.desktop` file and the
  `settings.toml` `[trust]` block straight onto their installed paths (the last by
  appending), the launch wrapper's mode depends on the umask, and one protected log (the
  newest, or one found held) can exceed the cap on Windows and Unix alike.
- (g) The wake: a text-write failure is covered only for its mapping and format, since there
  is no seam for a real one; the attach-failure path has no test; the wake function's doc
  does not say it errors only before the first write; one reason string has a catch-all
  arm; the refusal-reason docs omit "text not confirmed" and two older reasons; there is
  no test for a wake line plus extra text, or for a partial wake line; on a narrow
  terminal the wake line wraps, and the refusal keeps its general reason but loses the
  name; an unconfirmed text step does not stop the next 2 s tick from retrying, and a
  persistent write failure warns on every attempt.
- (h) A failed closing-record write: the shutdown still ends the rows, and a kill during it
  may resume them.
- (k) A startup Cleanup's count reaches a window granted before the Cleanup finished only
  at the next window; it stays in the record until acknowledged.
- (n) Closed (0.6.6): the updater takes its spawner from its caller (`sot_updater::Spawner`). The daemon's
  `UpdaterSpawner` runs every discovery, stage, prepare and prepared-state command through the child signal, so what the
  update pipeline starts is contained like any other daemon child; the window's `WindowSpawner` keeps its explicit
  kill-on-drop policy, which does not contain descendants.
- (p) Closed (0.6.6): every controlled end of the serving daemon fires the child signal (see "controlled exits"
  below). What no code can do stays a limit: an uncatchable signal, an abort or an OS kill runs no daemon code at all;
  on Linux the guard ends the daemon's children then, on Windows the jobs do, and on macOS nothing does.
- Window: see the release notes.

## Update (0.6.6): controlled exits

Every controlled end of the serving daemon takes one terminal, `lifecycle::shutdown::exit`: it fires the child signal on a
thread of its own, which asks each contained tree to end and reports each failed request; it waits at most two seconds for
that answer (a child creation stalled in the OS holds the mutex the fire needs, and no exit, the backstop's included,
waits on it longer; when no thread can be started for the fire, the exit goes without it); and it makes the daemon's one
raw process exit. A request is not an observed death: on Linux the guard ends what is left, and on macOS these
controlled ends are the only ones that end the daemon's children, and they reach only each child's process group
(residual 7).

The main future's result becomes a status while the runtime still exists: Ok is 0, an error is printed and is 1, a panic of
the future is 101. A finished close exits 0; the shutdown's backstop exits 1; the update restart exits 75, and only while no
shutdown has begun: the update is committed under the lease lock (`Leases::commit_update`, which moves the lease to
`Updating`, so no close begins afterwards and none is granted) and the exit, with its wait for the fire, comes after the
lock is released; a close that began first keeps its own exit. INT and TERM are caught on a thread of their own with a
runtime of its own, unblocked whatever mask the daemon inherited and checked to be deliverable, take the same fire and
then end the daemon by the signal itself with its default action, however stalled its main runtime is: a shell reads 130
and 143, and a service manager counts the end as a clean stop, so `systemctl stop` leaves the unit inactive rather than
failed and an outside TERM is not restarted under `Restart=on-failure` (as before 0.6.6, when no handler was installed); a
failed installation refuses the boot. A bad `agent-exec` recipe is 2, a
missing `sot-capsule` or a state dir that is not private is 1 and no derivable config directory is 78. The guard exits as
the daemon did, so a launcher sees these codes. Capsules are outside all of this by design (an update restart or a window
Keep leaves them running).

## Update (0.6.6): tested scope

The daemon's lifetime is read on real daemons, real `sot-capsule` supervisors and real children
(`rust/backend/tests/daemon_lifetime`), never from source text. On Linux: the guard exits as the daemon did (a close 0,
the backstop 1, SIGKILL, SIGABRT) and forwards TERM, INT and HUP, also to the group; a lost guard ends the daemon within a
second; the drain outlasts a forking child and ends only its own subtree; a killed daemon's REPL, Pluto (server, worker
and tree) and Quarto (engine server, worker and tree) all end while the capsule stays and a successor adopts it; Pluto's
notebook worker, outside Pluto's process group and spinning so it cannot end by itself, has ended by the time a closed
daemon's guard exits; the main
future's Ok, error and panic are 0, 1 and 101; INT and TERM end a daemon whose runtime is stalled and whose inherited mask
blocks them, by the signal, and a test-owned service unit's stop ends inactive, not failed, with no restart; a close that
outlasts its bound exits 1; `update.apply` against a pointer armed with the real
updater, and the automatic update through the real stage, prepare and arm, exit 75, end the REPL's tree and leave the
capsule, and the automatic one waits while a window is attached; a close and an update in either order keep the first one's
exit; an update the daemon may not take leaves it serving; the updater's discovery and prepare commands end with the
daemon. On macOS the hosted jobs compile the native launcher with and without its phase barriers, run its premises and
the fence claim's on real children and run the crate's own lifecycle tests (the group recognition), and the window's
pane-timing job starts plain-shell capsule rows on a test daemon, attaches to them and closes it, the daemon reporting
every row ended and exiting 0. No agent session has run end to end on a Mac; the only rows a test starts there are the
pane-timing job's plain shells; the successor and controlled-outcome cases are not built for macOS. Nothing more of the
daemon's lifetime is tested on a Mac.
Windows runs the crate's lifecycle tests: the per-child kill-on-close jobs, including that a killed daemon's
contained tree ends with it, and the suspended interval above observed as the limit; that a capsule outlives a killed
daemon there is the capsule suite's adoption case, through the product's own capsule spawn.

Not tested, and stated as limits: macOS after an abrupt end of the daemon (by decision, above); on macOS, an agent
session end to end, and the successor and controlled-outcome cases, which are not built there (they drive the Linux
guard's launched process and read `/proc`); Windows console events
(CTRL_C, CTRL_BREAK, CTRL_CLOSE), logoff and shutdown; the interval on Windows between a child's creation and its
assignment to its job, in which a daemon death leaves one process that never ran (Windows abrupt-death coverage is the
per-child jobs the daemon holds, which no daemon death outlives; there is no aggregate job, because it would end nothing
the per-child jobs do not and would leave this same interval; std has no stable way to name a job at creation, which
`CREATE_SUSPENDED` and a later assignment work around, and a `CreateProcessW` spawn with `PROC_THREAD_ATTRIBUTE_JOB_LIST`,
as the pseudoconsole spawn already does, would close it); a binary built with `panic=abort`, a
stack overflow, an allocation failure and the OS's out-of-memory kill; power loss.

## Update (0.6.6): hub relay locality

A generated hub relay socket is physically local transport to a remote daemon. The window carries that distinction from endpoint parsing to control, page and lane consumers and never acquires a local-window lease through it. Relay unit target/path overrides and local daemon leases retain their existing behavior.

## Update (0.6.6): Foreign notice

Foreign reports a refused boot, pid or creation-time claim and does not by itself prove another OS account. Without a granted or pending lease, the window names the known cause: unknown verification, then unsupported backend, then a refused identity claim, then unreached or absent backend.

## Update (0.6.6): final window teardown

The Ctrl+Q prompt reads Tab and Enter by key identity; other non-repeat keys cancel and repeats do nothing. Final window teardown starts after the leave acknowledgement and any required notice presentation, or at an explicit second-close decision. Queued writes share a one-second OS-monotonic deadline, returning event loops use a one-second runtime shutdown timeout, and one independent three-second std-thread backstop bounds final process teardown. These intervals do not shorten the Close acknowledgement wait. A timed-out blocking task can continue until process termination; tests separately observe cleanup of a yielding task's owned child. The existing nonzero handover exit remains immediate.

The macOS default menu remains enabled. Earlier T1 review recorded native Cmd+Q bypassing the Ctrl+Q prompt; this lane does not re-test or change that native menu route.

## Update (0.6.6): the Linux lifetime guard

A process the daemon starts is contained by ancestry on Linux. `main`'s serving prologue starts the durable parent (a
capsule's birth parent, outside the daemon's tree), then `lifecycle::daemon_children::guard::install` forks: the
launched process becomes the guard, a subreaper that blocks every catchable signal and reads them from a signalfd, and
the daemon is its child with `PR_SET_PDEATHSIG` set to SIGKILL. The guard forwards every signal but SIGCHLD to the
daemon and reaps. When the daemon is reaped it kills and reaps its own children until `waitpid` answers ECHILD, within
`DRAIN_BOUND` (10 s), then exits as the daemon did: the daemon's code, or its signal with the default disposition and no
core. A process the daemon started that moved to a group or session of its own is still a descendant, and an orphan goes
to its nearest living subreaper ancestor, so it is a child of the guard when its parent ends. An unreaped child's pid is
never reused, so the drain's kill cannot reach another process. The durable parent is born before the guard and never
descends from it, so a capsule's supervisor is never drained: capsules stay outside the daemon's lifetime.

On Linux, then, a process the daemon starts ends with the daemon, and a process a row's agent starts belongs to the row
and survives (a capsule is outside the daemon's lifetime by design). Known limits: the guard's own loss ends the daemon at once and leaves its processes to the unit's
cgroup or to end on their own; a brokered start and an uninterruptible kernel call are outside it (residual 7).

macOS after an abrupt daemon end (SIGKILL, abort, a crash): nothing in 0.6.6 ends the daemon's children. macOS has no
subreaper and no cgroup, a process group is left by `setsid` and Pluto's and Quarto's workers leave it, and macOS installs
as experimental without service-manager wiring. This is a limit of an experimental platform, decided by the maintainer;
every controlled end still kills each child's process group, and not a process that left it (residual 7).
