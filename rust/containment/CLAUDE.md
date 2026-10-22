# rust/containment: containment (contract)

Every child the daemon starts lives in a kill domain its owner ends, and every wait on one is bounded. The crate
(`sot-containment`) depends on `sot-platform` and nothing else of ours; `sotd` (the backend binary) depends on it and
nothing re-exports it. It holds one piece of run-time state, the registry of contained trees (inside `Signal`, one per
process: `process()`), and it decides nothing about when to end anything: lifecycle fires the signal, and each owner
releases its own child.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on and the limit it has. The Tests section holds the test
of each.

1. **A contained child's tree is asked to end when its owner lets go or the signal fires.** A child started through
   `Signal::spawn`, `Signal::spawn_std` or `Signal::output` leads a kill domain of its own: a process group (Unix) or a
   job (Windows, no breakaway, with a console process group of its own). Its owner's `kill`, `wait` or drop of the
   `Contained` or `ContainedStd`, and `Signal::fire`, each request the end of everything still in the domain. On Unix
   the leader is observed unreaped, the requests are made, and only then is it reaped; on Windows `wait` reads the
   child's status first and then asks the job to end, retaining the job handle in between. Limits: a successful request
   is not a death (nothing waits for one); a failed request is returned by `kill`, `wait` and `fire` (guarantee 3), but
   in a drop it is only logged and the tree is discarded from the registry, because the async child then enters tokio's
   reaper and its group id is free to be reused; a Unix process that left the group (`setsid`, a double fork) is not
   reached (on Linux the guard, guarantee 8, kills it at the daemon's end); on Windows a process started through a
   broker (WMI, COM activation, a scheduled task, a service) is outside the job; a process in uninterruptible sleep
   survives a kill.
2. **The fire is permanent, and a start racing it is never lost.** `fire` is never reset. Once it is published a start
   is refused before anything is created. Creation, adoption and registration of a child hold the registry's mutex
   that `fire` takes (`Signal::reserve`, `Provisional`, `Held::fill`), so a child created while the fire runs is either
   registered and requested, or ended by its provisional owner, on an error or an unwind too. Limit: an OS creation or
   adoption that never returns holds the mutex and so delays `fire`; a caller that cannot wait (the daemon's terminal)
   bounds its own wait for the answer.
3. **A request is checked.** `fire` attempts every registered tree, keeps a tree whose request failed (a later checked
   cleanup retries it), reports every failure, and waits for no death. A group and its unreaped leader are asked
   independently, only ESRCH counts as already gone, and the owner reaps the leader only after the requests succeed; a
   failed request returns before a blocking reap. When a probe and a cleanup both fail, `ContainedStd`'s blocking waits
   (`wait`, `wait_within`) keep both reasons; the async `Contained::wait` returns at its probe's failure and keeps
   that one. On macOS alone a group request refused with EPERM counts as no live member when a libproc listing of the
   group, each member's status record and a second listing find every member ended or in another group and none added
   (`contain::macos::checked_no_live_group`); a live, failed or ambiguous observation stays an error that carries its
   evidence.
4. **Every wait is bounded on the OS clock, and observing is not owning.** `ContainedStd::wait_within(bound)` returns
   `Ok(None)` at the bound after asking the tree to end and reaping the leader; `Signal::output(cmd, within)` turns that
   into an error saying `did not exit within <bound>`, and collects the two pipes until the same deadline (with a
   250 ms floor for the end of a leader that ended at the bound): a process that left the tree and keeps a pipe open
   makes it return a `TimedOut` error naming that holder, and its reader thread ends with the holder.
   `Contained::wait_until_exited` only observes the leader's exit (Unix `WNOWAIT`); it keeps the child's identity and
   containment, cancelling it loses nothing, and a later `wait` or `kill` takes the tree. Limits: at the bound
   `Ok(None)` confirms successful requests and the reap, not descendant death; an OS termination or reap has no
   wall-clock ceiling; a signal that interrupts an identity wait is no answer (the caller asks again); `wait` and
   `kill` themselves have no bound (only `wait_within` and `output` do).
5. **A Unix child keeps no descriptor past its exec but its stdio, as far as the sweep reaches.** `contain::prepare`'s
   `pre_exec` marks descriptors above 2 close-on-exec through `sot_platform::host::inherit_none`: on Linux one
   `close_range(3, ~0, CLOEXEC)`; where that call is missing (macOS, an old kernel) a loop over `3..getdtablesize()` that
   ignores a flag error, so a descriptor numbered above a limit lowered after it was opened is not marked. That sweep is
   the belt; the braces are platform's birth rules (a descriptor made through `sot_platform::host::fd` is born
   close-on-exec, on macOS under the birth lock that `Signal::spawn` and `spawn_std` take around their fork). On Windows
   nothing here closes a handle: a child holds the stdio its owner hands it and what std and the workspace's own
   constructors leave inheritable, which is nothing (the clippy handle groups hold the constructors); a handle a process
   inherited beyond its three standard ones, or one a dependency makes, is not reached (stable std has no handle list at
   spawn).
6. **The daemon's child signal cannot be hidden from it.** `reset_child_signal` makes `SIGCHLD` default and unblocked in
   the calling (main) thread before anything else runs, so an ignored or blocked `SIGCHLD` inherited from the parent can
   neither make the kernel reap a contained leader early nor keep `Contained::wait` from seeing its exit. The caller
   must be the thread that lives as long as the daemon (the main thread). Unix only.
7. **An interrupt goes to the leader's own signal or console group.** `Contained::interrupt` sends `SIGINT` to the
   unreaped leader alone (Unix); a reaped child is not asked. On Windows it sends a Ctrl+Break to the leader's console
   process group (`CREATE_NEW_PROCESS_GROUP`, set at `prepare`), which reaches the leader and the processes in that
   group; a child that shares the daemon's console (one that asks for no console of its own) is reached through it, so
   the event can also reach processes outside the group that share that console. Windows isolation of the event has no
   test.
8. **On Linux the guard kills the daemon's children until it has none, within `DRAIN_BOUND`.** Every serving daemon is
   the child of a guard (`guard::install`) that is a subreaper, forked before any thread exists
   (`guard::require_one_thread` refuses a second thread), mirrors the daemon's end, and then kills its own children
   (every orphan of the subtree comes to it) until it has none. The drain is bounded, not the death: a `SIGKILL` the guard
   cannot deliver (a descendant of other credentials) is ignored, a process in uninterruptible sleep outlasts the bound,
   and at the bound the guard logs each child that remains ("did not end within ...; it has SIGKILL pending") and exits.
   Also outside it: a process a broker started, the guard itself killed, and a capsule (its durable parent is born before
   the guard). On macOS only controlled ends end children (the daemon's terminal fires first); Windows has the job.
9. **Starts are held by a clippy group, with named exceptions.** `rust/clippy.toml`'s process-spawns group lists the ways
   std and tokio start a process, portable-pty's `spawn_command`, libc's `fork`, `vfork`, `posix_spawn`, `posix_spawnp`,
   `execv`, `execve`, `execvp` and `system`, windows-sys's `CreateProcess{W,A,AsUserW,AsUserA}` and the topology's
   `LinkGate` starts; every call outside `Signal::spawn`, `spawn_std` and `output` carries an
   `#[allow(clippy::disallowed_methods, reason = "...")]` that says why it stands, and rust.yml's "Disallowed methods"
   step fails any other. The starts outside `Signal` that stand today: the durable parent (`rows/spawn/durable/proxy.rs`),
   the Windows capsule supervisors (`rows/spawn/detach.rs`), the installer's steps (`install/cmd.rs`), the capsule's
   ConPTY (`rust/log/src/capsule/producer/conpty/mod.rs`), the topology's ssh (`rust/topology/src/
   ssh_bridge.rs`), the window's updater and browser opener (`rust/frontend/src/selfupdate.rs`, `browser_open.rs`) and
   its drawer shell (`ui/drawer/terminal/pty.rs`), and the native launcher's fork in C
   (`rust/platform/src/host/process_tree/native_birth.c`), which no Rust lint sees. Not held either: libc's and
   windows-sys's other start functions (`popen`, `CreateProcessWithLogonW`, `ShellExecute*`, and the like, called nowhere
   today), a start inside a dependency (winresource's compile in a build script), a raw `syscall`, and the lane client's
   ssh through sot-log's `Endpoint` trait. The updater starts nothing: its caller's `Spawner` does.

## Owns
The process-wide `Signal` and its registry of trees with the mutex held across creation and registration; `Contained`
and `ContainedStd`; the provisional owner of a created child; `contain.rs` (the group or job, `prepare`'s hook, adoption,
the checked request, `interrupt`, the macOS group evidence, Windows' suspended-start resume); `reset_child_signal`;
the Linux guard (`guard.rs`: `require_one_thread`, `install`, `guard_pid`, `DRAIN_BOUND`); the process-spawns group of
`rust/clippy.toml` and what its allows say.

## Calls in
`sot_platform::host::inherit_none` and `host::fd::fork_guard` (the sweep and the macOS birth lock),
`sot_platform::host::winjob::AnonymousJob` (the Windows job), libc (killpg, waitid, pidfd, prctl, kqueue and libproc
on macOS), windows-sys (jobs, console events, toolhelp for resuming a suspended start), tokio (`process`, `signal`,
`sync::watch`) and tracing.

## Calls out
`sot_containment::{Signal, process, Contained, ContainedStd, reset_child_signal}` and
`sot_containment::guard::{require_one_thread, install, guard_pid, DRAIN_BOUND}` (Linux). `Signal::claim_exit` serves
lifecycle's terminal (one exit takes the claim) and has no other reader; it is lifecycle's state kept on the signal (the
terminal's tests use private signals, so a lifecycle-owned claim would need them redone), listed in the result of pass
5b-1 for the lifecycle follow-up. Behind the `test-support` feature the crate also exposes the fault hooks and observers
its tests and the backend's use (`leftover_tests.rs`, `watched_tests.rs`, and the `after_create`, `after_adopt`,
`held_groups` and `contain::*` cells); an installed build has none of them.

## Does not
Decide when anything ends (lifecycle's terminal fires it; each owner releases its own child). Wait for a death after a
request. Know a row, a lease, a lane or an op. Contain a capsule (it outlives the daemon by design). Make descriptors
(platform) or find the Julia and ssh programs it starts (their owners). Hold a connection.

## Platforms
Linux: process groups, `PR_SET_CHILD_SUBREAPER` guard, pidfd, `WNOWAIT` observation. macOS: process groups, the
birth lock around every fork, EPERM evidence through libproc and kqueue; no guard, so only controlled ends end
children. Windows: an anonymous job per child (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), a console process group per
child, a suspended start resumed after the job assignment (the interval between creation and assignment is observed as
a stated limit), a killed daemon's jobs end with it.

## Tests
One per guarantee. Paths are under `rust/containment/src/`, except where a path starts `backend/`: those are the
daemon-lifetime harness's, which needs the `sotd` binary and stays in `rust/backend/tests/daemon_lifetime/`. A test of
a Windows or macOS arm runs on the hosted runner for its OS (`start_tests_windows.rs`, `start_tests_macos.rs`,
`contain_tests.rs`).

| # | guarantee | test |
|---|---|---|
| 1 | tree asked to end at release or fire | `signal_tests.rs`: `fire_kills_a_tree_whose_owner_is_blocked`, `wait_kills_the_tree_at_the_leaders_exit`, `a_dropped_child_takes_its_tree`, `a_one_shot_takes_its_tree_after_it_exits`; Windows: `contain_tests.rs` `fire_kills_a_windows_tree`, `a_dropped_std_child_takes_its_windows_tree`, `a_dropped_async_child_takes_its_windows_tree`, `contain_job_permits_no_breakaway`; `start_tests_windows.rs` `windows_job_daemon_death` |
| 2 | permanent fire, no lost start | `signal_tests.rs`: `a_start_after_the_fire_creates_nothing`; `start_tests.rs`: `ready_tree_before_registration_cannot_be_outwaited_std` and `_async`, `suspended_start_cannot_be_outwaited_std` and `_async`, `a_post_create_unwind_cleans_the_partial_std_tree` and `_async`, `spawn_errors_release_the_start_lock` |
| 3 | checked request | `start_tests.rs`: `fire_checks_group_and_leader_request_failures`, `wait_within_checks_request_failures`, `wait_within_checks_reap_failures`, `signal_tests.rs` `a_leader_that_left_its_group_dies_at_the_fire` (the leader is asked on its own), `probe_and_cleanup_failures_preserve_both_reasons`; Windows `fire_checks_job_request_failures`; macOS `start_tests_macos.rs` (the EPERM cases) |
| 4 | bounded waits, observing is not owning | `signal_tests.rs`: `wait_within_returns_a_prompt_exit`, `wait_within_ends_a_child_past_its_bound`, `exit_observation_retains_owner_and_descendant`, `a_reaped_child_answers_from_its_reap`; `signal_output_bound_tests.rs`: `output_ends_at_its_bound`, `output_does_not_wait_for_an_escaped_pipe_holder`; `start_tests.rs`: `a_signal_during_the_wait_is_no_answer` |
| 5 | no inherited descriptor (the sweep's reach) | `start_tests.rs`: `a_child_keeps_no_descriptor_its_start_caught_before_close_on_exec` |
| 6 | SIGCHLD | `signal_tests.rs`: `an_ignored_sigchld_is_reset_at_startup`, `a_blocked_sigchld_is_unblocked_at_startup` |
| 7 | interrupt | `signal_tests.rs`: `an_interrupt_reaches_the_leader_alone`, `a_reaped_child_is_not_interrupted` (Unix only; the Windows console group has none) |
| 8 | Linux guard (forwarding, mirroring and the premises; the drain's bound and an undeliverable kill have no test of their own) | `backend/tests/daemon_lifetime/guard.rs`: `the_guard_mirrors_the_daemon_and_forwards_signals`, `a_stop_of_the_guard_stops_the_daemon_with_it`, `losing_the_guard_ends_the_daemon_at_once`, `the_prologue_refuses_a_second_thread`; `backend/tests/contained_job.rs` `a_killed_test_job_leaves_no_row` |
| 9 | starts held by the group | rust.yml's "Disallowed methods" step (`cargo clippy --workspace ... -D clippy::disallowed_methods`), on all three systems at the release gate and on Linux at each landing; the allows' reasons are the record |

The observers the tests use fail closed: `Leftover` reads only `ESRCH` at `pidfd_open` as a process already gone and fails on any other refusal (`signal_tests.rs`: `a_leftover_that_cannot_be_observed_fails_and_one_that_is_gone_is_seen_gone`).
