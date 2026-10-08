# rust/backend/src/rows/spawn/durable: the capsule-only birth parent (rows)

A capsule's supervisor is forked by a process that is not the daemon, after the row's authority fence is claimed. The
daemon starts one parent (`sotd durable-parent`, over a private socketpair that is the parent's standard input) the first
time a capsule is launched and again if it was lost; the parent lives in a session of its own. For each launch the
parent creates the row's state dir, takes the row's `supervisor.lock` (`BirthClaim`), forks the supervisor with the
native launcher held at its gate, and tells the daemon it is born. The daemon publishes the birth (the run-gate permit
and the row's guard it already holds) and asks the parent to release it. The supervisor inherits the claim's descriptor,
adopts it as its own authority fence, and answers the parent on a private channel; only then does the parent close its
copy, so the fence is never free between the claim and the supervisor's first act. Part of the daemon's rows subsystem;
charter: `rust/backend/src/rows/CLAUDE.md`. Unix only: Windows creates the supervisor directly and the supervisor takes
the fence at its first act.

## Files
- `mod.rs`: `Spec` (one supervisor launch), `launch` and `Launched` (started, or contended)
- `wire.rs`: `Channel` (length-prefixed JSON over a socketpair, descriptors by `SCM_RIGHTS`) and the `Request` and `Reply` messages
- `proxy.rs`: the daemon's end: `Client` starts the parent, routes its replies, `DurableChild` waits for the supervisor's exit
- `parent.rs`: the parent's loop (`run`): accepted launches, the takeover channels, the exit reports, the daemon's loss
- `accept.rs`: one launch's acceptance (`accept`): validate, create the state dir, claim the fence, fork gated

## Start here
`accept.rs::accept` for what a launch is before the daemon hears of it, then `parent.rs::Parent::watch` for what ends
the parent's hold on a claim.

## Rules
- A launch is accepted only with the claim in hand: `accept` forks nothing before `BirthClaim::take` succeeds, and a
  held fence is `Refusal::Contended`, never an error and never a retry (`Parent::launch`).
- The parent keeps the physical gate writer and the kernel parent authority of an accepted supervisor until it has
  taken the claim over; the daemon's death never closes the gate. A birth the daemon had not yet released is released
  by the parent (`Parent::daemon_gone`), under the claim it holds: it is not cancelled and not replayed.
- The parent lets go of its copy of the claim only on a takeover record from the supervisor it forked (`watch`: the pid
  must be the child's); a channel that closes with nothing on it, or from another pid, keeps the claim until the child
  ends.
- The new child holds no descriptor of the parent's: `accept` names the channel, every other birth's gate and claim and
  every takeover reader to the launcher's close list.
- A supervisor that took its claim over outlives the parent as an ordinary orphan (`Parent::serve` abandons it); a
  parent exits when the daemon is gone and no accepted birth is waiting for its takeover.
- The supervisor's exit is reported to the daemon only while it lives; `DurableChild::wait` errs when the parent is
  lost first, which the watchdog treats as a crash and rechecks (`watchdog_may_act`).
