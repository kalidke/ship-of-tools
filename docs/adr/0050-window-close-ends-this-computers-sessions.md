# ADR 0050: the window close ends this computer's sessions

**Status:** current — Accepted; daemon half lane A, window half lane B, launchers lane C.

2026-10 amendment (0.6.6): the grant order has no token step and a bad token no longer counts as `Foreign`; the daemon's lease check reads no token.

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
process the daemon starts, but a capsule supervisor and the update pipeline's children (known limit
(n)), is killed with everything it started that did not leave it (residual 7): each runs in its own process group
on Unix and its own job on Windows, its leader is reaped only after that kill, and their owners are given 3 s to
let go; the final record is written; the
waiting `fe.leaving{close}` is answered with the not-ended count, and if that is above
zero the daemon waits up to 5 s for `fe.notice_seen` before exiting 0. Rows that ended
are forgotten, their registration deleted and its directory synced before the final
record clears `closing`; rows not ended stay registered and running, and are counted. The product
never runs `pkill` or `tmux kill-server`.

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
   without cgroup v2 at `/sys/fs/cgroup`, or without `cgroup.kill` (Linux before 5.14).
   macOS has no such container at all. On Windows the leg's job permits no breakaway (ruling (g)); see residual 7.
6. Closed: a row's remembered scopes are the durable file `row-scopes` in its state dir,
   read by every end, a startup Cleanup included, so a daemon restart no longer loses them.
7. A daemon child's tree is killed with it, but a process can leave. Unix: a descendant that moves to another
   process group is outside it, by `setpgid` (a shell's job control does this) or by `setsid`; this covers Julia's
   `detach` (a `run(detach(cmd))` child has pgid = sid = its own pid), so Pluto's notebook workers, which Malt starts
   detached, and quarto's julia server, which quarto starts detached (measured with quarto 1.7.31). Under the systemd
   unit the daemon's cgroup ends them when the daemon exits; started without systemd, an idle worker exits when its
   server socket closes and a busy one when its cell ends. The daemon's ssh bridges set `ControlMaster=no`, `ControlPath=none`
   and `ControlPersist=no`, so none leaves a master behind. Windows: nothing started inside a daemon child's job or a
   row's job can leave it. Outside it are a process a broker starts (WMI, COM activation, the task scheduler, a
   service) and a program started through an app-execution alias, which the Store install of juliaup makes `julia`: a
   julia started that way ran, with what it started, outside the starting process's job (measured 2026-10-03; the
   mechanism is not documented). Code a row or a REPL runs can start one.

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
- (n) Every process `rust/updater` starts inside the daemon runs outside containment, with `kill_on_drop` only, which
  does not run at the daemon's exit: the release check (update.rs `check`) and staging and prepare (update.rs
  `stage_prepare_arm_inner`), whose children are curl or gh, tar, unzip or PowerShell, git, julia and npm. A
  shutdown or exit while one runs leaves it and what it started to end on their own; under the systemd unit its
  cgroup ends them. The three calls carry `clippy::disallowed_methods` allows naming this limit.
- (p) Only the requested shutdown fires the child signal. Every other exit leaves the contained trees to end on
  their own, for example the update restart (exit 75, update.rs `exit_for_update`), the shutdown's backstop (exit 1),
  an accept-loop failure (`server::run` returning an error) and a termination signal (SIGTERM, SIGINT), which the
  daemon does not handle. Under the systemd unit its cgroup ends them.
- Window: see the release notes.
