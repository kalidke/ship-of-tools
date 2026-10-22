# rust/containment: containment (contract)

Every child the daemon starts lives in a kill domain its owner ends, and every wait on one is bounded. The crate
(`sot-containment`) depends on `sot-platform` and nothing else of ours; `sotd` (the backend binary) depends on it and
nothing re-exports it. It holds one piece of run-time state, the registry of contained trees (inside `Signal`, one per
process: `process()`), and it decides nothing about when to end anything: lifecycle fires the signal, and each owner
releases its own child.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on and the limit it has. The Tests section holds the test
of each.

1. **A contained child dies with its tree when its owner lets go or the signal fires.** A child started through
   `Signal::spawn`, `Signal::spawn_std` or `Signal::output` leads a kill domain of its own: a process group (Unix) or a
   job (Windows, no breakaway, with a console process group of its own). Its owner's `kill`, `wait` or drop of the
   `Contained` or `ContainedStd`, and `Signal::fire`, each request the end of everything still in the domain; the leader
   is reaped only after the requests have succeeded. Limits: a successful request is not a death (it does not wait for
   one); a Unix process that left the group (`setsid`, a double fork) is not reached (on Linux the guard, guarantee 8,
   ends it at the daemon's end); on Windows a process started through a broker (WMI, COM activation, a scheduled task, a
   service) is outside the job; a process in uninterruptible sleep survives a kill.
2. **The fire is permanent, and a start racing it is never lost.** `fire` is never reset. Once it is published a start
   is refused before anything is created. Creation, adoption and registration of a child hold the registry's mutex
   that `fire` takes (`Signal::reserve`, `Provisional`, `Held::fill`), so a child created while the fire runs is either
   registered and requested, or ended by its provisional owner, on an error or an unwind too. Limit: an OS creation or
   adoption that never returns holds the mutex and so delays `fire`; a caller that cannot wait (the daemon's terminal)
   bounds its own wait for the answer.
3. **A request is checked.** `fire` attempts every registered tree, keeps a tree whose request failed (a later
   checked cleanup retries it), reports every failure, and waits for no death. A group and its unreaped leader are
   asked independently, only ESRCH counts as already gone, and the owner reaps the leader only after the requests
   succeed; a failed request returns before a blocking reap, and when a probe and a cleanup both fail both reasons are
   kept. On macOS alone a group request refused with EPERM counts as no live member when a libproc listing of the group,
   each member's status record and a second listing find every member ended or in another group and none added
   (`contain::macos::checked_no_live_group`); a live, failed or ambiguous observation stays an error that carries its
   evidence.
4. **Every wait is bounded on the OS clock, and observing is not owning.** `ContainedStd::wait_within` and
   `Signal::output(cmd, within)` end at their bound: the tree is asked to end, the leader is reaped, and the error says
   `did not exit within <bound>`. `Contained::wait_until_exited` only observes the leader's exit (Unix `WNOWAIT`); it
   keeps the child's identity and containment, cancelling it loses nothing, and a later `wait` or `kill` takes the tree.
   Limits: at the bound `Ok(None)` confirms successful requests and the reap, not descendant death; an OS termination
   or reap has no wall-clock ceiling; a signal that interrupts an identity wait is no answer (the caller asks again).
5. **A Unix child keeps no descriptor past its exec but its stdio.** `contain::prepare`'s `pre_exec` marks every
   descriptor above 2 close-on-exec through `sot_platform::host::inherit_none`, whatever flag it had at its start. That
   sweep is the belt; the braces are platform's birth rules (a descriptor made through `sot_platform::host::fd` is
   born close-on-exec, on macOS under the birth lock that `Signal::spawn` and `spawn_std` take around their fork).
   On Windows nothing here closes a handle: a child holds the stdio its owner hands it and what std and the workspace's
   own constructors leave inheritable, which is nothing (the clippy handle groups hold the constructors); a handle a
   process inherited beyond its three standard ones, or one a dependency makes, is not reached (stable std has no handle
   list at spawn).
6. **The daemon's child signal cannot be hidden from it.** `reset_child_signal` makes `SIGCHLD` default and unblocked in
   the calling (main) thread before anything else runs, so an ignored or blocked `SIGCHLD` inherited from the parent can
   neither make the kernel reap a contained leader early nor keep `Contained::wait` from seeing its exit. The caller
   must be the thread that lives as long as the daemon (the main thread). Unix only.
7. **An interrupt reaches a contained child's own tree and nothing wider.** `Contained::interrupt` sends `SIGINT` to the
   unreaped leader alone (Unix) or a Ctrl+Break to its console process group (Windows, `CREATE_NEW_PROCESS_GROUP`, set
   at `prepare`); a reaped child is not asked. On Windows the event reaches a process only while the daemon shares its
   console, so a child that asks for no console of its own is not reached.
8. **On Linux a process the daemon starts, at any depth, ends within `DRAIN_BOUND` of the daemon's end.** Every serving
   daemon is the child of a guard (`guard::install`) that is a subreaper, forked before any thread exists
   (`guard::require_one_thread` refuses a second thread), mirrors the daemon's end and kills its own children until it
   has none. Limits: a process a broker started is not its descendant, the guard itself killed ends nothing, a kernel call
   in uninterruptible sleep outlasts the bound, and a capsule is outside it by design (its durable parent is born before
   the guard). On macOS only controlled ends end children (the daemon's terminal fires first); Windows has the job.
9. **A process is started only through `Signal`.** `rust/clippy.toml`'s process-spawns group lists every way std and
   tokio start a process, portable-pty's `spawn_command`, libc's `fork`, `vfork`, `posix_spawn`, `posix_spawnp`,
   `execv`, `execve`, `execvp` and `system`, windows-sys's `CreateProcess{W,A,AsUserW,AsUserA}` and the topology's
   `LinkGate` starts; every call outside `Signal::spawn`, `spawn_std` and `output` carries an
   `#[allow(clippy::disallowed_methods, reason = "...")]` that says why it stands, and rust.yml's "Disallowed methods"
   step fails any other. Not held: libc's and windows-sys's other start functions (`popen`, `CreateProcessWithLogonW`,
   `ShellExecute*`, and the like, called nowhere today), a start inside a dependency (winresource's compile in a build
   script), a raw `syscall`, sot-log's own starts reached through its public items, and the lane client's ssh through
   sot-log's `Endpoint` trait (only the window builds one). The updater starts nothing: its caller's `Spawner` does.

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
lifecycle's terminal (one exit takes the claim) and has no other reader. Under the `test-support` feature the crate
also exposes its fault hooks and the test observers lifecycle, files, pages, sidecars and the updater's tests use
(`Signal::after_create`, `after_adopt`, `held_groups`, `contain::REQUEST_EVENTS`, `PROBE_FAILURE`, `REQUEST_FAILURE`,
`REAP_FAILURE`, `tests::Leftover`, `start_tests::Watched`); an installed build has none of them.

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
| 1 | tree dies with owner or fire | `signal_tests.rs`: `fire_kills_a_tree_whose_owner_is_blocked`, `wait_kills_the_tree_at_the_leaders_exit`, `a_dropped_child_takes_its_tree`, `a_one_shot_takes_its_tree_after_it_exits`; Windows: `contain_tests.rs` `fire_kills_a_windows_tree`, `a_dropped_std_child_takes_its_windows_tree`, `a_dropped_async_child_takes_its_windows_tree`, `contain_job_permits_no_breakaway`; `start_tests_windows.rs` `windows_job_daemon_death` |
| 2 | permanent fire, no lost start | `signal_tests.rs`: `a_start_after_the_fire_creates_nothing`; `start_tests.rs`: `ready_tree_before_registration_cannot_be_outwaited_std` and `_async`, `suspended_start_cannot_be_outwaited_std` and `_async`, `a_post_create_unwind_cleans_the_partial_std_tree` and `_async`, `spawn_errors_release_the_start_lock` |
| 3 | checked request | `start_tests.rs`: `fire_checks_group_and_leader_request_failures`, `wait_within_checks_request_failures`, `wait_within_checks_reap_failures`, `signal_tests.rs` `a_leader_that_left_its_group_dies_at_the_fire` (the leader is asked on its own), `probe_and_cleanup_failures_preserve_both_reasons`; Windows `fire_checks_job_request_failures`; macOS `start_tests_macos.rs` (the EPERM cases) |
| 4 | bounded waits, observing is not owning | `signal_tests.rs`: `wait_within_returns_a_prompt_exit`, `wait_within_ends_a_child_past_its_bound`, `exit_observation_retains_owner_and_descendant`, `a_reaped_child_answers_from_its_reap`; `signal_output_bound_tests.rs`: `output_ends_at_its_bound`; `start_tests.rs`: `a_signal_during_the_wait_is_no_answer` |
| 5 | no inherited descriptor | `start_tests.rs`: `a_child_keeps_no_descriptor_its_start_caught_before_close_on_exec` |
| 6 | SIGCHLD | `signal_tests.rs`: `an_ignored_sigchld_is_reset_at_startup`, `a_blocked_sigchld_is_unblocked_at_startup` |
| 7 | interrupt | `signal_tests.rs`: `an_interrupt_reaches_the_leader_alone`, `a_reaped_child_is_not_interrupted` |
| 8 | Linux guard | `backend/tests/daemon_lifetime/guard.rs`: `the_guard_mirrors_the_daemon_and_forwards_signals`, `a_stop_of_the_guard_stops_the_daemon_with_it`, `losing_the_guard_ends_the_daemon_at_once`, `the_prologue_refuses_a_second_thread`; `backend/tests/contained_job.rs` `a_killed_test_job_leaves_no_row` |
| 9 | starts only through `Signal` | rust.yml's "Disallowed methods" step (`cargo clippy --workspace ... -D clippy::disallowed_methods`), on all three systems at the release gate and on Linux at each landing; the allows' reasons are the record |
