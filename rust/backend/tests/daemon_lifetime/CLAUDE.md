# rust/backend/tests/daemon_lifetime: the daemon-lifetime harness (tests)

The premises and, from the next commits, the cases of lane L2 (a daemon lifetime and the children it owns), run on
real processes: a real `sotd` and `sot-capsule`, a real launcher, a real fence. A case saves what the product did
before it cleans anything, then ends only the processes it holds an identity over, and asserts the saved result. Part
of the backend's tests; the suites span subsystems, so this page names no charter.

## Files
- `main.rs`: the module list and how to build the barrier variants the held-process cases need
- `done.rs`: the done test's parts: its inputs (a case-private copy of Pluto's folder, the trees, the environment), the trees it starts through each product route and the oracle it reads after the daemon is killed (feature `daemon-lifetime-faults`)
- `durable.rs`: the durable parent's place in a real daemon's process tree: it descends from nothing the daemon started (feature `daemon-lifetime-faults`)
- `guard.rs`: the Linux lifetime guard on real daemons: it mirrors the daemon's end and forwards signals, a lost guard ends the daemon, the drain outlasts a forking child and ends only its own subtree (these two need Julia), the relay refresh follows the guard, the prologue refuses a second thread (feature `daemon-lifetime-faults`); and `Run`, the guarded daemon the cases start
- `fixture_owner.rs`: `Fixture`, the outside owner: authority over every process a case starts, saved results, bounded cleanup
- `observations.rs`: `wait_for`, and `BarrierDir`, the folder a process held at a phase barrier reports to
- `native/`: the per-OS process authority (an identity opened while alive; death read from it)
- `native_premises.rs`: the native launcher and the fence claim on real children (gate, owning return, errors, claim lifetime, source group)
- `routes.rs`: the daemon's real routes the cases drive and what they read back: a ready row, a spinning REPL cell, a tree's identities, the nonce round trip, a process's children and command line
- `successor.rs`: a successor daemon and a capsule birth held at each of five phases of its way to its first act: the successor starts no second supervisor and reaches the original (feature `daemon-lifetime-faults`; needs the barrier build of `sot-capsule`)
- `workers.rs`: the product worker factories read where they stand: a Julia `Distributed` worker against the process that started it, and the done test of the guard, `every_descendant_of_a_killed_daemon_ends`: a killed daemon's REPL, Pluto and Quarto trees all end, the capsule stays and a successor adopts it (ignored in a plain run; needs Julia 1.12, Pluto's environment and, for its Quarto half, Quarto 1.7.31)
- `fixtures/`: the real inputs and process trees of the cases (see its page)

## Start here
`native_premises.rs`, the first test, for the shape of a case: begin a child, take authority over it, observe, clean up.

## Rules
- No case reads source text as proof: a premise is a real launch, lock or daemon, and a death is read from an
  identity opened while the process was alive, never from a number or a name.
- A case saves what the product did before `Fixture::cleanup`, ends only its recorded identities within
  `CLEANUP_RESERVE`, and asserts the saved result after; cleanup never calls product code.
- A case that holds a real process at a phase barrier needs the barrier build: `sot-capsule` built with
  `--features native-barrier` into the target the tests run from, and `--features daemon-lifetime-faults` on the test
  run. An installed build has neither feature.
- One case runs at a time in the binary (`Fixture::new` takes the lock): a gated child holds a copy of every descriptor its
  parent had open until it execs or ends, so a case that reads a pipe to EOF would wait on another case's child.
- A role a case re-runs this binary for (`claim_parent_role`, `source_group_role`) enters through
  `sot_log::test_isolated::test_command` and `enter`, and does nothing in an ordinary run.
- On macOS the authority is not yet established (`native/macos.rs`): every identity request fails with that cause, so a
  case fails at its first step and never passes by skipping.
- A process a case ends is one it started, one a fixture process it started reported, one the product reported over its
  authenticated lane, the daemon's own pid from `SO_PEERCRED` on the case's control connection, or one in the children
  list of a process it holds. A process reached by walking parent links upward is looked at through a pidfd
  (`Fixture::observe`) and is never signalled or recorded for cleanup.
- The Quarto half runs where `quarto --version` answers and is printed as not checked, never passed, where it does not;
  its transport files are under a case-private `XDG_RUNTIME_DIR` that only the case's `quarto` wrapper sets.
